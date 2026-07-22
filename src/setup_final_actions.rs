use serde::Serialize;
use serde_json::{Map as JsonMap, Value};
use url::Url;

pub const SETUP_ACTIONS_EXTENSION: &str = "greentic.setup.actions.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResolvedFinalSetupAction {
    pub provider_id: String,
    pub action_id: String,
    pub label: String,
    pub kind: String,
    pub url: String,
    pub opens_new_window: bool,
    pub copyable: bool,
    pub html: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FinalSetupActionDiagnostic {
    pub provider_id: String,
    pub action_id: Option<String>,
    pub reason: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FinalSetupActionResolution {
    pub actions: Vec<ResolvedFinalSetupAction>,
    pub diagnostics: Vec<FinalSetupActionDiagnostic>,
}

pub fn resolve_final_setup_actions(
    provider_id: &str,
    descriptor: &Value,
    public_state: &Value,
) -> FinalSetupActionResolution {
    let mut resolution = FinalSetupActionResolution::default();
    if descriptor.get("schema_id").and_then(Value::as_str) != Some(SETUP_ACTIONS_EXTENSION) {
        resolution.diagnostics.push(diagnostic(
            provider_id,
            None,
            "invalid_schema",
            "setup action descriptor must use greentic.setup.actions.v1",
        ));
        return resolution;
    }
    if descriptor.get("provider_id").and_then(Value::as_str) != Some(provider_id) {
        resolution.diagnostics.push(diagnostic(
            provider_id,
            None,
            "provider_mismatch",
            "setup action descriptor provider_id does not match setup target",
        ));
        return resolution;
    }
    let Some(actions) = descriptor.get("actions").and_then(Value::as_array) else {
        resolution.diagnostics.push(diagnostic(
            provider_id,
            None,
            "invalid_actions",
            "setup action descriptor actions must be an array",
        ));
        return resolution;
    };

    let context = build_public_context(public_state);
    for action in actions {
        match resolve_action(provider_id, action, &context) {
            Ok(Some(action)) => resolution.actions.push(action),
            Ok(None) => {}
            Err(err) => resolution.diagnostics.push(err),
        }
    }
    resolution
}

fn resolve_action(
    provider_id: &str,
    action: &Value,
    context: &Value,
) -> Result<Option<ResolvedFinalSetupAction>, FinalSetupActionDiagnostic> {
    let action_id = action
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let action_id_opt = (!action_id.is_empty()).then(|| action_id.to_string());
    let label = required_str(provider_id, action_id_opt.as_deref(), action, "label")?;
    let kind = required_str(provider_id, action_id_opt.as_deref(), action, "kind")?;
    let template = required_str(
        provider_id,
        action_id_opt.as_deref(),
        action,
        "url_template",
    )?;
    if action_id.is_empty() {
        return Err(diagnostic(
            provider_id,
            None,
            "missing_id",
            "setup action id is required",
        ));
    }
    if kind != "deep_link" {
        return Err(diagnostic(
            provider_id,
            Some(action_id),
            "unsupported_kind",
            "only deep_link setup actions are supported",
        ));
    }
    if !visible_when_matches(action.get("visible_when"), context) {
        return Ok(None);
    }
    for name in action
        .get("requires")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if is_secret_key(name)
            || value_at_path(context, name)
                .and_then(public_scalar)
                .is_none()
        {
            return Err(diagnostic(
                provider_id,
                Some(action_id),
                "missing_required_value",
                &format!("required value {name} is not available as a public value"),
            ));
        }
    }
    let Some(url) = resolve_template(&template, context) else {
        return Err(diagnostic(
            provider_id,
            Some(action_id),
            "unresolved_template",
            "url_template contains an unresolved placeholder",
        ));
    };
    if !safe_action_url(&url) {
        return Err(diagnostic(
            provider_id,
            Some(action_id),
            "invalid_url_scheme",
            "resolved setup action URL must use https or a supported app deep-link scheme",
        ));
    }
    let opens_new_window = action
        .get("opens_new_window")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let copyable = action
        .get("copyable")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    Ok(Some(ResolvedFinalSetupAction {
        provider_id: provider_id.to_string(),
        action_id: action_id.to_string(),
        label: label.clone(),
        kind: kind.clone(),
        html: action_html(&label, &url),
        url,
        opens_new_window,
        copyable,
    }))
}

fn required_str(
    provider_id: &str,
    action_id: Option<&str>,
    action: &Value,
    field: &str,
) -> Result<String, FinalSetupActionDiagnostic> {
    action
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            diagnostic(
                provider_id,
                action_id,
                "missing_field",
                &format!("setup action {field} is required"),
            )
        })
}

fn build_public_context(public_state: &Value) -> Value {
    let mut root = JsonMap::new();
    merge_public_values(&mut root, public_state);
    if let Some(values) = public_state.get("values") {
        root.insert("values".to_string(), values.clone());
        merge_public_values(&mut root, values);
    }
    if let Some(status) = public_state.get("setup_status") {
        root.insert("setup_status".to_string(), status.clone());
    }
    Value::Object(root)
}

fn merge_public_values(target: &mut JsonMap<String, Value>, source: &Value) {
    let Some(source) = source.as_object() else {
        return;
    };
    for (key, value) in source {
        if is_secret_key(key) || value.is_null() {
            continue;
        }
        match value {
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                target.insert(key.clone(), value.clone());
            }
            Value::Object(_) => merge_public_values(target, value),
            _ => {}
        }
    }
}

fn visible_when_matches(visible_when: Option<&Value>, context: &Value) -> bool {
    let Some(conditions) = visible_when.and_then(Value::as_object) else {
        return true;
    };
    conditions.iter().all(|(path, expected)| {
        value_at_path(context, path).is_some_and(|actual| actual == expected)
    })
}

fn resolve_template(template: &str, context: &Value) -> Option<String> {
    if let Some(name) = whole_placeholder(template) {
        return value_at_path(context, name).and_then(public_scalar);
    }
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let (prefix, after_start) = rest.split_at(start);
        output.push_str(prefix);
        let after_start = &after_start[1..];
        let end = after_start.find('}')?;
        let (name, after_end) = after_start.split_at(end);
        let value = value_at_path(context, name).and_then(public_scalar)?;
        output.push_str(&url_encode(&value));
        rest = &after_end[1..];
    }
    output.push_str(rest);
    Some(output)
}

fn whole_placeholder(template: &str) -> Option<&str> {
    let name = template.strip_prefix('{')?.strip_suffix('}')?;
    (!name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-'))
    .then_some(name)
}

fn value_at_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.split('.').any(is_secret_key) {
        return None;
    }
    path.split('.')
        .try_fold(value, |current, segment| current.as_object()?.get(segment))
}

fn public_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Schemes a resolved `deep_link` action URL may use. `https` is the default
/// for hosted share links; the native-app deep-link schemes let a provider hand
/// the user straight into its desktop/mobile client (e.g. Webex's
/// `webexteams://im?email=…`) instead of a browser round-trip. Must stay in sync
/// with `isSafeFinalSetupActionUrl` in `assets/setup-ui/app.js`.
const SAFE_ACTION_URL_SCHEMES: &[&str] = &["https", "webexteams"];

fn safe_action_url(url: &str) -> bool {
    Url::parse(url)
        .map(|parsed| SAFE_ACTION_URL_SCHEMES.contains(&parsed.scheme()))
        .unwrap_or(false)
}

/// Build the copy-paste anchor for a final setup action. Deliberately
/// class-free: the snippet is pasted into arbitrary external pages, so it must
/// not depend on Greentic stylesheets. Same shape for every provider.
fn action_html(label: &str, url: &str) -> String {
    format!(
        r#"<a href="{}" target="_blank" rel="noopener noreferrer">{}</a>"#,
        html_escape_attr(url),
        html_escape_text(label)
    )
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn html_escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html_escape_attr(value: &str) -> String {
    html_escape_text(value)
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub(crate) fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "token"
        || key == "secret"
        || key == "password"
        || key.ends_with("_token")
        || key.ends_with("_secret")
        || key.contains("access_token")
        || key.contains("refresh_token")
        || key.contains("id_token")
        || key.contains("device_code")
        || key.contains("bot_access_token")
}

fn diagnostic(
    provider_id: &str,
    action_id: Option<&str>,
    reason: &str,
    detail: &str,
) -> FinalSetupActionDiagnostic {
    FinalSetupActionDiagnostic {
        provider_id: provider_id.to_string(),
        action_id: action_id.map(ToOwned::to_owned),
        reason: reason.to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn descriptor(action: Value) -> Value {
        json!({
            "schema_id": "greentic.setup.actions.v1",
            "provider_id": "messaging-test",
            "actions": [action]
        })
    }

    #[test]
    fn resolves_whole_url_placeholder_without_encoding_scheme() {
        let resolved = resolve_final_setup_actions(
            "messaging-test",
            &descriptor(json!({
                "id": "add",
                "label": "Add to Test",
                "kind": "deep_link",
                "url_template": "{add_url}",
                "requires": ["add_url"],
                "visible_when": {"setup_status.ok": true}
            })),
            &json!({
                "setup_status": {"ok": true},
                "values": {"add_url": "https://example.test/add?x=1"}
            }),
        );

        assert_eq!(resolved.diagnostics, Vec::new());
        assert_eq!(resolved.actions[0].url, "https://example.test/add?x=1");
    }

    #[test]
    fn resolves_template_placeholders_with_url_encoding() {
        let resolved = resolve_final_setup_actions(
            "messaging-test",
            &descriptor(json!({
                "id": "telegram",
                "label": "Add",
                "kind": "deep_link",
                "url_template": "https://t.me/{bot_username}",
                "requires": ["bot_username"]
            })),
            &json!({"values": {"bot_username": "bot name"}}),
        );

        assert_eq!(resolved.actions[0].url, "https://t.me/bot%20name");
    }

    #[test]
    fn resolves_webex_native_app_deep_link_scheme() {
        // Webex's "Add to WebEx" action hands the user straight to the native
        // client via a `webexteams://` deep link. It must survive the scheme
        // allowlist rather than being rejected as an unsafe scheme.
        let resolved = resolve_final_setup_actions(
            "messaging-test",
            &descriptor(json!({
                "id": "add-to-webex",
                "label": "Add to WebEx",
                "kind": "deep_link",
                "url_template": "webexteams://im?email={bot_email}",
                "requires": ["bot_email"]
            })),
            &json!({"values": {"bot_email": "barbara@example.com"}}),
        );

        assert_eq!(
            resolved.actions[0].url,
            "webexteams://im?email=barbara%40example.com"
        );
        assert!(resolved.diagnostics.is_empty());
    }

    #[test]
    fn suppresses_action_when_visible_when_does_not_match() {
        let resolved = resolve_final_setup_actions(
            "messaging-test",
            &descriptor(json!({
                "id": "add",
                "label": "Add",
                "kind": "deep_link",
                "url_template": "{add_url}",
                "requires": ["add_url"],
                "visible_when": {"setup_status.ok": true}
            })),
            &json!({"setup_status": {"ok": false}, "values": {"add_url": "https://example.test"}}),
        );

        assert!(resolved.actions.is_empty());
        assert!(resolved.diagnostics.is_empty());
    }

    #[test]
    fn suppresses_secret_required_values() {
        let resolved = resolve_final_setup_actions(
            "messaging-test",
            &descriptor(json!({
                "id": "bad",
                "label": "Bad",
                "kind": "deep_link",
                "url_template": "https://example.test/{access_token}",
                "requires": ["access_token"]
            })),
            &json!({"values": {"access_token": "secret"}}),
        );

        assert!(resolved.actions.is_empty());
        assert_eq!(resolved.diagnostics[0].reason, "missing_required_value");
    }

    #[test]
    fn rejects_unsafe_url_schemes_and_escapes_html() {
        let rejected = resolve_final_setup_actions(
            "messaging-test",
            &descriptor(json!({
                "id": "bad",
                "label": "Bad",
                "kind": "deep_link",
                "url_template": "javascript:alert(1)",
                "requires": []
            })),
            &json!({}),
        );
        assert!(rejected.actions.is_empty());
        assert_eq!(rejected.diagnostics[0].reason, "invalid_url_scheme");

        let escaped = resolve_final_setup_actions(
            "messaging-test",
            &descriptor(json!({
                "id": "add",
                "label": "Add \"<&>",
                "kind": "deep_link",
                "url_template": "{add_url}",
                "requires": ["add_url"]
            })),
            &json!({"values": {"add_url": "https://example.test/?q=\"<&>"}}),
        );
        assert!(escaped.actions[0].html.contains("&quot;&lt;&amp;&gt;"));
        assert!(escaped.actions[0].html.contains("Add \"&lt;&amp;&gt;"));
    }
}
