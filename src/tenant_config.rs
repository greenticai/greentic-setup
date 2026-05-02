//! Tenant config synchronization for webchat-gui OAuth settings.
//!
//! After setup persists OAuth answers to secrets, this module updates the
//! static tenant config JSON (`assets/webchat-gui/config/tenants/<tenant>.json`)
//! to enable/disable OAuth providers and set client IDs. This ensures the
//! webchat-gui runtime serves the correct auth config without manual editing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::platform_setup::load_effective_static_routes_defaults;

/// Well-known OIDC provider definitions.
struct OidcProviderDef {
    id_suffix: &'static str,
    label: &'static str,
    answer_enable_key: &'static str,
    answer_client_id_key: &'static str,
    authorization_url: &'static str,
    scope: &'static str,
}

const OIDC_PROVIDERS: &[OidcProviderDef] = &[
    OidcProviderDef {
        id_suffix: "google",
        label: "Sign in with Google",
        answer_enable_key: "oauth_enable_google",
        answer_client_id_key: "oauth_google_client_id",
        authorization_url: "https://accounts.google.com/o/oauth2/v2/auth",
        scope: "openid profile email",
    },
    OidcProviderDef {
        id_suffix: "microsoft",
        label: "Sign in with Microsoft",
        answer_enable_key: "oauth_enable_microsoft",
        answer_client_id_key: "oauth_microsoft_client_id",
        authorization_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
        scope: "openid profile email",
    },
    OidcProviderDef {
        id_suffix: "github",
        label: "Sign in with GitHub",
        answer_enable_key: "oauth_enable_github",
        answer_client_id_key: "oauth_github_client_id",
        authorization_url: "https://github.com/login/oauth/authorize",
        scope: "read:user user:email",
    },
];

/// Resolve the target tenant config file, scaffolding `<tenant>.json` from
/// `default.json` when it does not yet exist.
///
/// The webchat-gui SPA's runtime-bootstrap fetches `/config/tenants/<tenant>.json`
/// directly via `originalFetch` (bypassing its own 404-fallback interceptor) when
/// resolving skin / nav_links overrides. If we instead patched `default.json`
/// here, those tenant-specific fields would never be picked up at runtime, *and*
/// the template would be polluted with the wrong `tenant_id`. So when the
/// tenant-specific file is missing, we copy `default.json` to `<tenant>.json`
/// (rewriting `tenant_id`) and write into the new file.
///
/// Returns `Ok(None)` when neither `<tenant>.json` nor `default.json` exists —
/// callers should treat that as a no-op.
fn resolve_or_scaffold_tenant_config(bundle_path: &Path, tenant: &str) -> Result<Option<PathBuf>> {
    let tenants_dir = bundle_path.join("assets/webchat-gui/config/tenants");
    let tenant_path = tenants_dir.join(format!("{tenant}.json"));
    if tenant_path.exists() {
        return Ok(Some(tenant_path));
    }

    let default_path = tenants_dir.join("default.json");
    if !default_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&default_path)
        .with_context(|| format!("read default tenant config {}", default_path.display()))?;
    let mut config: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parse default tenant config {}", default_path.display()))?;
    if let Some(obj) = config.as_object_mut() {
        obj.insert("tenant_id".to_string(), Value::String(tenant.to_string()));
    }
    let output = serde_json::to_string_pretty(&config)?;
    std::fs::write(&tenant_path, output)
        .with_context(|| format!("scaffold tenant config {}", tenant_path.display()))?;
    Ok(Some(tenant_path))
}

/// Synchronize webchat-gui OAuth answers to the tenant config JSON.
///
/// Only runs for `messaging-webchat-gui` providers. Updates the tenant config
/// at `<bundle>/assets/webchat-gui/config/tenants/<tenant>.json`, scaffolding
/// the file from `default.json` when missing.
pub fn sync_oauth_to_tenant_config(
    bundle_path: &Path,
    tenant: &str,
    provider_id: &str,
    answers: &Value,
) -> Result<bool> {
    if !provider_id.contains("webchat-gui") {
        return Ok(false);
    }

    let answers_obj = match answers.as_object() {
        Some(m) => m,
        None => return Ok(false),
    };

    let oauth_enabled = answers_obj
        .get("oauth_enabled")
        .and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
        .unwrap_or(false);

    let Some(target) = resolve_or_scaffold_tenant_config(bundle_path, tenant)? else {
        return Ok(false);
    };

    update_tenant_config(
        &target,
        tenant,
        oauth_enabled,
        answers_obj,
        resolve_public_base_url(bundle_path, tenant, answers_obj)?,
    )
}

fn update_tenant_config(
    config_path: &Path,
    tenant: &str,
    oauth_enabled: bool,
    answers: &Map<String, Value>,
    public_base_url: Option<String>,
) -> Result<bool> {
    let raw = std::fs::read_to_string(config_path)
        .with_context(|| format!("read tenant config {}", config_path.display()))?;

    let mut config: Value = serde_json::from_str(&raw).context("parse tenant config as JSON")?;

    let auth = config.as_object_mut().and_then(|m| {
        m.entry("auth")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
    });

    let Some(auth) = auth else {
        return Ok(false);
    };

    let providers = auth
        .entry("providers")
        .or_insert_with(|| Value::Array(vec![]));

    let Some(providers_arr) = providers.as_array_mut() else {
        return Ok(false);
    };

    let mut changed = false;

    for def in OIDC_PROVIDERS {
        let enabled = oauth_enabled
            && answers
                .get(def.answer_enable_key)
                .and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
                .unwrap_or(false);

        let client_id = answers
            .get(def.answer_client_id_key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let provider_id = format!("{tenant}-{}", def.id_suffix);
        let redirect_uri = public_base_url.as_deref().map(|public_base_url| {
            format!(
                "{}/v1/web/webchat/{}/",
                public_base_url.trim_end_matches('/'),
                tenant,
            )
        });

        // Find existing provider entry — match by exact ID or by suffix (e.g. "google", "microsoft")
        if let Some(existing) = providers_arr.iter_mut().find(|p| {
            let id = p.get("id").and_then(Value::as_str).unwrap_or("");
            id == provider_id || id == def.id_suffix
        }) {
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("enabled".to_string(), Value::Bool(enabled));
                if !client_id.is_empty() {
                    obj.insert("clientId".to_string(), Value::String(client_id));
                }
                if let Some(redirect_uri) = redirect_uri.as_ref() {
                    obj.insert(
                        "redirectUri".to_string(),
                        Value::String(redirect_uri.clone()),
                    );
                }
                changed = true;
            }
        } else if enabled {
            // Add new provider entry
            let mut entry = Map::new();
            entry.insert("id".to_string(), Value::String(provider_id));
            entry.insert("label".to_string(), Value::String(def.label.to_string()));
            entry.insert("type".to_string(), Value::String("oidc".to_string()));
            entry.insert("enabled".to_string(), Value::Bool(true));
            entry.insert(
                "authorizationUrl".to_string(),
                Value::String(def.authorization_url.to_string()),
            );
            if !client_id.is_empty() {
                entry.insert("clientId".to_string(), Value::String(client_id));
            }
            if let Some(redirect_uri) = redirect_uri.as_ref() {
                entry.insert(
                    "redirectUri".to_string(),
                    Value::String(redirect_uri.clone()),
                );
            }
            entry.insert("scope".to_string(), Value::String(def.scope.to_string()));
            entry.insert(
                "responseType".to_string(),
                Value::String("code".to_string()),
            );
            providers_arr.push(Value::Object(entry));
            changed = true;
        }
    }

    // Handle custom OIDC provider
    let custom_enabled = oauth_enabled
        && answers
            .get("oauth_enable_custom")
            .and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
            .unwrap_or(false);

    if custom_enabled {
        let custom_id = format!("{tenant}-custom-oidc");
        let label = answers
            .get("oauth_custom_label")
            .and_then(Value::as_str)
            .unwrap_or("SSO Login")
            .to_string();
        let auth_url = answers
            .get("oauth_custom_auth_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let client_id = answers
            .get("oauth_custom_client_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let scopes = answers
            .get("oauth_custom_scopes")
            .and_then(Value::as_str)
            .unwrap_or("openid profile email")
            .to_string();
        let redirect_uri = public_base_url.as_deref().map(|public_base_url| {
            format!(
                "{}/v1/web/webchat/{}/",
                public_base_url.trim_end_matches('/'),
                tenant,
            )
        });

        if let Some(existing) = providers_arr
            .iter_mut()
            .find(|p| p.get("id").and_then(Value::as_str) == Some(&custom_id))
        {
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("enabled".to_string(), Value::Bool(true));
                obj.insert("label".to_string(), Value::String(label));
                if !auth_url.is_empty() {
                    obj.insert("authorizationUrl".to_string(), Value::String(auth_url));
                }
                if !client_id.is_empty() {
                    obj.insert("clientId".to_string(), Value::String(client_id));
                }
                if let Some(redirect_uri) = redirect_uri.as_ref() {
                    obj.insert(
                        "redirectUri".to_string(),
                        Value::String(redirect_uri.clone()),
                    );
                }
                obj.insert("scope".to_string(), Value::String(scopes));
                changed = true;
            }
        } else {
            let mut entry = Map::new();
            entry.insert("id".to_string(), Value::String(custom_id));
            entry.insert("label".to_string(), Value::String(label));
            entry.insert("type".to_string(), Value::String("oidc".to_string()));
            entry.insert("enabled".to_string(), Value::Bool(true));
            if !auth_url.is_empty() {
                entry.insert("authorizationUrl".to_string(), Value::String(auth_url));
            }
            if !client_id.is_empty() {
                entry.insert("clientId".to_string(), Value::String(client_id));
            }
            if let Some(redirect_uri) = redirect_uri.as_ref() {
                entry.insert(
                    "redirectUri".to_string(),
                    Value::String(redirect_uri.clone()),
                );
            }
            entry.insert("scope".to_string(), Value::String(scopes));
            entry.insert(
                "responseType".to_string(),
                Value::String("code".to_string()),
            );
            providers_arr.push(Value::Object(entry));
            changed = true;
        }
    }

    if changed {
        let output = serde_json::to_string_pretty(&config)?;
        std::fs::write(config_path, output)
            .with_context(|| format!("write tenant config {}", config_path.display()))?;
    }

    Ok(changed)
}

fn resolve_public_base_url(
    bundle_path: &Path,
    tenant: &str,
    answers: &Map<String, Value>,
) -> Result<Option<String>> {
    if let Some(value) = answers
        .get("public_base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !is_placeholder_public_base_url(value))
    {
        return Ok(Some(value.to_string()));
    }

    let from_policy = load_effective_static_routes_defaults(bundle_path, tenant, Some("default"))?
        .and_then(|policy| policy.public_base_url)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .filter(|value| !is_placeholder_public_base_url(value));

    Ok(from_policy)
}

/// Sanitize a `nav_links` array (from either the table-wizard answer or a
/// parsed `nav_links_json` string). Drops malformed entries silently —
/// label or url missing/empty, label not a string or locale-keyed object —
/// and normalises the on-disk shape to `{label, url, external?}`.
fn sanitize_nav_link_array(arr: &[Value]) -> Vec<Value> {
    arr.iter()
        .filter_map(|entry| {
            let obj = entry.as_object()?;
            // `label` accepts either a plain string or a locale-keyed JSON
            // object (e.g. `{"en":"Help","id":"Bantuan"}`).
            let label_value = sanitize_i18n_text(obj.get("label")?)?;

            let url = obj.get("url").and_then(Value::as_str).map(str::trim)?;
            if url.is_empty() {
                return None;
            }

            let mut clean = serde_json::Map::new();
            clean.insert("label".to_string(), label_value);
            clean.insert("url".to_string(), Value::String(url.to_string()));
            if obj.get("external").and_then(Value::as_bool) == Some(true) {
                clean.insert("external".to_string(), Value::Bool(true));
            }
            // Optional `num`: short prefix chip (e.g. "M5"). Same i18n
            // resolution as label.
            if let Some(num) = obj.get("num").and_then(sanitize_i18n_text_opt) {
                clean.insert("num".to_string(), num);
            }
            // Tooltip is collected as three flat columns by the wizard
            // (`tooltip_eyebrow`, `tooltip_title`, `tooltip_lede`); rebuild
            // the nested `tooltip: { eyebrow?, title?, lede? }` object here
            // so the runtime SPA's renderTopbarNav sees the canonical shape.
            // Operators who hand-edit tenant.json can also pass `tooltip` as
            // an already-nested object; if present, we sanitise that path
            // instead of the flat columns.
            let nested_tooltip = obj.get("tooltip").and_then(|v| v.as_object());
            let tooltip_obj = if let Some(map) = nested_tooltip {
                build_tooltip_obj(
                    map.get("eyebrow"),
                    map.get("title"),
                    map.get("lede"),
                )
            } else {
                build_tooltip_obj(
                    obj.get("tooltip_eyebrow"),
                    obj.get("tooltip_title"),
                    obj.get("tooltip_lede"),
                )
            };
            if let Some(t) = tooltip_obj {
                clean.insert("tooltip".to_string(), t);
            }
            Some(Value::Object(clean))
        })
        .collect()
}

/// Sanitize an i18n-aware text value: accepts either a plain string or a
/// locale-keyed object whose values are strings. Returns `None` when the
/// input is missing, the wrong shape, or has only empty values.
fn sanitize_i18n_text(value: &Value) -> Option<Value> {
    match value {
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(Value::String(trimmed.to_string()))
            }
        }
        Value::Object(map) => {
            let mut clean = serde_json::Map::new();
            for (locale, v) in map {
                if let Some(s) = v.as_str() {
                    let trimmed = s.trim();
                    if !trimmed.is_empty() {
                        clean.insert(locale.clone(), Value::String(trimmed.to_string()));
                    }
                }
            }
            if clean.is_empty() {
                None
            } else {
                Some(Value::Object(clean))
            }
        }
        _ => None,
    }
}

/// Convenience: take an `Option<&Value>` and return `Option<Value>`.
fn sanitize_i18n_text_opt(value: &Value) -> Option<Value> {
    sanitize_i18n_text(value)
}

/// Build a tooltip object from optional eyebrow/title/lede inputs. Returns
/// `None` if all three are empty (no tooltip → omit the field).
fn build_tooltip_obj(
    eyebrow: Option<&Value>,
    title: Option<&Value>,
    lede: Option<&Value>,
) -> Option<Value> {
    let mut clean = serde_json::Map::new();
    if let Some(v) = eyebrow.and_then(sanitize_i18n_text_opt) {
        clean.insert("eyebrow".to_string(), v);
    }
    if let Some(v) = title.and_then(sanitize_i18n_text_opt) {
        clean.insert("title".to_string(), v);
    }
    if let Some(v) = lede.and_then(sanitize_i18n_text_opt) {
        clean.insert("lede".to_string(), v);
    }
    if clean.is_empty() {
        None
    } else {
        Some(Value::Object(clean))
    }
}

fn is_placeholder_public_base_url(value: &str) -> bool {
    let normalized = value.trim().trim_end_matches('/').to_ascii_lowercase();
    normalized.is_empty()
        || normalized.contains("example.com")
        || normalized.contains("localhost")
        || normalized.contains("127.0.0.1")
}

/// Synchronize webchat-gui `skin` answer to the tenant config JSON.
///
/// Only runs for `messaging-webchat-gui` providers. When the operator picks a
/// non-empty `skin` value in setup (e.g. `3aigent`), this writes that value to
/// the `skin` field in `<bundle>/assets/webchat-gui/config/tenants/<tenant>.json`,
/// falling back to `default.json` if a tenant-specific file does not exist.
///
/// The webchat-gui SPA's runtime-bootstrap reads this field and, when present,
/// overrides URL-path skin loading to load `/skins/<skin>/skin.json` instead.
pub fn sync_skin_to_tenant_config(
    bundle_path: &Path,
    tenant: &str,
    provider_id: &str,
    answers: &Value,
) -> Result<bool> {
    if !provider_id.contains("webchat-gui") {
        return Ok(false);
    }

    let skin = answers
        .as_object()
        .and_then(|m| m.get("skin"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let Some(skin) = skin else {
        return Ok(false);
    };

    let Some(target) = resolve_or_scaffold_tenant_config(bundle_path, tenant)? else {
        return Ok(false);
    };

    let raw = std::fs::read_to_string(&target)
        .with_context(|| format!("read tenant config {}", target.display()))?;
    let mut config: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parse tenant config {}", target.display()))?;

    let obj = match config.as_object_mut() {
        Some(o) => o,
        None => return Ok(false),
    };

    let already_set = obj
        .get("skin")
        .and_then(Value::as_str)
        .map(|existing| existing == skin)
        .unwrap_or(false);
    if already_set {
        return Ok(false);
    }

    obj.insert("skin".to_string(), Value::String(skin.to_string()));
    let output = serde_json::to_string_pretty(&config)?;
    std::fs::write(&target, output)
        .with_context(|| format!("write tenant config {}", target.display()))?;
    Ok(true)
}

/// Sync the webchat-gui setup answer `nav_links_json` into the tenant config's
/// `nav_links` array.
///
/// Operators enter the array as a JSON string in the wizard
/// (e.g. `[{"label":"Module 5","url":"https://...","external":true}]`). This
/// function parses that string, validates each entry has non-empty `label` and
/// `url` strings (skipping malformed entries instead of failing), and writes
/// the resulting array to `<bundle>/assets/webchat-gui/config/tenants/<tenant>.json`.
///
/// An empty answer (or one that parses to an empty array) clears any existing
/// `nav_links` so removing all entries via the wizard hides the topbar nav at
/// runtime.
///
/// The webchat-gui SPA's runtime-bootstrap reads this array and renders one
/// anchor per entry between the brand block and the locale picker.
pub fn sync_nav_links_to_tenant_config(
    bundle_path: &Path,
    tenant: &str,
    provider_id: &str,
    answers: &Value,
) -> Result<bool> {
    if !provider_id.contains("webchat-gui") {
        return Ok(false);
    }

    let answers_obj = match answers.as_object() {
        Some(m) => m,
        None => return Ok(false),
    };

    // Three answer shapes are accepted, in priority order:
    //
    // 1. `nav_links` as a native array — produced by the new
    //    `kind: table` wizard. Each row is already a JSON object with
    //    `label`/`url`/`external` keys; we just sanitise.
    // 2. `nav_links_json` as a JSON string — legacy advanced-input answer
    //    that pre-dates the table wizard. Parsed, then sanitised.
    // 3. Neither present — leave the existing tenant config untouched.
    let parsed_links: Vec<Value> = if let Some(arr) =
        answers_obj.get("nav_links").and_then(Value::as_array)
    {
        sanitize_nav_link_array(arr)
    } else if let Some(raw) = answers_obj
        .get("nav_links_json")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        if raw.is_empty() {
            Vec::new()
        } else {
            let parsed: Value = serde_json::from_str(raw).with_context(|| {
                format!("parse nav_links_json answer (expected JSON array): {raw}")
            })?;
            let Some(arr) = parsed.as_array() else {
                anyhow::bail!("nav_links_json must be a JSON array, got: {raw}");
            };
            sanitize_nav_link_array(arr)
        }
    } else {
        return Ok(false);
    };

    let Some(target) = resolve_or_scaffold_tenant_config(bundle_path, tenant)? else {
        return Ok(false);
    };

    let raw_config = std::fs::read_to_string(&target)
        .with_context(|| format!("read tenant config {}", target.display()))?;
    let mut config: Value = serde_json::from_str(&raw_config)
        .with_context(|| format!("parse tenant config {}", target.display()))?;

    let obj = match config.as_object_mut() {
        Some(o) => o,
        None => return Ok(false),
    };

    let next = Value::Array(parsed_links);
    if obj.get("nav_links") == Some(&next) {
        return Ok(false);
    }

    obj.insert("nav_links".to_string(), next);

    let output = serde_json::to_string_pretty(&config)?;
    std::fs::write(&target, output)
        .with_context(|| format!("write tenant config {}", target.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::{
        is_placeholder_public_base_url, resolve_or_scaffold_tenant_config, resolve_public_base_url,
        sync_nav_links_to_tenant_config, sync_oauth_to_tenant_config, sync_skin_to_tenant_config,
        update_tenant_config,
    };
    use serde_json::{Map, Value, json};

    #[test]
    fn resolve_public_base_url_ignores_placeholder_answer() {
        let temp = tempfile::tempdir().unwrap();
        let answers = json!({
            "public_base_url": "https://example.com"
        });
        let resolved =
            resolve_public_base_url(temp.path(), "demo", answers.as_object().unwrap()).unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_public_base_url_prefers_non_placeholder_answer() {
        let temp = tempfile::tempdir().unwrap();
        let answers = json!({
            "public_base_url": "https://demo.example.net"
        });
        let resolved =
            resolve_public_base_url(temp.path(), "demo", answers.as_object().unwrap()).unwrap();
        assert_eq!(resolved.as_deref(), Some("https://demo.example.net"));
    }

    #[test]
    fn update_tenant_config_preserves_existing_redirect_when_public_base_url_missing() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join("demo.json");
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&json!({
                "auth": {
                    "providers": [
                        {
                            "id": "demo-google",
                            "enabled": true,
                            "redirectUri": "https://existing.example.net/v1/web/webchat/demo/"
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut answers = Map::new();
        answers.insert("oauth_enabled".into(), Value::Bool(true));
        answers.insert("oauth_enable_google".into(), Value::Bool(true));
        answers.insert(
            "oauth_google_client_id".into(),
            Value::String("client-id".into()),
        );

        update_tenant_config(&config_path, "demo", true, &answers, None).unwrap();

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            updated["auth"]["providers"][0]["redirectUri"].as_str(),
            Some("https://existing.example.net/v1/web/webchat/demo/")
        );
    }

    #[test]
    fn placeholder_detection_catches_local_and_example_urls() {
        assert!(is_placeholder_public_base_url("https://example.com"));
        assert!(is_placeholder_public_base_url("http://localhost:8080"));
        assert!(is_placeholder_public_base_url("http://127.0.0.1:8080"));
        assert!(!is_placeholder_public_base_url("https://demo.example.net"));
    }

    #[test]
    fn sync_skin_skips_non_webchat_provider() {
        let temp = tempfile::tempdir().unwrap();
        let answers = json!({ "skin": "3aigent" });
        let changed =
            sync_skin_to_tenant_config(temp.path(), "demo", "messaging-slack", &answers).unwrap();
        assert!(!changed);
    }

    #[test]
    fn sync_skin_skips_when_answer_missing_or_empty() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        std::fs::write(tenants_dir.join("demo.json"), r#"{"tenant_id":"demo"}"#).unwrap();

        for empty in [json!({}), json!({"skin": ""}), json!({"skin": "   "})] {
            let changed =
                sync_skin_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &empty)
                    .unwrap();
            assert!(!changed, "should not write for {empty}");
        }
    }

    #[test]
    fn sync_skin_writes_field_to_tenant_json() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(
            &tenant_file,
            r#"{"tenant_id":"demo","legacy_skin":"_template"}"#,
        )
        .unwrap();

        let answers = json!({ "skin": "3aigent" });
        let changed =
            sync_skin_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&tenant_file).unwrap()).unwrap();
        assert_eq!(updated["skin"].as_str(), Some("3aigent"));
        // legacy_skin must be preserved (separate concern)
        assert_eq!(updated["legacy_skin"].as_str(), Some("_template"));
    }

    #[test]
    fn sync_skin_scaffolds_tenant_json_from_default_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let default_file = tenants_dir.join("default.json");
        std::fs::write(
            &default_file,
            r#"{"tenant_id":"default","legacy_skin":"_template"}"#,
        )
        .unwrap();

        let answers = json!({ "skin": "3aigent" });
        let changed =
            sync_skin_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        // demo.json was scaffolded with skin field and remapped tenant_id
        let demo_file = tenants_dir.join("demo.json");
        assert!(demo_file.exists(), "demo.json must be scaffolded");
        let demo_parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&demo_file).unwrap()).unwrap();
        assert_eq!(demo_parsed["tenant_id"].as_str(), Some("demo"));
        assert_eq!(demo_parsed["skin"].as_str(), Some("3aigent"));
        assert_eq!(demo_parsed["legacy_skin"].as_str(), Some("_template"));

        // default.json must be left untouched — runtime depends on it as a template
        let default_after: Value =
            serde_json::from_str(&std::fs::read_to_string(&default_file).unwrap()).unwrap();
        assert_eq!(default_after["tenant_id"].as_str(), Some("default"));
        assert!(default_after.get("skin").is_none());
    }

    #[test]
    fn sync_skin_returns_false_when_no_tenant_or_default_config_exists() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();

        let answers = json!({ "skin": "3aigent" });
        let changed =
            sync_skin_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(!changed);
    }

    #[test]
    fn sync_skin_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(&tenant_file, r#"{"tenant_id":"demo","skin":"3aigent"}"#).unwrap();

        let answers = json!({ "skin": "3aigent" });
        let changed =
            sync_skin_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(!changed, "no-op when value already matches");
    }

    #[test]
    fn sync_nav_links_skips_non_webchat_provider() {
        let temp = tempfile::tempdir().unwrap();
        let answers = json!({ "nav_links_json": r#"[{"label":"X","url":"/x"}]"# });
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-slack", &answers)
                .unwrap();
        assert!(!changed);
    }

    #[test]
    fn sync_nav_links_skips_when_answer_absent() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        std::fs::write(tenants_dir.join("demo.json"), r#"{"tenant_id":"demo"}"#).unwrap();

        let answers = json!({});
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(!changed, "absent answer leaves config alone");
    }

    #[test]
    fn sync_nav_links_accepts_native_array_from_table_wizard() {
        // The new `kind: table` wizard writes the answer as a native JSON
        // array (not a JSON-string-as-array) under the `nav_links` key.
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(&tenant_file, r#"{"tenant_id":"demo"}"#).unwrap();

        let answers = json!({
            "nav_links": [
                { "label": "Help", "url": "/help", "external": false },
                { "label": "Docs", "url": "https://docs.example", "external": true },
                // Whitespace-only label dropped silently.
                { "label": "  ", "url": "/skipped" }
            ]
        });
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&tenant_file).unwrap()).unwrap();
        let links = updated["nav_links"].as_array().expect("nav_links array");
        assert_eq!(links.len(), 2, "third row dropped (label whitespace)");
        assert_eq!(links[0]["label"].as_str(), Some("Help"));
        assert_eq!(links[0]["url"].as_str(), Some("/help"));
        assert!(links[0].get("external").is_none(), "external=false omitted");
        assert_eq!(links[1]["label"].as_str(), Some("Docs"));
        assert_eq!(links[1]["external"].as_bool(), Some(true));
    }

    #[test]
    fn sync_nav_links_writes_array_to_tenant_json() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(&tenant_file, r#"{"tenant_id":"demo"}"#).unwrap();

        let answers = json!({
            "nav_links_json": r#"[
                { "label": "Module 5", "url": "https://example.com/m5", "external": true },
                { "label": "Help",     "url": "/help" }
            ]"#
        });
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&tenant_file).unwrap()).unwrap();
        let links = updated["nav_links"].as_array().expect("nav_links array");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0]["label"].as_str(), Some("Module 5"));
        assert_eq!(links[0]["url"].as_str(), Some("https://example.com/m5"));
        assert_eq!(links[0]["external"].as_bool(), Some(true));
        assert_eq!(links[1]["label"].as_str(), Some("Help"));
        assert_eq!(links[1]["url"].as_str(), Some("/help"));
        // external defaults to absent (not false) when omitted
        assert!(links[1].get("external").is_none());
    }

    #[test]
    fn sync_nav_links_drops_malformed_entries() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(&tenant_file, r#"{"tenant_id":"demo"}"#).unwrap();

        // Mix of valid + bad entries: missing url, empty label, non-object.
        let answers = json!({
            "nav_links_json": r#"[
                { "label": "Good", "url": "/ok" },
                { "label": "No URL" },
                { "label": "", "url": "/blank" },
                "not an object",
                { "label": "Also good", "url": "https://x" }
            ]"#
        });
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&tenant_file).unwrap()).unwrap();
        let links = updated["nav_links"].as_array().unwrap();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0]["label"].as_str(), Some("Good"));
        assert_eq!(links[1]["label"].as_str(), Some("Also good"));
    }

    #[test]
    fn sync_nav_links_clears_existing_when_answer_is_empty_array() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(
            &tenant_file,
            r#"{"tenant_id":"demo","nav_links":[{"label":"Old","url":"/old"}]}"#,
        )
        .unwrap();

        let answers = json!({ "nav_links_json": "[]" });
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&tenant_file).unwrap()).unwrap();
        assert!(updated["nav_links"].as_array().unwrap().is_empty());
    }

    #[test]
    fn sync_nav_links_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(
            &tenant_file,
            r#"{"tenant_id":"demo","nav_links":[{"label":"X","url":"/x"}]}"#,
        )
        .unwrap();

        let answers = json!({ "nav_links_json": r#"[{"label":"X","url":"/x"}]"# });
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(!changed, "no-op when content already matches");
    }

    #[test]
    fn sync_nav_links_returns_error_on_invalid_json() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        std::fs::write(tenants_dir.join("demo.json"), r#"{"tenant_id":"demo"}"#).unwrap();

        let answers = json!({ "nav_links_json": "not valid json" });
        let err =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap_err();
        assert!(err.to_string().contains("parse nav_links_json"));
    }

    #[test]
    fn sync_nav_links_scaffolds_tenant_json_from_default_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let default_file = tenants_dir.join("default.json");
        std::fs::write(&default_file, r#"{"tenant_id":"default"}"#).unwrap();

        let answers = json!({ "nav_links_json": r#"[{"label":"Help","url":"/help"}]"# });
        let changed =
            sync_nav_links_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let demo_file = tenants_dir.join("demo.json");
        assert!(demo_file.exists());
        let demo_parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&demo_file).unwrap()).unwrap();
        assert_eq!(demo_parsed["tenant_id"].as_str(), Some("demo"));
        assert_eq!(demo_parsed["nav_links"][0]["label"].as_str(), Some("Help"));

        let default_parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&default_file).unwrap()).unwrap();
        assert!(default_parsed.get("nav_links").is_none());
    }

    #[test]
    fn sync_oauth_scaffolds_tenant_json_from_default_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let default_file = tenants_dir.join("default.json");
        std::fs::write(
            &default_file,
            serde_json::to_string_pretty(&json!({
                "tenant_id": "default",
                "auth": { "providers": [] }
            }))
            .unwrap(),
        )
        .unwrap();

        let answers = json!({
            "oauth_enabled": true,
            "oauth_enable_google": true,
            "oauth_google_client_id": "client-xyz"
        });
        let changed =
            sync_oauth_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let demo_file = tenants_dir.join("demo.json");
        assert!(demo_file.exists());
        let demo_parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&demo_file).unwrap()).unwrap();
        assert_eq!(demo_parsed["tenant_id"].as_str(), Some("demo"));
        let provider = demo_parsed["auth"]["providers"]
            .as_array()
            .and_then(|arr| arr.iter().find(|p| p["id"] == "demo-google"))
            .expect("demo-google provider was added");
        assert_eq!(provider["enabled"].as_bool(), Some(true));
        assert_eq!(provider["clientId"].as_str(), Some("client-xyz"));

        // default.json must not be polluted
        let default_parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&default_file).unwrap()).unwrap();
        let default_providers = default_parsed["auth"]["providers"].as_array().unwrap();
        assert!(default_providers.is_empty(), "default.json must stay empty");
    }

    #[test]
    fn resolve_or_scaffold_returns_existing_tenant_file_unchanged() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let tenant_file = tenants_dir.join("demo.json");
        std::fs::write(&tenant_file, r#"{"tenant_id":"demo","skin":"existing"}"#).unwrap();

        let resolved = resolve_or_scaffold_tenant_config(temp.path(), "demo")
            .unwrap()
            .unwrap();
        assert_eq!(resolved, tenant_file);

        // File contents must be untouched
        assert_eq!(
            std::fs::read_to_string(&tenant_file).unwrap(),
            r#"{"tenant_id":"demo","skin":"existing"}"#
        );
    }

    #[test]
    fn resolve_or_scaffold_returns_none_when_neither_exists() {
        let temp = tempfile::tempdir().unwrap();
        let resolved = resolve_or_scaffold_tenant_config(temp.path(), "demo").unwrap();
        assert!(resolved.is_none());
    }

    #[test]
    fn resolve_or_scaffold_for_default_tenant_returns_default_json() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let default_file = tenants_dir.join("default.json");
        std::fs::write(&default_file, r#"{"tenant_id":"default"}"#).unwrap();

        let resolved = resolve_or_scaffold_tenant_config(temp.path(), "default")
            .unwrap()
            .unwrap();
        assert_eq!(resolved, default_file);
    }
}
