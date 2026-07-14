//! Shared helpers for provider setup backend contracts.
//!
//! The UI server currently owns most backend-contract execution, but the
//! mutation rules here are deliberately UI-independent so CLI and future
//! state-machine execution can share the same safety behavior.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use serde_json::{Map as JsonMap, Value};
use url::Url;

const REDACTED_SECRET_MARKER: &str = "[redacted:dev-secret]";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredProviderHttpRoute {
    pub provider_id: String,
    pub pack_path: PathBuf,
    pub methods: Vec<String>,
    pub target: ProviderHttpRouteTarget,
    pub segments: Vec<ProviderHttpRouteSegment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderHttpRouteTarget {
    SetupComponent { component_ref: String, op: String },
    ProviderIngress { component_ref: String, op: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderHttpRouteSegment {
    Literal(String),
    Tenant,
    Team,
    Wildcard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderHttpRouteMatch {
    pub route: DeclaredProviderHttpRoute,
    pub tenant: String,
    pub team: String,
}

pub fn server_owned_config_keys(contract: &Value) -> BTreeSet<String> {
    contract
        .get("server_owned_config_keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn merge_browser_config_update(
    stored: &mut JsonMap<String, Value>,
    incoming: &Value,
    contract: &Value,
    default_config: JsonMap<String, Value>,
) -> anyhow::Result<()> {
    let Some(incoming) = incoming.as_object() else {
        return Ok(());
    };
    let server_owned = server_owned_config_keys(contract);
    let config = stored
        .entry("config".to_string())
        .or_insert_with(|| Value::Object(default_config))
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored config is not an object"))?;

    for (key, value) in incoming {
        if server_owned.contains(key) {
            continue;
        }
        // Ephemeral tunnel URLs are engine-assigned: the browser only ever
        // *echoes* one from an earlier render, and the echo can be stale.
        // Merging it reverts the engine's corrected ingress URL on every
        // "Continue" click, which un-completes dependent steps and re-runs
        // them forever. A user-supplied *stable* URL still merges normally.
        if key == "public_base_url"
            && value
                .as_str()
                .is_some_and(crate::setup_tunnel::is_ephemeral_tunnel_url)
        {
            if config.get(key) != Some(value) {
                eprintln!(
                    "[setup config] ignoring browser-echoed ephemeral public_base_url \
                     {value} (stored: {stored}); ephemeral tunnel URLs are engine-owned",
                    stored = config.get(key).and_then(Value::as_str).unwrap_or("<unset>")
                );
            }
            continue;
        }
        config.insert(key.clone(), value.clone());
    }
    Ok(())
}

pub fn required_steps(contract: &Value) -> Vec<&str> {
    contract
        .get("required_order")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

pub fn action_by_id<'a>(contract: &'a Value, id: &str) -> Option<&'a Value> {
    contract
        .get("actions")
        .and_then(Value::as_array)?
        .iter()
        .find(|action| action.get("id").and_then(Value::as_str) == Some(id))
}

pub fn value_at_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|part| !part.is_empty())
        .try_fold(root, |current, part| current.get(part))
}

pub fn completion_met(values: &Value, completion: &Value) -> bool {
    let Some(path) = completion.get("state_path").and_then(Value::as_str) else {
        return false;
    };
    let Some(value) = value_at_path(values, path) else {
        return false;
    };
    if completion
        .get("exists")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return !value.is_null()
            && value
                .as_str()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(true);
    }
    if let Some(expected) = completion.get("equals") {
        return value == expected;
    }
    value.as_bool().unwrap_or(false)
}

pub fn render_status(contract: &Value, stored: &JsonMap<String, Value>) -> Value {
    let values = render_values(stored);
    let recovery_step = oauth_resume_token(stored)
        .and_then(|token| oauth_action_by_token_store_key(contract, token))
        .and_then(|action| action.get("id").and_then(Value::as_str));
    let items: Vec<Value> = required_steps(contract)
        .into_iter()
        .map(|step| {
            let done = Some(step) != recovery_step
                && action_by_id(contract, step)
                    .and_then(|action| action.get("completion"))
                    .is_some_and(|completion| completion_met(&values, completion));
            serde_json::json!({
                "id": step,
                "label": step.replace('_', " "),
                "state": if done { "done" } else { "pending" },
            })
        })
        .collect();
    let ok = !items.is_empty()
        && items
            .iter()
            .all(|item| item.get("state").and_then(Value::as_str) == Some("done"));
    let next = recovery_step.unwrap_or_else(|| {
        items
            .iter()
            .find(|item| item.get("state").and_then(Value::as_str) != Some("done"))
            .and_then(|item| item.get("id").and_then(Value::as_str))
            .unwrap_or("complete")
    });
    let blocked = stored
        .get("last_setup_result")
        .and_then(blocked_from_result)
        .unwrap_or(Value::Null);
    serde_json::json!({
        "ok": ok,
        "next": next,
        "items": items,
        "blocked": blocked,
    })
}

pub fn next_action_id(contract: &Value, stored: &JsonMap<String, Value>) -> Option<String> {
    let status = render_status(contract, stored);
    status
        .get("next")
        .and_then(Value::as_str)
        .filter(|step| *step != "complete")
        .map(str::to_string)
}

pub fn executor_kind(action: &Value) -> Option<&str> {
    action
        .get("executor")
        .and_then(Value::as_object)
        .and_then(|executor| executor.get("kind"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn executor(action: &Value) -> anyhow::Result<&Value> {
    action
        .get("executor")
        .ok_or_else(|| anyhow!("setup backend action missing executor"))
}

pub fn required_executor_str<'a>(executor: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    executor
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("setup backend executor missing {key}"))
}

pub fn config_mut(
    stored: &mut JsonMap<String, Value>,
) -> anyhow::Result<&mut JsonMap<String, Value>> {
    stored
        .entry("config".to_string())
        .or_insert_with(|| Value::Object(JsonMap::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored config is not an object"))
}

pub fn config_str(config: &JsonMap<String, Value>, key: &str) -> String {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

pub fn oauth_client_id(
    executor: &Value,
    config: &JsonMap<String, Value>,
) -> anyhow::Result<String> {
    let client_id_key = required_executor_str(executor, "client_id_config_key")?;
    let configured_client_id = config_str(config, client_id_key);
    if !configured_client_id.is_empty() {
        return Ok(configured_client_id);
    }
    Ok(executor
        .get("client_id_default")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string())
}

pub fn oauth_device_login_next_message() -> &'static str {
    "open the Microsoft device-code page, enter the code, then click Continue setup"
}

pub fn public_oauth_response(body: &Value) -> Value {
    let mut public = JsonMap::new();
    for key in [
        "user_code",
        "verification_uri",
        "verification_url",
        "verification_uri_complete",
        "expires_in",
        "interval",
        "message",
    ] {
        if let Some(value) = body.get(key) {
            public.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(public)
}

pub fn device_login_payload(
    config: &JsonMap<String, Value>,
    user_code_key: &str,
    body: &Value,
) -> Value {
    serde_json::json!({
        "url": config_str(config, "oauth_verification_uri"),
        "userCode": config_str(config, user_code_key),
        "user_code": config_str(config, user_code_key),
        "verification_uri_complete": body.get("verification_uri_complete").cloned().unwrap_or(Value::Null),
        "message": body.get("message").cloned().unwrap_or(Value::Null),
    })
}

pub fn execute_oauth_device_code_start_with_response(
    stored: &mut JsonMap<String, Value>,
    action: &Value,
    body: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let client_id = {
        let config = config_mut(stored)?;
        oauth_client_id(executor, config)?
    };
    if client_id.is_empty() {
        let client_id_key = required_executor_str(executor, "client_id_config_key")?;
        return Ok(step_result(
            action,
            false,
            &format!("set {client_id_key}, then retry"),
            serde_json::json!({
                "ok": false,
                "missing_config_key": client_id_key,
            }),
        ));
    }
    let authority_tenant = {
        let config = config_mut(stored)?;
        executor
            .get("authority_tenant_config_key")
            .and_then(Value::as_str)
            .map(|key| config_str(config, key))
            .filter(|value| !value.is_empty())
            .or_else(|| {
                executor
                    .get("authority_tenant_default")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "organizations".to_string())
    };
    let authority_template = required_executor_str(executor, "authority_url_template")?;
    let authority = authority_template.replace("{authority_tenant}", &authority_tenant);
    let token_url = format!("{}/oauth2/v2.0/token", authority.trim_end_matches('/'));
    let device_code = body
        .get("device_code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if device_code.is_empty() {
        return Ok(step_result(
            action,
            false,
            "OAuth device-code response did not include a device code.",
            serde_json::json!({ "ok": false, "body": body }),
        ));
    }
    let oauth_kind = executor
        .get("oauth_kind")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let device_code_key = executor
        .get("device_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_device_code");
    let user_code_key = executor
        .get("user_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_user_code");

    let login = {
        let config = config_mut(stored)?;
        config.insert(
            "oauth_kind".to_string(),
            Value::String(oauth_kind.to_string()),
        );
        config.insert(device_code_key.to_string(), Value::String(device_code));
        if let Some(user_code) = body.get("user_code").and_then(Value::as_str) {
            config.insert(
                user_code_key.to_string(),
                Value::String(user_code.to_string()),
            );
        }
        if let Some(verification_uri) = body
            .get("verification_uri")
            .or_else(|| body.get("verification_url"))
            .and_then(Value::as_str)
        {
            config.insert(
                "oauth_verification_uri".to_string(),
                Value::String(verification_uri.to_string()),
            );
        }
        config.insert("oauth_token_url".to_string(), Value::String(token_url));
        config.insert("oauth_client_id".to_string(), Value::String(client_id));
        device_login_payload(config, user_code_key, body)
    };
    stored.insert(
        "last_oauth".to_string(),
        serde_json::json!({
            "kind": oauth_kind,
            "response": public_oauth_response(body),
        }),
    );
    Ok(step_result(
        action,
        false,
        oauth_device_login_next_message(),
        serde_json::json!({
            "ok": false,
            "pending_device_login": true,
            "login": login,
            "body": public_oauth_response(body),
        }),
    ))
}

pub fn execute_oauth_device_code_start(
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let config = config_mut(stored)?;
    let client_id = oauth_client_id(executor, config)?;
    if client_id.is_empty() {
        let client_id_key = required_executor_str(executor, "client_id_config_key")?;
        return Ok(step_result(
            action,
            false,
            &format!("set {client_id_key}, then retry"),
            serde_json::json!({
                "ok": false,
                "missing_config_key": client_id_key,
            }),
        ));
    }
    let authority_tenant = executor
        .get("authority_tenant_config_key")
        .and_then(Value::as_str)
        .map(|key| config_str(config, key))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            executor
                .get("authority_tenant_default")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "organizations".to_string());
    let authority_template = required_executor_str(executor, "authority_url_template")?;
    let authority = authority_template.replace("{authority_tenant}", &authority_tenant);
    let scopes = executor
        .get("scopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if scopes.trim().is_empty() {
        return Ok(step_result(
            action,
            false,
            "OAuth device-code action has no scopes.",
            serde_json::json!({ "ok": false, "error": "oauth_device_code executor missing scopes" }),
        ));
    }
    let device_url = format!("{}/oauth2/v2.0/devicecode", authority.trim_end_matches('/'));
    let mut response = crate::http_client::api_agent()
        .post(&device_url)
        .send_form([
            ("client_id", client_id.as_str()),
            ("scope", scopes.as_str()),
        ])
        .context("OAuth device-code request failed")?;
    let body = response
        .body_mut()
        .read_json::<Value>()
        .context("failed to parse OAuth device-code response")?;
    execute_oauth_device_code_start_with_response(stored, action, &body)
}

pub fn oauth_device_login_started(stored: &JsonMap<String, Value>, action: &Value) -> bool {
    let Some(executor) = action.get("executor") else {
        return false;
    };
    let device_code_key = executor
        .get("device_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_device_code");
    stored
        .get("config")
        .and_then(Value::as_object)
        .map(|config| {
            !config_str(config, device_code_key).is_empty()
                && !config_str(config, "oauth_client_id").is_empty()
                && !config_str(config, "oauth_token_url").is_empty()
        })
        .unwrap_or(false)
}

pub fn execute_oauth_device_code_complete_with_response(
    stored: &mut JsonMap<String, Value>,
    action: &Value,
    status: u16,
    body: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let oauth_kind = executor
        .get("oauth_kind")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let device_code_key = executor
        .get("device_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_device_code");
    let token_store_key = required_executor_str(executor, "token_store_key")?;
    {
        let config = config_mut(stored)?;
        let device_code = config_str(config, device_code_key);
        let client_id = config_str(config, "oauth_client_id");
        let token_url = config_str(config, "oauth_token_url");
        if device_code.is_empty() || client_id.is_empty() || token_url.is_empty() {
            return Ok(step_result(
                action,
                false,
                "start device login first",
                serde_json::json!({ "ok": false, "error": "device_login_not_started" }),
            ));
        }
    }
    if let Some(error) = body.get("error").and_then(Value::as_str)
        && matches!(error, "authorization_pending" | "slow_down")
    {
        return Ok(step_result(
            action,
            false,
            "authorization is still pending",
            serde_json::json!({ "ok": false, "body": body }),
        ));
    }
    if status >= 400 || body.get("access_token").and_then(Value::as_str).is_none() {
        return Ok(step_result(
            action,
            false,
            "OAuth token polling failed.",
            serde_json::json!({ "ok": false, "http_status": status, "body": body }),
        ));
    }
    {
        let config = config_mut(stored)?;
        if let Some(token) = body.get("access_token").and_then(Value::as_str) {
            config.insert(
                token_store_key.to_string(),
                Value::String(token.to_string()),
            );
        }
        config.remove(device_code_key);
        let user_code_key = executor
            .get("user_code_store_key")
            .and_then(Value::as_str)
            .unwrap_or("oauth_user_code");
        config.remove(user_code_key);
        config.remove("oauth_kind");
        config.remove("oauth_client_id");
        config.remove("oauth_token_url");
    }
    clear_oauth_resume_for_token(stored, token_store_key);
    let oauth = stored
        .entry("oauth".to_string())
        .or_insert_with(|| Value::Object(JsonMap::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored oauth state is not an object"))?;
    oauth.insert(
        oauth_kind.to_string(),
        serde_json::json!({
            "ok": true,
            "completed_at": current_timestamp_ms(),
            "token_store_key": token_store_key,
        }),
    );
    Ok(step_result(
        action,
        true,
        "click again to continue setup",
        serde_json::json!({
            "ok": true,
            "persisted_keys": [token_store_key],
        }),
    ))
}

pub fn execute_oauth_device_code_complete(
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let device_code_key = executor
        .get("device_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_device_code");
    let (device_code, client_id, token_url) = {
        let config = config_mut(stored)?;
        (
            config_str(config, device_code_key),
            config_str(config, "oauth_client_id"),
            config_str(config, "oauth_token_url"),
        )
    };
    if device_code.is_empty() || client_id.is_empty() || token_url.is_empty() {
        return Ok(step_result(
            action,
            false,
            "start device login first",
            serde_json::json!({ "ok": false, "error": "device_login_not_started" }),
        ));
    }
    let agent = crate::http_client::api_agent_any_status();
    let mut response = agent
        .post(&token_url)
        .send_form([
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id.as_str()),
            ("device_code", device_code.as_str()),
        ])
        .context("OAuth device-code token polling failed")?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json::<Value>()
        .context("failed to parse OAuth device-code token response")?;
    execute_oauth_device_code_complete_with_response(stored, action, status, &body)
}

pub fn expand_template(
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    template: &str,
) -> String {
    let mut expanded = template
        .replace("{tenant}", tenant)
        .replace("{team}", team)
        .replace("{env}", env);
    if expanded.contains("{public_base_url}") {
        let public_base = config_str(config, "public_base_url")
            .trim_end_matches('/')
            .to_string();
        expanded = expanded.replace("{public_base_url}", &public_base);
    }
    for (key, value) in config {
        if let Some(value) = value.as_str() {
            expanded = expanded.replace(&format!("{{{key}}}"), value);
        }
    }
    expanded
}

pub fn expand_json_template(
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    value: &Value,
) -> Value {
    match value {
        Value::String(template) => {
            Value::String(expand_template(tenant, team, env, config, template))
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| expand_json_template(tenant, team, env, config, item))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        expand_json_template(tenant, team, env, config, value),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn template_unresolved(value: &str) -> bool {
    value.contains('{') || value.contains('}')
}

pub fn validate_provider_http_url(url: &str) -> anyhow::Result<()> {
    let parsed =
        Url::parse(url).with_context(|| format!("provider_http target is not a URL: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        anyhow::bail!("provider_http target must be an http(s) URL");
    }
    Ok(())
}

pub fn provider_http_url(
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> anyhow::Result<String> {
    let Some(template) = executor
        .get("url_template")
        .or_else(|| executor.get("target_url_template"))
        .and_then(Value::as_str)
    else {
        anyhow::bail!("provider_http path_template requires setup UI/provider route dispatch");
    };
    let url = expand_template(tenant, team, env, config, template);
    validate_provider_http_url(&url)?;
    Ok(url)
}

pub fn is_safe_same_origin_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && Url::parse(path).is_err()
}

pub fn provider_http_path(
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> anyhow::Result<String> {
    let path_template = executor
        .get("path_template")
        .or_else(|| executor.get("target_path_template"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("provider_http executor requires path_template"))?;
    let path = expand_template(tenant, team, env, config, path_template);
    if !is_safe_same_origin_path(&path) {
        anyhow::bail!("provider_http executor path_template must resolve to a safe absolute path");
    }
    Ok(path)
}

pub fn provider_http_payload(
    _provider_id: &str,
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    if let Some(template) = executor
        .get("body")
        .or_else(|| executor.get("body_template"))
        .or_else(|| executor.get("request_body"))
    {
        return Ok(expand_json_template(tenant, team, env, config, template));
    }
    anyhow::bail!(
        "provider_http headless execution requires an explicit body/body_template/request_body"
    );
}

fn provider_http_state_key<'a>(action: &'a Value, executor: &'a Value) -> &'a str {
    executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .or_else(|| action.get("id").and_then(Value::as_str))
        .unwrap_or("last_provider_http")
}

fn provider_http_response_ok(response: &Value) -> bool {
    response
        .get("body")
        .and_then(|body| body.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| response.get("ok").and_then(Value::as_bool).unwrap_or(false))
}

pub fn provider_http_result_from_response(
    stored: &mut JsonMap<String, Value>,
    action: &Value,
    target: &str,
    response: Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let ok = provider_http_response_ok(&response);
    let state_key = provider_http_state_key(action, executor);
    let result = serde_json::json!({
        "ok": ok,
        "target": target,
        "response": response,
    });
    stored.insert(state_key.to_string(), result.clone());
    Ok(step_result(
        action,
        ok,
        if ok {
            "click again to continue setup"
        } else {
            "fix provider setup endpoint and retry"
        },
        result,
    ))
}

pub fn execute_runtime_observation(
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_activity");
    if stored.get(state_key).is_some() {
        return Ok(step_result(
            action,
            true,
            "runtime observation is present",
            serde_json::json!({
                "ok": true,
                "state_store_key": state_key,
                "observed": stored.get(state_key).cloned().unwrap_or(Value::Null),
            }),
        ));
    }
    Ok(step_result(
        action,
        false,
        "runtime observation not present yet",
        serde_json::json!({
            "ok": false,
            "waiting": true,
            "blocked": true,
            "error": "runtime observation not present yet",
            "retryable": true,
            "source": executor.get("source").cloned().unwrap_or(Value::Null),
            "event": executor.get("event").cloned().unwrap_or(Value::Null),
            "state_store_key": state_key,
        }),
    ))
}

pub fn execute_microsoft_graph_application(
    stored: &mut JsonMap<String, Value>,
    provider_id: &str,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let config = config_mut(stored)?;
    let token_key = required_executor_str(executor, "graph_token_store_key")?;
    let token = config_str(config, token_key);
    if token.is_empty() {
        return Ok(step_result(
            action,
            false,
            &format!("complete OAuth for {token_key}, then retry"),
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "missing_token_store_key": token_key,
                "retryable": true,
            }),
        ));
    }

    let app_id_key = required_executor_str(executor, "app_id_config_key")?;
    let secret_key = required_executor_str(executor, "client_secret_config_key")?;
    let display_name_key = required_executor_str(executor, "display_name_config_key")?;
    let configured_app_id = config_str(config, app_id_key);
    let mut display_name = config_str(config, display_name_key);
    if display_name.is_empty() {
        display_name = "Greentic Bot".to_string();
    }

    let graph_base_url = executor
        .get("graph_base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://graph.microsoft.com/v1.0")
        .trim_end_matches('/')
        .to_string();
    validate_provider_http_url(&graph_base_url)?;
    let select = "id,appId,displayName,signInAudience";
    let filter = if configured_app_id.is_empty() {
        format!("displayName eq '{}'", odata_string(&display_name))
    } else {
        format!("appId eq '{}'", odata_string(&configured_app_id))
    };
    let lookup_url = format!(
        "{graph_base_url}/applications?$filter={}&$select={}",
        url_encode(&filter),
        url_encode(select)
    );
    let agent = graph_agent();
    let lookup = graph_json_request(&agent, "GET", &lookup_url, Some(&token), None)?;
    if !lookup.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        if let Some(result) =
            oauth_required_result(action, token_key, "authenticated request failed", &lookup)
        {
            return Ok(result);
        }
        return Ok(step_result(
            action,
            false,
            "Microsoft Graph application lookup failed.",
            lookup,
        ));
    }

    let items = lookup
        .get("body")
        .and_then(|body| body.get("value"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (app, action_name) = if let Some(app) = items.first() {
        (
            app.clone(),
            if configured_app_id.is_empty() {
                "reuse"
            } else {
                "reuse_by_app_id"
            },
        )
    } else {
        if !configured_app_id.is_empty() {
            return Ok(step_result(
                action,
                false,
                "configured app id was not found in Microsoft Graph applications",
                serde_json::json!({ "ok": false, "configured_app_id": configured_app_id }),
            ));
        }
        let create = graph_json_request(
            &agent,
            "POST",
            &format!("{graph_base_url}/applications"),
            Some(&token),
            Some(serde_json::json!({
                "displayName": display_name,
                "signInAudience": executor
                    .get("sign_in_audience")
                    .and_then(Value::as_str)
                    .unwrap_or("AzureADMultipleOrgs"),
            })),
        )?;
        if !create.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            if let Some(result) =
                oauth_required_result(action, token_key, "authenticated request failed", &create)
            {
                return Ok(result);
            }
            return Ok(step_result(
                action,
                false,
                "Microsoft Graph application create failed.",
                create,
            ));
        }
        (create.get("body").cloned().unwrap_or(Value::Null), "create")
    };

    let object_id = app.get("id").and_then(Value::as_str).unwrap_or_default();
    let app_id = app.get("appId").and_then(Value::as_str).unwrap_or_default();
    if !app_id.is_empty() {
        config.insert(app_id_key.to_string(), Value::String(app_id.to_string()));
    }
    config.insert(
        display_name_key.to_string(),
        Value::String(display_name.clone()),
    );

    let mut secret_action = "keep_existing_secret";
    if config_str(config, secret_key).is_empty() {
        if object_id.is_empty() {
            return Ok(step_result(
                action,
                false,
                "app object id missing; cannot add password",
                serde_json::json!({ "ok": false, "app": app }),
            ));
        }
        let password_display_name = executor
            .get("password_display_name")
            .and_then(Value::as_str)
            .unwrap_or("setup secret");
        let secret = graph_json_request(
            &agent,
            "POST",
            &format!(
                "{graph_base_url}/applications/{}/addPassword",
                url_encode(object_id)
            ),
            Some(&token),
            Some(serde_json::json!({
                "passwordCredential": { "displayName": password_display_name }
            })),
        )?;
        if !secret.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            if let Some(result) =
                oauth_required_result(action, token_key, "authenticated request failed", &secret)
            {
                return Ok(result);
            }
            return Ok(step_result(
                action,
                false,
                "Microsoft Graph addPassword failed.",
                secret,
            ));
        }
        if let Some(secret_text) = secret
            .get("body")
            .and_then(|body| body.get("secretText"))
            .and_then(Value::as_str)
        {
            config.insert(
                secret_key.to_string(),
                Value::String(secret_text.to_string()),
            );
            secret_action = "generated_secret";
        }
    }

    let result = serde_json::json!({
        "ok": true,
        "action": action_name,
        "secret_action": secret_action,
        "app_id": app_id,
        "bot_app_id": app_id,
        "app_object_id": object_id,
        "display_name": display_name,
        "provider_id": provider_id,
    });
    stored.insert("last_app_registration".to_string(), result.clone());
    Ok(step_result(
        action,
        true,
        "click again to continue setup",
        result,
    ))
}

pub fn execute_microsoft_graph_teams_app_user_install(
    stored: &mut JsonMap<String, Value>,
    tenant: &str,
    team: &str,
    env: &str,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let config = config_mut(stored)?.clone();
    let token_key = required_executor_str(executor, "graph_token_store_key")?;
    let token = config_str(&config, token_key);
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_teams_app_install");
    let links = expand_executor_links(tenant, team, env, &config, executor);
    let publish_state_key = executor
        .get("publish_state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_teams_app_publish");
    let publish = stored
        .get(publish_state_key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let catalog_app_id = publish
        .get("catalog_app_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if catalog_app_id.is_empty() {
        return Ok(step_result(
            action,
            false,
            "publish the Teams app before installing it for the signed-in user",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "missing_catalog_app_id",
                "links": links,
            }),
        ));
    }
    if token.is_empty() {
        let result = serde_json::json!({
            "ok": true,
            "action": "manual_unverified",
            "catalog_app_id": catalog_app_id,
            "warning": format!("{} is unavailable; continuing with manual install links", token_key),
            "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
            "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
        });
        stored.insert(state_key.to_string(), result.clone());
        return Ok(step_result(
            action,
            true,
            "open the bot chat link and send a message",
            result,
        ));
    }

    let graph_base_url = executor
        .get("graph_base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://graph.microsoft.com/v1.0")
        .trim_end_matches('/')
        .to_string();
    validate_provider_http_url(&graph_base_url)?;
    let agent = graph_agent();
    let installed = graph_json_request(
        &agent,
        "GET",
        &format!("{graph_base_url}/me/teamwork/installedApps?$expand=teamsApp"),
        Some(&token),
        None,
    )?;
    if !installed
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let result = serde_json::json!({
            "ok": true,
            "action": "manual_unverified",
            "catalog_app_id": catalog_app_id,
            "warning": "Graph could not verify installed apps; continuing with manual install links",
            "previous": installed,
            "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
            "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
        });
        stored.insert(state_key.to_string(), result.clone());
        return Ok(step_result(
            action,
            true,
            "open the bot chat link and send a message",
            result,
        ));
    }
    let items = installed
        .get("body")
        .and_then(|body| body.get("value"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let existing = items.iter().find(|item| {
        item.get("teamsApp")
            .and_then(|teams_app| teams_app.get("id"))
            .and_then(Value::as_str)
            == Some(catalog_app_id.as_str())
    });
    let (action_name, installed_app_id) = if let Some(item) = existing {
        (
            "keep",
            item.get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    } else {
        let created = graph_json_request(
            &agent,
            "POST",
            &format!("{graph_base_url}/me/teamwork/installedApps"),
            Some(&token),
            Some(serde_json::json!({
                "teamsApp@odata.bind": format!("{graph_base_url}/appCatalogs/teamsApps/{catalog_app_id}")
            })),
        )?;
        if !created.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let result = serde_json::json!({
                "ok": true,
                "action": "manual_unverified",
                "catalog_app_id": catalog_app_id,
                "warning": "Graph could not install the app; continuing with manual install links",
                "previous": created,
                "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
                "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
            });
            stored.insert(state_key.to_string(), result.clone());
            return Ok(step_result(
                action,
                true,
                "open the bot chat link and send a message",
                result,
            ));
        }
        (
            "install",
            created
                .get("body")
                .and_then(|body| body.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    };
    let result = serde_json::json!({
        "ok": true,
        "action": action_name,
        "catalog_app_id": catalog_app_id,
        "installed_app_id": installed_app_id,
        "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
        "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
    });
    stored.insert(state_key.to_string(), result.clone());
    Ok(step_result(
        action,
        true,
        "open the bot chat link and send a message",
        result,
    ))
}

pub fn execute_microsoft_graph_teams_app_catalog_publish(
    provider_pack_path: &Path,
    stored: &mut JsonMap<String, Value>,
    tenant: &str,
    team: &str,
    env: &str,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let config = config_mut(stored)?;
    let token_key = required_executor_str(executor, "graph_token_store_key")?;
    let token = config_str(config, token_key);
    if token.is_empty() {
        return Ok(step_result(
            action,
            false,
            &format!("complete OAuth for {token_key}, then retry"),
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "missing_token_store_key": token_key,
                "retryable": true,
            }),
        ));
    }
    let app_id_key = required_executor_str(executor, "teams_app_id_config_key")?;
    let version_key = required_executor_str(executor, "teams_app_version_config_key")?;
    let bot_app_id_key = required_executor_str(executor, "bot_app_id_config_key")?;
    if config_str(config, app_id_key).is_empty() {
        let fallback = config_str(config, bot_app_id_key);
        if fallback.is_empty() {
            return Ok(step_result(
                action,
                false,
                &format!("{app_id_key} or {bot_app_id_key} is required"),
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "missing_config_keys": [app_id_key, bot_app_id_key],
                }),
            ));
        }
        config.insert(app_id_key.to_string(), Value::String(fallback));
    }
    if config_str(config, version_key).is_empty() {
        config.insert(version_key.to_string(), Value::String("1.0.0".to_string()));
    }
    let package = build_teams_app_package(provider_pack_path, tenant, team, env, config, executor)?;
    let app_id = config_str(config, app_id_key);
    let manifest_version = config_str(config, version_key);
    let links = expand_executor_links(tenant, team, env, config, executor);
    let graph_base_url = executor
        .get("graph_base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("https://graph.microsoft.com/v1.0")
        .trim_end_matches('/')
        .to_string();
    validate_provider_http_url(&graph_base_url)?;
    let agent = graph_agent();
    let filter = format!("externalId eq '{}'", odata_string(&app_id));
    let lookup_url = format!(
        "{graph_base_url}/appCatalogs/teamsApps?$filter={}&$select={}",
        url_encode(&filter),
        url_encode("id,externalId,displayName,distributionMethod")
    );
    let lookup = graph_json_request(&agent, "GET", &lookup_url, Some(&token), None)?;
    if !lookup.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        if let Some(result) =
            oauth_required_result(action, token_key, "authenticated request failed", &lookup)
        {
            return Ok(result);
        }
        return Ok(step_result(
            action,
            false,
            "Teams app catalog lookup failed.",
            lookup,
        ));
    }
    let items = lookup
        .get("body")
        .and_then(|body| body.get("value"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (url, action_name, catalog_app_id) = if let Some(item) = items.first() {
        let catalog_app_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        (
            format!(
                "{graph_base_url}/appCatalogs/teamsApps/{}/appDefinitions",
                url_encode(catalog_app_id)
            ),
            "update",
            catalog_app_id.to_string(),
        )
    } else {
        (
            format!("{graph_base_url}/appCatalogs/teamsApps"),
            "publish",
            String::new(),
        )
    };
    let published = graph_binary_request(&agent, "POST", &url, &token, package)?;
    if !published
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        if let Some(result) = oauth_required_result(
            action,
            token_key,
            "authenticated request failed",
            &published,
        ) {
            return Ok(result);
        }
        return Ok(step_result(
            action,
            false,
            "Teams app catalog publish failed.",
            published,
        ));
    }
    let body_catalog_id = published
        .get("body")
        .and_then(|body| body.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let catalog_app_id = if catalog_app_id.is_empty() {
        body_catalog_id.to_string()
    } else {
        catalog_app_id
    };
    let add_to_teams_url = links
        .get("add_to_teams_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if catalog_app_id.is_empty() {
                String::new()
            } else {
                format!(
                    "https://teams.microsoft.com/l/app/{}?source=app-details-dialog",
                    url_encode(&catalog_app_id)
                )
            }
        });
    let result = serde_json::json!({
        "ok": true,
        "action": action_name,
        "teams_app_id": app_id,
        "catalog_app_id": catalog_app_id,
        "manifest_version": manifest_version,
        "add_to_teams_url": add_to_teams_url,
    });
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_teams_app_publish");
    stored.insert(state_key.to_string(), result.clone());
    Ok(step_result(
        action,
        true,
        "open the Add to Teams link, install the app, then continue",
        result,
    ))
}

fn build_teams_app_package(
    provider_pack_path: &Path,
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> anyhow::Result<Vec<u8>> {
    let manifest_asset = required_executor_str(executor, "manifest_template_asset")?;
    let base = executor
        .get("package_assets_base")
        .and_then(Value::as_str)
        .unwrap_or("assets/teams-app");
    if !is_safe_pack_relative_path(manifest_asset) || !is_safe_pack_relative_path(base) {
        anyhow::bail!("teams app package asset path is not safe");
    }
    let mut manifest = crate::discovery::read_pack_json_asset(provider_pack_path, manifest_asset)?;
    replace_json_templates(tenant, team, env, config, &mut manifest);

    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut out);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("manifest.json", options)?;
        let manifest_text = serde_json::to_vec_pretty(&manifest)?;
        std::io::Write::write_all(&mut writer, &manifest_text)?;
        for name in ["color.png", "outline.png"] {
            let asset = format!("{}/{}", base.trim_end_matches('/'), name);
            if let Ok(bytes) = read_pack_binary_asset(provider_pack_path, &asset) {
                writer.start_file(name, options)?;
                std::io::Write::write_all(&mut writer, &bytes)?;
            }
        }
        writer.finish()?;
    }
    Ok(out.into_inner())
}

fn read_pack_binary_asset(pack_path: &Path, entry_name: &str) -> anyhow::Result<Vec<u8>> {
    if !is_safe_pack_relative_path(entry_name) {
        anyhow::bail!("unsafe pack asset path: {entry_name}");
    }
    let file = std::fs::File::open(pack_path)
        .with_context(|| format!("open provider pack {}", pack_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("provider pack is not a zip archive")?;
    let mut entry = archive
        .by_name(entry_name)
        .with_context(|| format!("provider pack missing {entry_name}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes)?;
    Ok(bytes)
}

fn is_safe_pack_relative_path(path: &str) -> bool {
    let candidate = Path::new(path);
    !path.trim().is_empty()
        && candidate.is_relative()
        && !candidate
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn replace_json_templates(
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    value: &mut Value,
) {
    match value {
        Value::String(text) => {
            *text = expand_template(tenant, team, env, config, text);
        }
        Value::Array(items) => {
            for item in items {
                replace_json_templates(tenant, team, env, config, item);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                replace_json_templates(tenant, team, env, config, value);
            }
        }
        _ => {}
    }
}

fn expand_executor_links(
    tenant: &str,
    team: &str,
    env: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> Value {
    let mut links = JsonMap::new();
    if let Some(raw_links) = executor.get("links").and_then(Value::as_object) {
        for (key, value) in raw_links {
            if let Some(template) = value.as_str() {
                let output_key = key.strip_suffix("_template").unwrap_or(key).to_string();
                links.insert(
                    output_key,
                    Value::String(expand_template(tenant, team, env, config, template)),
                );
            }
        }
    }
    Value::Object(links)
}

fn graph_agent() -> ureq::Agent {
    crate::http_client::api_agent_any_status()
}

fn graph_json_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> anyhow::Result<Value> {
    let auth = bearer.map(|token| format!("Bearer {token}"));
    let response = match method {
        "GET" => {
            let mut request = agent.get(url);
            if let Some(auth) = auth.as_deref() {
                request = request.header("Authorization", auth);
            }
            request.call()
        }
        "POST" => {
            let mut request = agent.post(url);
            if let Some(auth) = auth.as_deref() {
                request = request.header("Authorization", auth);
            }
            if let Some(body) = body {
                request.send_json(&body)
            } else {
                request.send_empty()
            }
        }
        other => anyhow::bail!("unsupported Microsoft Graph method: {other}"),
    };
    let mut response = response.with_context(|| format!("request failed: {url}"))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json::<Value>()
        .unwrap_or(Value::Null);
    Ok(serde_json::json!({
        "ok": status < 400,
        "status": status,
        "body": body,
    }))
}

fn graph_binary_request(
    agent: &ureq::Agent,
    method: &str,
    url: &str,
    bearer: &str,
    payload: Vec<u8>,
) -> anyhow::Result<Value> {
    let auth = format!("Bearer {bearer}");
    let response = match method {
        "POST" => agent
            .post(url)
            .header("Authorization", auth)
            .header("Content-Type", "application/zip")
            .send(&payload),
        other => anyhow::bail!("unsupported Microsoft Graph binary method: {other}"),
    };
    let mut response = response.with_context(|| format!("request failed: {url}"))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().unwrap_or_default();
    let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    Ok(serde_json::json!({
        "ok": status < 400,
        "status": status,
        "body": body,
    }))
}

fn oauth_required_result(
    action: &Value,
    token_key: &str,
    reason: &str,
    response: &Value,
) -> Option<Value> {
    if !response_needs_oauth(response) {
        return None;
    }
    Some(step_result(
        action,
        false,
        &format!("complete OAuth for {token_key}, then retry"),
        serde_json::json!({
            "ok": false,
            "blocked": true,
            "error": "oauth_required",
            "reason": reason,
            "token_store_key": token_key,
            "resume_step": action.get("id").and_then(Value::as_str).unwrap_or_default(),
            "previous": response,
        }),
    ))
}

fn response_needs_oauth(response: &Value) -> bool {
    let status = response.get("status").and_then(Value::as_u64).unwrap_or(0);
    let body = response.get("body").unwrap_or(response);
    let text = body.to_string().to_ascii_lowercase();
    let says_unauthorized = status == 401
        || text.contains("http 401")
        || text.contains("status 401")
        || text.contains("unauthorized")
        || text.contains("invalid token");
    let says_expired = text.contains("expired")
        || text.contains("expiry")
        || text.contains("token exp")
        || text.contains("lifetime validation failed")
        || text.contains("token is expired");
    let says_invalid = text.contains("invalid token")
        || text.contains("access token is invalid")
        || text.contains("invalid access token")
        // Microsoft Graph 401s carry CamelCase codes and JWT-shape complaints
        // (e.g. a redaction placeholder sent as the bearer token) with none of
        // the spaced phrases above.
        || text.contains("invalidauthenticationtoken")
        || text.contains("jwt is not well formed")
        || text.contains("compact serialization format");
    says_unauthorized && (says_expired || says_invalid)
}

fn odata_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

pub fn execute_provider_http_external(
    stored: &mut JsonMap<String, Value>,
    provider_id: &str,
    tenant: &str,
    team: &str,
    env: &str,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let config = config_mut(stored)?.clone();
    let target = match provider_http_url(tenant, team, env, &config, executor) {
        Ok(url) => url,
        Err(err) => {
            return Ok(step_result(
                action,
                false,
                &err.to_string(),
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": err.to_string(),
                }),
            ));
        }
    };
    if template_unresolved(&target) || target.trim().is_empty() {
        return Ok(step_result(
            action,
            false,
            "provider_http executor target could not be resolved",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "provider_http executor target could not be resolved",
                "target": target,
            }),
        ));
    }
    let payload = match provider_http_payload(provider_id, tenant, team, env, &config, action) {
        Ok(payload) => payload,
        Err(err) => {
            return Ok(step_result(
                action,
                false,
                &err.to_string(),
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": err.to_string(),
                    "target": target,
                }),
            ));
        }
    };
    let method = executor
        .get("method")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("POST")
        .to_ascii_uppercase();
    let agent = crate::http_client::api_agent_any_status();
    let response = match method.as_str() {
        "POST" => agent.post(&target).send_json(&payload),
        "GET" => agent.get(&target).call(),
        _ => {
            return Ok(step_result(
                action,
                false,
                "provider_http headless execution supports GET and POST only",
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": "unsupported_provider_http_method",
                    "method": method,
                    "target": target,
                }),
            ));
        }
    };
    let mut response = match response {
        Ok(response) => response,
        Err(err) => {
            return Ok(step_result(
                action,
                false,
                "Provider setup service is not running",
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": "Provider setup service is not running",
                    "target": target,
                    "detail": err.to_string(),
                }),
            ));
        }
    };
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json::<Value>()
        .unwrap_or(Value::Null);
    let response = serde_json::json!({
        "ok": status < 400,
        "status": status,
        "body": body,
    });
    provider_http_result_from_response(stored, action, &target, response)
}

pub fn load_declared_provider_http_routes(
    bundle_path: &Path,
) -> anyhow::Result<Vec<DeclaredProviderHttpRoute>> {
    let discovered = crate::discovery::discover(bundle_path)?;
    let mut routes = Vec::new();
    for provider in discovered.providers {
        let Some(http_routes_extension) =
            crate::discovery::read_pack_extension(&provider.pack_path, "greentic.http-routes.v1")?
        else {
            continue;
        };
        let http_routes =
            extension_inline(&http_routes_extension).unwrap_or(&http_routes_extension);
        let ingress_extension = crate::discovery::read_pack_extension(
            &provider.pack_path,
            "messaging.provider_ingress.v1",
        )?;
        let ingress = ingress_extension
            .as_ref()
            .and_then(|extension| extension_inline(extension).or(Some(extension)));
        let Some(records) = http_routes.get("routes").and_then(Value::as_array) else {
            continue;
        };
        for record in records {
            let Some(pattern) = record
                .get("pattern")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let methods = record
                .get("methods")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect();
            let Some(target) = declared_provider_http_route_target(record, ingress) else {
                continue;
            };
            routes.push(DeclaredProviderHttpRoute {
                provider_id: provider.provider_id.clone(),
                pack_path: provider.pack_path.clone(),
                methods,
                target,
                segments: parse_provider_http_route_pattern(pattern),
            });
        }
    }
    Ok(routes)
}

pub fn find_declared_provider_http_route(
    bundle_path: &Path,
    method: &str,
    path: &str,
    default_tenant: &str,
    default_team: &str,
) -> anyhow::Result<Option<ProviderHttpRouteMatch>> {
    let mut routes = load_declared_provider_http_routes(bundle_path)?;
    routes.sort_by(|a, b| {
        let a_wild = a
            .segments
            .iter()
            .any(|segment| matches!(segment, ProviderHttpRouteSegment::Wildcard));
        let b_wild = b
            .segments
            .iter()
            .any(|segment| matches!(segment, ProviderHttpRouteSegment::Wildcard));
        b.segments
            .len()
            .cmp(&a.segments.len())
            .then(a_wild.cmp(&b_wild))
    });
    let request_segments: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    for route in routes {
        if !route.methods.is_empty()
            && !route
                .methods
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(method))
        {
            continue;
        }
        if let Some((tenant, team)) =
            match_provider_http_route(&route, &request_segments, default_tenant, default_team)
        {
            return Ok(Some(ProviderHttpRouteMatch {
                route,
                tenant,
                team,
            }));
        }
    }
    Ok(None)
}

fn extension_inline(extension: &Value) -> Option<&Value> {
    extension.get("inline").or(Some(extension))
}

fn declared_provider_http_route_target(
    record: &Value,
    ingress: Option<&Value>,
) -> Option<ProviderHttpRouteTarget> {
    let setup_component_ref = record
        .get("setup_component_ref")
        .or_else(|| record.get("component_ref"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(component_ref) = setup_component_ref {
        let op = record
            .get("setup_op")
            .or_else(|| record.get("op"))
            .or_else(|| record.get("provider_op"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("handle_http")
            .to_string();
        return Some(ProviderHttpRouteTarget::SetupComponent {
            component_ref: component_ref.to_string(),
            op,
        });
    }

    let ingress = ingress?;
    let component_ref = ingress
        .get("component_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let op = record
        .get("provider_op")
        .or_else(|| record.get("op"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ingest_http")
        .to_string();
    Some(ProviderHttpRouteTarget::ProviderIngress {
        component_ref: component_ref.to_string(),
        op,
    })
}

pub fn parse_provider_http_route_pattern(pattern: &str) -> Vec<ProviderHttpRouteSegment> {
    pattern
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            if segment == "{tenant}" {
                ProviderHttpRouteSegment::Tenant
            } else if segment == "{team}" {
                ProviderHttpRouteSegment::Team
            } else if segment.ends_with("*}") || segment == "*" {
                ProviderHttpRouteSegment::Wildcard
            } else {
                ProviderHttpRouteSegment::Literal(segment.to_string())
            }
        })
        .collect()
}

pub fn match_provider_http_route(
    route: &DeclaredProviderHttpRoute,
    request_segments: &[&str],
    default_tenant: &str,
    default_team: &str,
) -> Option<(String, String)> {
    let mut tenant = default_tenant.to_string();
    let mut team = default_team.to_string();
    let mut request_index = 0;
    for segment in &route.segments {
        match segment {
            ProviderHttpRouteSegment::Literal(expected) => {
                if request_segments.get(request_index)? != expected {
                    return None;
                }
                request_index += 1;
            }
            ProviderHttpRouteSegment::Tenant => {
                tenant = request_segments.get(request_index)?.to_string();
                if tenant.is_empty() {
                    return None;
                }
                request_index += 1;
            }
            ProviderHttpRouteSegment::Team => {
                team = request_segments.get(request_index)?.to_string();
                if team.is_empty() {
                    return None;
                }
                request_index += 1;
            }
            ProviderHttpRouteSegment::Wildcard => {
                return Some((tenant, team));
            }
        }
    }
    if request_index == request_segments.len() {
        Some((tenant, team))
    } else {
        None
    }
}

pub fn execute_provider_http_local_route(
    bundle_path: &Path,
    stored: &mut JsonMap<String, Value>,
    provider_id: &str,
    tenant: &str,
    team: &str,
    env: &str,
    action: &Value,
) -> anyhow::Result<Value> {
    let executor = executor(action)?;
    let config = config_mut(stored)?.clone();
    let target = match provider_http_path(tenant, team, env, &config, executor) {
        Ok(path) => path,
        Err(err) => {
            return Ok(step_result(
                action,
                false,
                &err.to_string(),
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": err.to_string(),
                }),
            ));
        }
    };
    let method = executor
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("POST");
    let Some(route_match) =
        find_declared_provider_http_route(bundle_path, method, &target, tenant, team)?
    else {
        let message = format!(
            "provider_http target {} is not declared by pack greentic.http-routes.v1",
            target
        );
        return Ok(step_result(
            action,
            false,
            &message,
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": message,
                "target": target,
                "provider_id": provider_id,
            }),
        ));
    };
    let ProviderHttpRouteTarget::SetupComponent { component_ref, op } = &route_match.route.target
    else {
        return Ok(step_result(
            action,
            false,
            "provider setup route must declare setup_component_ref/setup_op",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "provider setup route must declare setup_component_ref/setup_op",
                "target": target,
                "provider_id": provider_id,
            }),
        ));
    };
    let payload = provider_http_payload(provider_id, tenant, team, env, &config, action)?;
    let body_json = serde_json::to_string(&payload)?;
    let headers_json = serde_json::to_string(&serde_json::json!({
        "method": method,
        "path": target,
        "query": "",
    }))?;
    let request = serde_json::json!({
        "v": 1,
        "domain": "messaging",
        "provider": route_match.route.provider_id,
        "tenant": route_match.tenant,
        "team": route_match.team,
        "method": method,
        "path": target,
        "query": Vec::<(String, String)>::new(),
        "headers": headers_json,
        "body_json": body_json,
    });
    let setup_config = crate::engine::SetupConfig {
        tenant: route_match.tenant.clone(),
        team: Some(route_match.team.clone()),
        env: env.to_string(),
        offline: false,
        verbose: false,
    };
    let output = crate::engine::invoke_setup_component_operation(
        bundle_path,
        &route_match.route.pack_path,
        component_ref,
        op,
        &request,
        &setup_config,
    )?;
    let ok = output
        .get("ok")
        .and_then(Value::as_bool)
        .or_else(|| {
            output
                .get("response")
                .and_then(|response| response.get("body_json"))
                .and_then(|body| body.get("ok"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(true);
    let response = serde_json::json!({
        "ok": ok,
        "response": output,
    });
    provider_http_result_from_response(stored, action, &target, response)
}

pub fn step_result(action: &Value, ok: bool, next: &str, result: Value) -> Value {
    serde_json::json!({
        "ok": ok,
        "step": action.get("id").and_then(Value::as_str).unwrap_or_default(),
        "next": next,
        "result": result,
    })
}

pub fn update_oauth_resume(stored: &mut JsonMap<String, Value>, setup_result: &Value) {
    let Some(result) = setup_result.get("result").and_then(Value::as_object) else {
        return;
    };
    if result.get("error").and_then(Value::as_str) != Some("oauth_required") {
        return;
    }
    let Some(token_store_key) = result.get("token_store_key").and_then(Value::as_str) else {
        return;
    };
    let resume_step = result
        .get("resume_step")
        .and_then(Value::as_str)
        .or_else(|| setup_result.get("step").and_then(Value::as_str))
        .unwrap_or_default();
    stored.insert(
        "oauth_resume".to_string(),
        serde_json::json!({
            "token_store_key": token_store_key,
            "resume_step": resume_step,
        }),
    );
}

pub fn clear_oauth_resume_for_token(stored: &mut JsonMap<String, Value>, token_store_key: &str) {
    let should_clear =
        oauth_resume_token(stored).is_some_and(|stored_key| stored_key == token_store_key);
    if should_clear {
        stored.remove("oauth_resume");
    }
}

pub fn append_backend_event(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    event: Value,
) -> anyhow::Result<PathBuf> {
    let path = backend_events_path(bundle_root, tenant, team, provider_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create setup backend event dir {}", parent.display()))?;
    }
    let mut event = event;
    if let Value::Object(object) = &mut event {
        object
            .entry("timestamp_ms".to_string())
            .or_insert_with(|| Value::from(current_timestamp_ms() as u64));
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open setup backend event log {}", path.display()))?;
    serde_json::to_writer(&mut file, &event)
        .with_context(|| format!("write setup backend event {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write setup backend event newline {}", path.display()))?;
    Ok(path)
}

pub fn record_action_result(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    stored: &mut JsonMap<String, Value>,
    action_result: Value,
) -> anyhow::Result<PathBuf> {
    update_oauth_resume(stored, &action_result);
    stored.insert("last_setup_result".to_string(), action_result.clone());
    save_backend_state(bundle_root, tenant, team, provider_id, stored)?;
    append_backend_event(
        bundle_root,
        tenant,
        team,
        provider_id,
        serde_json::json!({
            "type": "action_result",
            "step": action_result.get("step").cloned().unwrap_or(Value::Null),
            "ok": action_result.get("ok").cloned().unwrap_or(Value::Null),
            "next": action_result.get("next").cloned().unwrap_or(Value::Null),
            "result": action_result.get("result").cloned().unwrap_or(Value::Null),
        }),
    )
}

pub fn blocked_from_result(setup_result: &Value) -> Option<Value> {
    let result = setup_result.get("result")?;
    if !result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let message = result
        .get("error")
        .or_else(|| setup_result.get("next"))
        .and_then(Value::as_str)
        .unwrap_or("Setup action is blocked.");
    let mut blocked = JsonMap::new();
    blocked.insert(
        "title".to_string(),
        Value::String("Setup action blocked".to_string()),
    );
    blocked.insert("summary".to_string(), Value::String(message.to_string()));
    blocked.insert("next".to_string(), Value::String(message.to_string()));
    if blocked_result_retryable(message, result) {
        blocked.insert("retryable".to_string(), Value::Bool(true));
    }
    if let Some(capability) = result.get("missing_host_capability").cloned() {
        blocked.insert("missing_host_capability".to_string(), capability);
    }
    if let Some(config_key) = result.get("missing_config_key").cloned() {
        blocked.insert("missing_config_key".to_string(), config_key);
    }
    Some(Value::Object(blocked))
}

pub fn blocked_result_retryable(message: &str, result: &Value) -> bool {
    result
        .get("retryable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || result
            .get("waiting")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || message.eq_ignore_ascii_case("runtime/tunnel not running")
        || result
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| {
                error.eq_ignore_ascii_case("runtime/tunnel not running")
                    || error.eq_ignore_ascii_case("oauth_required")
            })
}

pub fn oauth_action_by_token_store_key<'a>(
    contract: &'a Value,
    token_store_key: &str,
) -> Option<&'a Value> {
    contract
        .get("actions")
        .and_then(Value::as_array)?
        .iter()
        .find(|action| {
            let Some(executor) = action.get("executor").and_then(Value::as_object) else {
                return false;
            };
            executor.get("kind").and_then(Value::as_str) == Some("oauth_device_code")
                && executor.get("token_store_key").and_then(Value::as_str) == Some(token_store_key)
        })
}

pub fn oauth_resume_token(stored: &JsonMap<String, Value>) -> Option<&str> {
    stored
        .get("oauth_resume")?
        .get("token_store_key")?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn render_values(stored: &JsonMap<String, Value>) -> Value {
    let mut values = JsonMap::new();
    for (key, value) in stored {
        values.insert(key.clone(), value.clone());
    }
    Value::Object(values)
}

pub fn backend_state_dir(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<PathBuf> {
    Ok(bundle_root
        .join("state")
        .join("setup")
        .join(validate_path_segment(tenant, "tenant")?)
        .join(validate_path_segment(team, "team")?)
        .join(validate_path_segment(provider_id, "provider_id")?))
}

pub fn backend_state_path(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<PathBuf> {
    Ok(backend_state_dir(bundle_root, tenant, team, provider_id)?.join("backend-contract.json"))
}

pub fn backend_events_path(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<PathBuf> {
    Ok(backend_state_dir(bundle_root, tenant, team, provider_id)?.join("events.jsonl"))
}

pub fn backend_archive_dir(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<PathBuf> {
    Ok(backend_state_dir(bundle_root, tenant, team, provider_id)?.join("archive"))
}

pub fn archive_backend_state(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    reason: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let state_path = backend_state_path(bundle_root, tenant, team, provider_id)?;
    if !state_path.is_file() {
        return Ok(None);
    }
    let archive_dir = backend_archive_dir(bundle_root, tenant, team, provider_id)?;
    std::fs::create_dir_all(&archive_dir)
        .with_context(|| format!("create setup backend archive dir {}", archive_dir.display()))?;
    let archive_path = archive_dir.join(format!(
        "{}-{}.json",
        safe_archive_segment(reason),
        current_timestamp_ms()
    ));
    std::fs::copy(&state_path, &archive_path).with_context(|| {
        format!(
            "archive setup backend state {} to {}",
            state_path.display(),
            archive_path.display()
        )
    })?;
    Ok(Some(archive_path))
}

pub fn archive_backend_file(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    source_path: &Path,
    reason: &str,
) -> anyhow::Result<Option<PathBuf>> {
    if !source_path.is_file() {
        return Ok(None);
    }
    let archive_dir = backend_archive_dir(bundle_root, tenant, team, provider_id)?;
    std::fs::create_dir_all(&archive_dir)
        .with_context(|| format!("create setup backend archive dir {}", archive_dir.display()))?;
    let archive_path = archive_dir.join(format!(
        "{}-{}.json",
        safe_archive_segment(reason),
        current_timestamp_ms()
    ));
    std::fs::copy(source_path, &archive_path).with_context(|| {
        format!(
            "archive setup backend file {} to {}",
            source_path.display(),
            archive_path.display()
        )
    })?;
    Ok(Some(archive_path))
}

pub fn legacy_backend_state_path(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<PathBuf> {
    Ok(bundle_root
        .join("state")
        .join("setup-backends")
        .join(validate_path_segment(env, "env")?)
        .join(validate_path_segment(tenant, "tenant")?)
        .join(validate_path_segment(team, "team")?)
        .join(format!(
            "{}.json",
            validate_path_segment(provider_id, "provider_id")?
        )))
}

#[derive(Clone, Debug)]
pub struct BackendStateMigration {
    pub legacy_path: PathBuf,
    pub legacy_setup_actions_path: PathBuf,
    pub state_path: PathBuf,
    pub archive_path: Option<PathBuf>,
    pub setup_actions_archive_path: Option<PathBuf>,
    pub event_path: PathBuf,
    pub source: String,
    pub legacy_removed: bool,
    pub setup_actions_removed: bool,
    pub empty: bool,
}

pub fn load_legacy_backend_state(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<Option<JsonMap<String, Value>>> {
    let legacy = legacy_backend_state_path(bundle_root, env, tenant, team, provider_id)?;
    read_state_file(&legacy)
}

pub fn migrate_backend_state(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<BackendStateMigration> {
    let state_path = backend_state_path(bundle_root, tenant, team, provider_id)?;
    let legacy_path = legacy_backend_state_path(bundle_root, env, tenant, team, provider_id)?;
    let legacy_setup_actions_path =
        crate::setup_actions::setup_actions_state_path(bundle_root, tenant, team, provider_id);
    let generic = read_state_file(&state_path)?;
    let legacy = read_state_file(&legacy_path)?;
    let (stored, source) = match (generic, legacy.clone()) {
        (Some(stored), _) => (stored, "generic"),
        (None, Some(stored)) => (stored, "legacy"),
        (None, None) => (JsonMap::new(), "empty"),
    };

    save_backend_state(bundle_root, tenant, team, provider_id, &stored)?;

    let archive_path = if legacy_path.is_file() {
        archive_backend_file(
            bundle_root,
            tenant,
            team,
            provider_id,
            &legacy_path,
            "legacy-migration",
        )?
    } else {
        None
    };
    let legacy_removed = if legacy_path.is_file() {
        std::fs::remove_file(&legacy_path)
            .with_context(|| format!("remove legacy setup backend {}", legacy_path.display()))?;
        true
    } else {
        false
    };
    let setup_actions_archive_path = if legacy_setup_actions_path.is_file() {
        archive_backend_file(
            bundle_root,
            tenant,
            team,
            provider_id,
            &legacy_setup_actions_path,
            "legacy-setup-actions-migration",
        )?
    } else {
        None
    };
    let setup_actions_removed = if legacy_setup_actions_path.is_file() {
        std::fs::remove_file(&legacy_setup_actions_path).with_context(|| {
            format!(
                "remove legacy setup actions {}",
                legacy_setup_actions_path.display()
            )
        })?;
        true
    } else {
        false
    };
    let event_path = append_backend_event(
        bundle_root,
        tenant,
        team,
        provider_id,
        serde_json::json!({
            "type": "state_migrated",
            "legacy_path": legacy_path,
            "state_path": state_path,
            "archive_path": archive_path,
            "legacy_setup_actions_path": legacy_setup_actions_path,
            "setup_actions_archive_path": setup_actions_archive_path,
            "source": source,
            "legacy_removed": legacy_removed,
            "setup_actions_removed": setup_actions_removed,
            "empty": stored.is_empty(),
        }),
    )?;

    Ok(BackendStateMigration {
        legacy_path,
        legacy_setup_actions_path,
        state_path,
        archive_path,
        setup_actions_archive_path,
        event_path,
        source: source.to_string(),
        legacy_removed,
        setup_actions_removed,
        empty: stored.is_empty(),
    })
}

pub fn reset_backend_state(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    reason: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let archive_path = archive_backend_state(bundle_root, tenant, team, provider_id, reason)?;
    let state_path = backend_state_path(bundle_root, tenant, team, provider_id)?;
    match std::fs::remove_file(&state_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("remove {}", state_path.display())),
    }
    append_backend_event(
        bundle_root,
        tenant,
        team,
        provider_id,
        serde_json::json!({
            "type": "state_reset",
            "reason": reason,
            "archive_path": archive_path,
        }),
    )?;
    Ok(archive_path)
}

pub fn load_backend_state(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> anyhow::Result<JsonMap<String, Value>> {
    let path = backend_state_path(bundle_root, tenant, team, provider_id)?;
    let mut stored = read_state_file(&path)?.unwrap_or_default();
    hydrate_redacted_backend_config(bundle_root, env, tenant, team, provider_id, &mut stored)?;
    repair_redacted_display_codes(&mut stored);
    Ok(stored)
}

pub fn save_backend_state(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    stored: &JsonMap<String, Value>,
) -> anyhow::Result<PathBuf> {
    let path = backend_state_path(bundle_root, tenant, team, provider_id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create setup backend state dir {}", parent.display()))?;
    }
    let env = backend_state_env(stored);
    persist_sensitive_backend_config(bundle_root, &env, tenant, team, provider_id, stored)?;
    let redacted = redact_backend_state_for_disk(stored);
    std::fs::write(&path, serde_json::to_vec_pretty(&redacted)?)
        .with_context(|| format!("write setup backend state {}", path.display()))?;
    Ok(path)
}

fn backend_state_env(stored: &JsonMap<String, Value>) -> String {
    stored
        .get("config")
        .and_then(Value::as_object)
        .and_then(|config| config.get("env"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("dev")
        .to_string()
}

fn persist_sensitive_backend_config(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
    stored: &JsonMap<String, Value>,
) -> anyhow::Result<()> {
    let Some(config) = stored.get("config").and_then(Value::as_object) else {
        return Ok(());
    };
    let sensitive: JsonMap<String, Value> = config
        .iter()
        .filter(|(key, value)| {
            is_sensitive_backend_state_key(key) && !value_is_redacted_marker(value)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if sensitive.is_empty() {
        return Ok(());
    }
    let config = Value::Object(sensitive);
    let bundle_root = bundle_root.to_path_buf();
    let env = env.to_string();
    let tenant = tenant.to_string();
    let team = team.to_string();
    let provider_id = provider_id.to_string();
    block_on_backend_secret_task(async move {
        crate::qa::persist::persist_all_config_as_secrets(
            &bundle_root,
            &env,
            &tenant,
            Some(&team),
            &provider_id,
            &config,
            None,
        )
        .await
    })
    .map(|_| ())
}

fn hydrate_redacted_backend_config(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
    stored: &mut JsonMap<String, Value>,
) -> anyhow::Result<()> {
    let Some(config) = stored.get_mut("config").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let keys: Vec<String> = config
        .iter()
        .filter(|(key, value)| {
            is_sensitive_backend_state_key(key) && value_is_redacted_marker(value)
        })
        .map(|(key, _)| key.clone())
        .collect();
    if keys.is_empty() {
        return Ok(());
    }
    let bundle_root = bundle_root.to_path_buf();
    let env = env.to_string();
    let tenant = tenant.to_string();
    let team = team.to_string();
    let provider_id = provider_id.to_string();
    let lookup_provider_id = provider_id.clone();
    let lookup_keys = keys.clone();
    let hydrated = block_on_backend_secret_task(async move {
        use greentic_secrets_lib::SecretsStore;

        let store = crate::secrets::open_dev_store(&bundle_root)?;
        let mut values = JsonMap::new();
        for key in lookup_keys {
            let uri =
                crate::canonical_secret_uri(&env, &tenant, Some(&team), &lookup_provider_id, &key);
            if let Ok(bytes) = store.get(&uri).await
                && let Ok(text) = String::from_utf8(bytes)
                && !text.is_empty()
            {
                values.insert(key, Value::String(text));
            }
        }
        Ok::<_, anyhow::Error>(values)
    })?;
    for key in keys {
        match hydrated.get(&key) {
            Some(value) => {
                config.insert(key, value.clone());
            }
            None => {
                // The store no longer holds this secret, so the redacted
                // placeholder cannot be restored. Drop the key entirely:
                // executors treat a missing credential as "complete OAuth /
                // re-enter it, then retry", whereas leaving the placeholder in
                // place sends the literal string "[redacted:dev-secret]" to
                // the provider as a credential (observed as a Graph 401
                // "JWT is not well formed" dead end).
                eprintln!(
                    "[setup backend-state] stored secret for {provider_id}/{key} is missing \
                     from the dev store; clearing the redacted placeholder so setup re-prompts \
                     for it instead of using the placeholder as a credential"
                );
                config.remove(&key);
            }
        }
    }
    Ok(())
}

fn redact_backend_state_for_disk(stored: &JsonMap<String, Value>) -> JsonMap<String, Value> {
    stored
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                redact_backend_state_value(Some(key.as_str()), value),
            )
        })
        .collect()
}

fn redact_backend_state_value(key: Option<&str>, value: &Value) -> Value {
    if let Some(key) = key
        && is_sensitive_backend_state_key(key)
        && !value.is_null()
    {
        return Value::String(REDACTED_SECRET_MARKER.to_string());
    }
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        redact_backend_state_value(Some(key.as_str()), value),
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_backend_state_value(None, item))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn is_sensitive_backend_state_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized.ends_with("url") {
        return false;
    }
    normalized.contains("accesstoken")
        || normalized.contains("refreshtoken")
        || normalized.contains("idtoken")
        || normalized.contains("devicecode")
        || normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.ends_with("token")
}

fn value_is_redacted_marker(value: &Value) -> bool {
    value.as_str() == Some(REDACTED_SECRET_MARKER)
}

fn repair_redacted_display_codes(stored: &mut JsonMap<String, Value>) {
    for value in stored.values_mut() {
        repair_redacted_display_codes_in_value(value);
    }
}

fn repair_redacted_display_codes_in_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            let fallback_code = object
                .values()
                .filter_map(Value::as_str)
                .filter(|text| *text != REDACTED_SECRET_MARKER)
                .find_map(extract_display_code_from_text);
            for (key, child) in object.iter_mut() {
                if is_display_code_key(key) && value_is_redacted_marker(child) {
                    if let Some(code) = fallback_code.as_ref() {
                        *child = Value::String(code.clone());
                    }
                } else {
                    repair_redacted_display_codes_in_value(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                repair_redacted_display_codes_in_value(item);
            }
        }
        _ => {}
    }
}

fn is_display_code_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized == "usercode" || normalized.ends_with("usercode")
}

fn extract_display_code_from_text(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let code_at = lower.find("code")?;
    let after_code = text.get(code_at + "code".len()..)?;
    after_code
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'))
        .map(str::trim)
        .filter(|token| {
            let len = token.len();
            (4..=32).contains(&len)
                && token.chars().any(|ch| ch.is_ascii_alphabetic())
                && token.chars().any(|ch| ch.is_ascii_digit())
        })
        .map(ToString::to_string)
        .next()
}

fn block_on_backend_secret_task<F, T>(future: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("build setup backend secret runtime")?
            .block_on(future)
    })
    .join()
    .map_err(|_| anyhow!("setup backend secret task panicked"))?
}

fn read_state_file(path: &Path) -> anyhow::Result<Option<JsonMap<String, Value>>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<JsonMap<String, Value>>(&text)
            .with_context(|| format!("parse setup backend state {}", path.display()))
            .map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn current_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn safe_archive_segment(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "archive".to_string()
    } else {
        out.to_string()
    }
}

pub fn validate_path_segment<'a>(value: &'a str, name: &str) -> anyhow::Result<&'a str> {
    let value = value.trim();
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        anyhow::bail!("invalid {name}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    fn write_setup_route_pack(pack_path: &Path) -> anyhow::Result<()> {
        let file = std::fs::File::create(pack_path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "greentic.http-routes.v1": {
                        "inline": {
                            "routes": [
                                {
                                    "pattern": "/v1/setup/messaging-example/{tenant}/{team}/register",
                                    "methods": ["POST"],
                                    "setup_component_ref": "setup-component",
                                    "setup_op": "handle_http"
                                }
                            ]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("components/setup-component.json", options)?;
        writer.write_all(
            json!({
                "operations": {
                    "handle_http": {
                        "echo_request": true
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;
        Ok(())
    }

    #[test]
    fn merge_browser_config_update_ignores_server_owned_keys() {
        let contract = json!({
            "server_owned_config_keys": [
                "oauth_device_code",
                "graph_access_token"
            ]
        });
        let mut stored = JsonMap::new();
        merge_browser_config_update(
            &mut stored,
            &json!({
                "public_base_url": "https://example.test",
                "oauth_device_code": "browser-device-code",
                "graph_access_token": "browser-token"
            }),
            &contract,
            JsonMap::new(),
        )
        .unwrap();

        let config = stored.get("config").and_then(Value::as_object).unwrap();
        assert_eq!(config["public_base_url"], "https://example.test");
        assert!(config.get("oauth_device_code").is_none());
        assert!(config.get("graph_access_token").is_none());
    }

    #[test]
    fn merge_browser_config_update_rejects_echoed_ephemeral_public_base_url() {
        let contract = json!({ "server_owned_config_keys": [] });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({"public_base_url": "https://current.trycloudflare.com"}),
        );

        // A browser echo of a (stale) ephemeral tunnel URL must not revert
        // the engine-assigned value; sibling keys still merge.
        merge_browser_config_update(
            &mut stored,
            &json!({
                "public_base_url": "https://stale-snapshot.trycloudflare.com",
                "bot_display_name": "Greentic Bot"
            }),
            &contract,
            JsonMap::new(),
        )
        .unwrap();
        let config = stored.get("config").and_then(Value::as_object).unwrap();
        assert_eq!(
            config["public_base_url"], "https://current.trycloudflare.com",
            "engine-owned ephemeral URL survives a stale browser echo"
        );
        assert_eq!(config["bot_display_name"], "Greentic Bot");

        // A user-supplied STABLE https URL is a deliberate override → merges.
        merge_browser_config_update(
            &mut stored,
            &json!({"public_base_url": "https://bot.example.com"}),
            &contract,
            JsonMap::new(),
        )
        .unwrap();
        let config = stored.get("config").and_then(Value::as_object).unwrap();
        assert_eq!(config["public_base_url"], "https://bot.example.com");
    }

    #[test]
    fn completion_met_supports_exists_equals_and_boolean() {
        let values = json!({
            "oauth": {"graph": {"ok": true}},
            "last_reconcile": {"ok": true},
            "last_activity": {"id": "activity-1"}
        });

        assert!(completion_met(
            &values,
            &json!({"state_path": "last_activity", "exists": true})
        ));
        assert!(completion_met(
            &values,
            &json!({"state_path": "last_reconcile.ok", "equals": true})
        ));
        assert!(completion_met(
            &values,
            &json!({"state_path": "oauth.graph.ok"})
        ));
        assert!(!completion_met(
            &values,
            &json!({"state_path": "missing.value", "exists": true})
        ));
    }

    #[test]
    fn render_status_reports_next_pending_step() {
        let contract = json!({
            "required_order": ["auth", "register"],
            "actions": [
                {
                    "id": "auth",
                    "completion": {"state_path": "oauth.graph.ok", "equals": true}
                },
                {
                    "id": "register",
                    "completion": {"state_path": "last_reconcile.ok", "equals": true}
                }
            ]
        });
        let mut stored = JsonMap::new();
        stored.insert("oauth".to_string(), json!({"graph": {"ok": true}}));

        let status = render_status(&contract, &stored);
        assert_eq!(status["ok"], false);
        assert_eq!(status["next"], "register");
        assert_eq!(status["items"][0]["state"], "done");
        assert_eq!(status["items"][1]["state"], "pending");
    }

    #[test]
    fn render_status_prioritizes_oauth_resume_recovery() {
        let contract = json!({
            "required_order": ["auth", "register"],
            "actions": [
                {
                    "id": "auth",
                    "executor": {
                        "kind": "oauth_device_code",
                        "token_store_key": "graph_access_token"
                    },
                    "completion": {"state_path": "oauth.graph.ok", "equals": true}
                },
                {
                    "id": "register",
                    "completion": {"state_path": "last_reconcile.ok", "equals": true}
                }
            ]
        });
        let mut stored = JsonMap::new();
        stored.insert("oauth".to_string(), json!({"graph": {"ok": true}}));
        stored.insert("last_reconcile".to_string(), json!({"ok": true}));
        stored.insert(
            "oauth_resume".to_string(),
            json!({"token_store_key": "graph_access_token"}),
        );

        let status = render_status(&contract, &stored);
        assert_eq!(status["ok"], false);
        assert_eq!(status["next"], "auth");
        assert_eq!(status["items"][0]["state"], "pending");
        assert_eq!(status["items"][1]["state"], "done");
    }

    #[test]
    fn render_status_includes_retryable_blocked_details() {
        let contract = json!({
            "required_order": ["observe"],
            "actions": [{
                "id": "observe",
                "completion": {"state_path": "last_activity", "exists": true}
            }]
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "last_setup_result".to_string(),
            step_result(
                &json!({"id": "observe"}),
                false,
                "runtime/tunnel not running",
                json!({
                    "ok": false,
                    "blocked": true,
                    "error": "runtime/tunnel not running"
                }),
            ),
        );

        let status = render_status(&contract, &stored);

        assert_eq!(status["ok"], false);
        assert_eq!(status["blocked"]["retryable"], true);
        assert_eq!(status["blocked"]["summary"], "runtime/tunnel not running");
    }

    #[test]
    fn next_action_id_returns_recovery_or_first_pending_step() {
        let contract = json!({
            "required_order": ["auth", "register"],
            "actions": [
                {
                    "id": "auth",
                    "executor": {
                        "kind": "oauth_device_code",
                        "token_store_key": "graph_access_token"
                    },
                    "completion": {"state_path": "oauth.graph.ok", "equals": true}
                },
                {
                    "id": "register",
                    "completion": {"state_path": "last_reconcile.ok", "equals": true}
                }
            ]
        });
        let mut stored = JsonMap::new();
        stored.insert("oauth".to_string(), json!({"graph": {"ok": true}}));
        assert_eq!(
            next_action_id(&contract, &stored).as_deref(),
            Some("register")
        );

        stored.insert(
            "oauth_resume".to_string(),
            json!({"token_store_key": "graph_access_token"}),
        );
        assert_eq!(next_action_id(&contract, &stored).as_deref(), Some("auth"));
    }

    #[test]
    fn record_action_result_saves_state_and_appends_jsonl_event() {
        let temp = tempfile::tempdir().unwrap();
        let action = json!({"id": "register"});
        let result = step_result(
            &action,
            false,
            "provider setup service is not running",
            json!({"ok": false, "blocked": true}),
        );
        let mut stored = JsonMap::new();

        let event_path = record_action_result(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
            &mut stored,
            result,
        )
        .unwrap();

        let state =
            load_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams").unwrap();
        assert_eq!(state["last_setup_result"]["step"], "register");
        let events = std::fs::read_to_string(event_path).unwrap();
        assert!(events.contains("\"type\":\"action_result\""));
        assert!(events.contains("\"step\":\"register\""));
    }

    #[test]
    fn archive_file_copies_legacy_artifact_into_generic_archive() {
        let temp = tempfile::tempdir().unwrap();
        let legacy =
            legacy_backend_state_path(temp.path(), "dev", "demo", "default", "messaging-teams")
                .unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"config":{"bot_app_id":"legacy"}}"#).unwrap();

        let archive = archive_backend_file(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
            &legacy,
            "legacy migration",
        )
        .unwrap()
        .expect("archive");

        assert!(archive.is_file());
        assert!(
            archive
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("legacy-migration-")
        );
        assert_eq!(
            std::fs::read_to_string(archive).unwrap(),
            r#"{"config":{"bot_app_id":"legacy"}}"#
        );
    }

    #[test]
    fn archive_and_reset_backend_state_preserve_prior_state() {
        let temp = tempfile::tempdir().unwrap();
        let mut stored = JsonMap::new();
        stored.insert("config".to_string(), json!({"bot_app_id": "old"}));
        save_backend_state(temp.path(), "demo", "default", "messaging-teams", &stored).unwrap();

        let archive = reset_backend_state(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
            "manual reset",
        )
        .unwrap()
        .expect("archive");

        assert!(archive.is_file());
        assert!(
            archive
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("manual-reset-")
        );
        assert_eq!(
            load_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams")
                .unwrap()
                .len(),
            0
        );
        let events = std::fs::read_to_string(
            backend_events_path(temp.path(), "demo", "default", "messaging-teams").unwrap(),
        )
        .unwrap();
        assert!(events.contains("\"type\":\"state_reset\""));
    }

    #[test]
    fn blocked_from_result_marks_runtime_and_oauth_as_retryable() {
        let runtime = step_result(
            &json!({"id": "observe"}),
            false,
            "runtime/tunnel not running",
            json!({
                "ok": false,
                "blocked": true,
                "error": "runtime/tunnel not running"
            }),
        );
        let blocked = blocked_from_result(&runtime).unwrap();
        assert_eq!(blocked["retryable"], true);
        assert_eq!(blocked["summary"], "runtime/tunnel not running");

        let oauth = step_result(
            &json!({"id": "publish"}),
            false,
            "reauthorize",
            json!({
                "ok": false,
                "blocked": true,
                "error": "oauth_required",
                "token_store_key": "graph_access_token"
            }),
        );
        assert_eq!(blocked_from_result(&oauth).unwrap()["retryable"], true);
    }

    #[test]
    fn oauth_required_result_treats_invalid_access_token_as_reauth() {
        let action = json!({"id": "discover"});
        let result = oauth_required_result(
            &action,
            "access_token",
            "authenticated request failed",
            &json!({
                "ok": false,
                "status": 401,
                "body": {
                    "error": "Azure subscription discovery failed (HTTP 401): The access token is invalid."
                }
            }),
        )
        .expect("invalid token should require OAuth");

        assert_eq!(result["ok"], false);
        assert_eq!(result["result"]["error"], "oauth_required");
        assert_eq!(result["result"]["token_store_key"], "access_token");
        assert_eq!(blocked_from_result(&result).unwrap()["retryable"], true);
    }

    #[test]
    fn oauth_required_result_treats_graph_invalid_authentication_token_as_reauth() {
        // Verbatim shape of a Microsoft Graph 401 when the bearer token is not
        // a JWT (e.g. a redaction placeholder): CamelCase code, no spaced
        // "invalid token"/"expired" phrase anywhere in the body.
        let action = json!({"id": "bot_app_identity"});
        let result = oauth_required_result(
            &action,
            "graph_access_token",
            "authenticated request failed",
            &json!({
                "ok": false,
                "status": 401,
                "body": {
                    "error": {
                        "code": "InvalidAuthenticationToken",
                        "message": "IDX14100: JWT is not well formed, there are no dots (.).\nThe token needs to be in JWS or JWE Compact Serialization Format. (JWS): 'EncodedHeader.EncodedPayload.EncodedSignature'."
                    }
                }
            }),
        )
        .expect("Graph InvalidAuthenticationToken must route to re-auth, not a dead end");

        assert_eq!(result["result"]["error"], "oauth_required");
        assert_eq!(result["result"]["token_store_key"], "graph_access_token");
        assert_eq!(blocked_from_result(&result).unwrap()["retryable"], true);
    }

    #[test]
    fn runtime_observation_waits_until_state_store_key_exists() -> anyhow::Result<()> {
        let action = json!({
            "id": "observe",
            "executor": {
                "kind": "runtime_observation",
                "source": "runtime",
                "event": "ready",
                "state_store_key": "last_runtime_ready"
            }
        });
        let mut stored = JsonMap::new();

        let waiting = execute_runtime_observation(&mut stored, &action)?;

        assert_eq!(waiting["ok"], false);
        assert_eq!(waiting["result"]["waiting"], true);
        assert_eq!(waiting["result"]["retryable"], true);
        assert_eq!(waiting["result"]["state_store_key"], "last_runtime_ready");
        assert!(blocked_from_result(&waiting).is_some());

        stored.insert(
            "last_runtime_ready".to_string(),
            json!({"ok": true, "runtime_context": {"public_base_url": "https://example.test"}}),
        );
        let complete = execute_runtime_observation(&mut stored, &action)?;

        assert_eq!(complete["ok"], true);
        assert_eq!(complete["result"]["state_store_key"], "last_runtime_ready");
        assert_eq!(complete["result"]["observed"]["ok"], true);
        Ok(())
    }

    #[test]
    fn graph_application_waits_for_oauth_token_without_losing_state() -> anyhow::Result<()> {
        let action = json!({
            "id": "register_app",
            "executor": {
                "kind": "microsoft_graph_application",
                "graph_token_store_key": "graph_access_token",
                "app_id_config_key": "bot_app_id",
                "client_secret_config_key": "bot_client_secret",
                "display_name_config_key": "bot_display_name"
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "bot_display_name": "Demo Bot"
            }),
        );

        let blocked = execute_microsoft_graph_application(&mut stored, "messaging-teams", &action)?;

        assert_eq!(blocked["ok"], false);
        assert_eq!(
            blocked["result"]["missing_token_store_key"],
            "graph_access_token"
        );
        assert_eq!(blocked["result"]["retryable"], true);
        assert_eq!(
            stored
                .get("config")
                .and_then(Value::as_object)
                .and_then(|config| config.get("bot_display_name"))
                .and_then(Value::as_str),
            Some("Demo Bot")
        );
        assert!(stored.get("last_app_registration").is_none());
        Ok(())
    }

    #[test]
    fn teams_app_catalog_publish_waits_for_oauth_token_before_packaging() -> anyhow::Result<()> {
        let action = json!({
            "id": "publish_app",
            "executor": {
                "kind": "microsoft_graph_teams_app_catalog_publish",
                "graph_token_store_key": "graph_access_token",
                "teams_app_id_config_key": "teams_app_id",
                "teams_app_version_config_key": "teams_app_version",
                "bot_app_id_config_key": "bot_app_id",
                "manifest_template_asset": "assets/teams-app/manifest.json"
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "bot_app_id": "bot-123"
            }),
        );

        let blocked = execute_microsoft_graph_teams_app_catalog_publish(
            Path::new("unused.gtpack"),
            &mut stored,
            "demo",
            "support",
            "dev",
            &action,
        )?;

        assert_eq!(blocked["ok"], false);
        assert_eq!(
            blocked["result"]["missing_token_store_key"],
            "graph_access_token"
        );
        assert_eq!(blocked["result"]["retryable"], true);
        assert!(stored.get("last_teams_app_publish").is_none());
        Ok(())
    }

    #[test]
    fn teams_app_user_install_can_continue_with_manual_links_without_graph_token()
    -> anyhow::Result<()> {
        let action = json!({
            "id": "install_app",
            "executor": {
                "kind": "microsoft_graph_teams_app_user_install",
                "graph_token_store_key": "graph_access_token",
                "links": {
                    "add_to_teams_url_template": "https://teams.microsoft.com/l/app/{catalog_app_id}?tenant={tenant}",
                    "open_bot_chat_url_template": "https://teams.microsoft.com/l/chat/0/0?users=28:{bot_app_id}&team={team}&env={env}"
                }
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "catalog_app_id": "catalog-123",
                "bot_app_id": "bot-123"
            }),
        );
        stored.insert(
            "last_teams_app_publish".to_string(),
            json!({
                "ok": true,
                "catalog_app_id": "catalog-123"
            }),
        );

        let result = execute_microsoft_graph_teams_app_user_install(
            &mut stored,
            "demo",
            "support",
            "dev",
            &action,
        )?;

        assert_eq!(result["ok"], true);
        assert_eq!(result["result"]["action"], "manual_unverified");
        assert_eq!(result["result"]["catalog_app_id"], "catalog-123");
        assert_eq!(
            result["result"]["add_to_teams_url"],
            "https://teams.microsoft.com/l/app/catalog-123?tenant=demo"
        );
        assert_eq!(
            result["result"]["open_bot_chat_url"],
            "https://teams.microsoft.com/l/chat/0/0?users=28:bot-123&team=support&env=dev"
        );
        assert_eq!(
            stored
                .get("last_teams_app_install")
                .and_then(|value| value.get("action"))
                .and_then(Value::as_str),
            Some("manual_unverified")
        );
        Ok(())
    }

    #[test]
    fn oauth_device_start_with_response_persists_private_state_and_redacts_public_result() {
        let action = json!({
            "id": "graph_auth",
            "executor": {
                "kind": "oauth_device_code",
                "authority_url_template": "https://login.microsoftonline.com/{authority_tenant}",
                "authority_tenant_default": "organizations",
                "client_id_config_key": "graph_setup_client_id",
                "client_id_default": "default-client",
                "scopes": ["User.Read"],
                "oauth_kind": "graph",
                "token_store_key": "graph_access_token",
                "device_code_store_key": "oauth_device_code",
                "user_code_store_key": "oauth_user_code"
            }
        });
        let mut stored = JsonMap::new();
        let result = execute_oauth_device_code_start_with_response(
            &mut stored,
            &action,
            &json!({
                "device_code": "private-device-code",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://microsoft.com/devicelogin",
                "verification_uri_complete": "https://microsoft.com/devicelogin?code=ABCD-EFGH",
                "expires_in": 900,
                "interval": 5
            }),
        )
        .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["result"]["pending_device_login"], true);
        assert_eq!(stored["config"]["oauth_device_code"], "private-device-code");
        assert_eq!(stored["config"]["oauth_user_code"], "ABCD-EFGH");
        assert_eq!(
            stored["config"]["oauth_token_url"],
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/token"
        );
        assert_eq!(stored["last_oauth"]["kind"], "graph");
        assert!(result["result"]["body"].get("device_code").is_none());
        assert!(
            stored["last_oauth"]["response"]
                .get("device_code")
                .is_none()
        );
    }

    #[test]
    fn oauth_device_complete_with_response_keeps_pending_login_resumable() {
        let action = json!({
            "id": "graph_auth",
            "executor": {
                "kind": "oauth_device_code",
                "oauth_kind": "graph",
                "token_store_key": "graph_access_token",
                "device_code_store_key": "oauth_device_code",
                "user_code_store_key": "oauth_user_code"
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "oauth_device_code": "private-device-code",
                "oauth_user_code": "OLD-CODE",
                "oauth_client_id": "client-id",
                "oauth_token_url": "https://login.example/token"
            }),
        );

        let result = execute_oauth_device_code_complete_with_response(
            &mut stored,
            &action,
            400,
            &json!({"error": "authorization_pending"}),
        )
        .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["next"], "authorization is still pending");
        assert_eq!(stored["config"]["oauth_device_code"], "private-device-code");
        assert!(stored.get("oauth").is_none());
    }

    #[test]
    fn oauth_device_complete_with_response_persists_token_and_clears_resume_state() {
        let action = json!({
            "id": "graph_auth",
            "executor": {
                "kind": "oauth_device_code",
                "oauth_kind": "graph",
                "token_store_key": "graph_access_token",
                "device_code_store_key": "oauth_device_code"
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "oauth_device_code": "private-device-code",
                "oauth_client_id": "client-id",
                "oauth_token_url": "https://login.example/token"
            }),
        );
        stored.insert(
            "oauth_resume".to_string(),
            json!({
                "token_store_key": "graph_access_token",
                "resume_step": "publish"
            }),
        );

        let result = execute_oauth_device_code_complete_with_response(
            &mut stored,
            &action,
            200,
            &json!({"access_token": "private-access-token"}),
        )
        .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(
            stored["config"]["graph_access_token"],
            "private-access-token"
        );
        assert!(stored["config"].get("oauth_device_code").is_none());
        assert!(stored["config"].get("oauth_user_code").is_none());
        assert!(stored.get("oauth_resume").is_none());
        assert_eq!(stored["oauth"]["graph"]["ok"], true);
        assert_eq!(
            stored["oauth"]["graph"]["token_store_key"],
            "graph_access_token"
        );
    }

    #[test]
    fn provider_http_external_posts_expanded_body_and_stores_result() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut data = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                match std::io::Read::read(&mut stream, &mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        data.extend_from_slice(&buffer[..n]);
                        if let Some(header_end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&data[..header_end]);
                            let content_length = headers
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("Content-Length:")
                                        .or_else(|| line.strip_prefix("content-length:"))
                                })
                                .and_then(|value| value.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if data.len() >= header_end + 4 + content_length {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let request = String::from_utf8_lossy(&data).to_string();
            tx.send(request).unwrap();
            let response = r#"{"ok":true,"registered":true}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            std::io::Write::write_all(&mut stream, wire.as_bytes()).unwrap();
        });
        let action = json!({
            "id": "register",
            "executor": {
                "kind": "provider_http",
                "url_template": format!("http://{addr}/setup/{{tenant}}/{{team}}"),
                "body": {
                    "provider_id": "messaging-example",
                    "tenant": "{tenant}",
                    "team": "{team}",
                    "endpoint": "{public_base_url}/ingress"
                },
                "state_store_key": "last_register"
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "public_base_url": "https://runtime.example.com/"
            }),
        );

        let result = execute_provider_http_external(
            &mut stored,
            "messaging-example",
            "demo",
            "support",
            "dev",
            &action,
        )
        .unwrap();

        let request = rx.recv().unwrap();
        server.join().unwrap();
        assert!(request.starts_with("POST /setup/demo/support HTTP/1.1"));
        assert!(request.contains("runtime.example.com"));
        assert!(request.contains("ingress"));
        assert_eq!(result["ok"], true);
        assert_eq!(stored["last_register"]["ok"], true);
        assert_eq!(
            stored["last_register"]["response"]["body"]["registered"],
            true
        );
    }

    #[test]
    fn provider_http_local_route_invokes_declared_setup_component_and_stores_result() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        let providers = bundle.join("providers").join("messaging");
        std::fs::create_dir_all(&providers).unwrap();
        write_setup_route_pack(&providers.join("messaging-example.gtpack")).unwrap();

        let action = json!({
            "id": "register",
            "executor": {
                "kind": "provider_http",
                "path_template": "/v1/setup/messaging-example/{tenant}/{team}/register",
                "body": {
                    "tenant": "{tenant}",
                    "team": "{team}",
                    "endpoint": "{public_base_url}/ingress"
                },
                "state_store_key": "last_register"
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "public_base_url": "https://runtime.example.com/"
            }),
        );

        let result = execute_provider_http_local_route(
            &bundle,
            &mut stored,
            "messaging-example",
            "demo",
            "support",
            "dev",
            &action,
        )
        .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(stored["last_register"]["ok"], true);
        assert_eq!(
            stored["last_register"]["target"],
            "/v1/setup/messaging-example/demo/support/register"
        );
        assert_eq!(
            stored["last_register"]["response"]["response"]["path"],
            "/v1/setup/messaging-example/demo/support/register"
        );
        assert_eq!(
            serde_json::from_str::<Value>(
                stored["last_register"]["response"]["response"]["body_json"]
                    .as_str()
                    .unwrap()
            )
            .unwrap()["endpoint"],
            "https://runtime.example.com/ingress"
        );
    }

    #[test]
    fn backend_state_uses_generic_path_without_legacy_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let legacy =
            legacy_backend_state_path(temp.path(), "dev", "demo", "default", "messaging-teams")
                .unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"config":{"bot_app_id":"old"}}"#).unwrap();

        let loaded =
            load_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams").unwrap();
        assert!(loaded.is_empty());
        assert_eq!(
            load_legacy_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams")
                .unwrap()
                .unwrap()["config"]["bot_app_id"],
            "old"
        );

        let mut generic = JsonMap::new();
        generic.insert("config".to_string(), json!({"bot_app_id": "new"}));
        save_backend_state(temp.path(), "demo", "default", "messaging-teams", &generic).unwrap();
        let new_path =
            backend_state_path(temp.path(), "demo", "default", "messaging-teams").unwrap();
        assert!(new_path.is_file());
        let loaded =
            load_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams").unwrap();
        assert_eq!(loaded["config"]["bot_app_id"], "new");
        assert_eq!(
            new_path.strip_prefix(temp.path()).unwrap(),
            Path::new("state/setup/demo/default/messaging-teams/backend-contract.json")
        );
    }

    #[test]
    fn backend_state_redacts_secret_config_on_disk_and_hydrates_on_load() {
        let temp = tempfile::tempdir().unwrap();
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "env": "dev",
                "service_access_token": "access-secret",
                "client_password": "password-secret",
                "oauth_token_url": "https://login.example/token",
                "public_base_url": "https://runtime.example.com"
            }),
        );
        stored.insert(
            "last_oauth".to_string(),
            json!({
                "response": {
                    "user_code": "SECRET-CODE",
                    "verification_uri": "https://login.example/device"
                }
            }),
        );

        let path = save_backend_state(temp.path(), "demo", "default", "messaging-generic", &stored)
            .expect("save state");
        let raw = std::fs::read_to_string(&path).expect("read state");

        assert!(!raw.contains("access-secret"));
        assert!(!raw.contains("password-secret"));
        assert!(raw.contains("SECRET-CODE"));
        assert!(raw.contains(REDACTED_SECRET_MARKER));
        assert!(raw.contains("https://login.example/token"));
        assert!(raw.contains("https://runtime.example.com"));

        let loaded = load_backend_state(temp.path(), "dev", "demo", "default", "messaging-generic")
            .expect("load state");
        assert_eq!(loaded["config"]["service_access_token"], "access-secret");
        assert_eq!(loaded["config"]["client_password"], "password-secret");
        assert_eq!(
            loaded["config"]["oauth_token_url"],
            "https://login.example/token"
        );
        assert_eq!(loaded["last_oauth"]["response"]["user_code"], "SECRET-CODE");
    }

    #[test]
    fn backend_state_drops_redacted_keys_missing_from_store_on_load() {
        // A secret can vanish from the dev store after the state file was
        // written (e.g. a concurrent whole-file rewrite losing the update).
        // The load must then DROP the redacted key — an executor treats a
        // missing credential as "complete OAuth, then retry" — rather than
        // leave the "[redacted:dev-secret]" placeholder to be sent to a
        // provider as a live credential.
        let temp = tempfile::tempdir().unwrap();
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "env": "dev",
                "graph_access_token": "real-token",
                "oauth_token_url": "https://login.example/token"
            }),
        );
        save_backend_state(temp.path(), "demo", "default", "messaging-teams", &stored)
            .expect("save state");

        // Simulate the lost update: wipe the dev store the save persisted to.
        let store_path = crate::secrets::ensure_path(temp.path()).expect("store path");
        std::fs::write(&store_path, "").expect("wipe dev store");

        let loaded = load_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams")
            .expect("load state");
        assert!(
            loaded["config"].get("graph_access_token").is_none(),
            "unhydratable redacted key must be dropped, got {:?}",
            loaded["config"].get("graph_access_token")
        );
        assert_eq!(
            loaded["config"]["oauth_token_url"],
            "https://login.example/token"
        );
    }

    #[test]
    fn backend_state_repairs_redacted_display_code_from_device_login_message() {
        let temp = tempfile::tempdir().unwrap();
        let path = backend_state_path(temp.path(), "demo", "default", "messaging-generic")
            .expect("state path");
        std::fs::create_dir_all(path.parent().unwrap()).expect("state dir");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "config": {
                    "env": "dev"
                },
                "last_oauth": {
                    "response": {
                        "message": "Open the verification page and enter the code LKZE33H3X to continue.",
                        "user_code": REDACTED_SECRET_MARKER,
                        "verification_uri": "https://login.example/device"
                    }
                }
            }))
            .unwrap(),
        )
        .expect("write state");

        let loaded = load_backend_state(temp.path(), "dev", "demo", "default", "messaging-generic")
            .expect("load state");
        assert_eq!(loaded["last_oauth"]["response"]["user_code"], "LKZE33H3X");
    }

    #[test]
    fn migrate_backend_state_archives_and_removes_legacy_state() {
        let temp = tempfile::tempdir().unwrap();
        let legacy =
            legacy_backend_state_path(temp.path(), "dev", "demo", "default", "messaging-teams")
                .unwrap();
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"config":{"bot_app_id":"old"}}"#).unwrap();
        let legacy_actions = crate::setup_actions::setup_actions_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
        );
        std::fs::create_dir_all(legacy_actions.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy_actions,
            r#"{"provider_id":"messaging-teams","tenant":"demo","team":"default","actions":[]}"#,
        )
        .unwrap();

        let migration =
            migrate_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams")
                .unwrap();

        assert_eq!(migration.source, "legacy");
        assert!(migration.legacy_removed);
        assert!(migration.setup_actions_removed);
        assert!(!legacy.exists());
        assert!(!legacy_actions.exists());
        assert!(migration.archive_path.unwrap().is_file());
        assert!(migration.setup_actions_archive_path.unwrap().is_file());
        let loaded =
            load_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams").unwrap();
        assert_eq!(loaded["config"]["bot_app_id"], "old");

        let repeated =
            migrate_backend_state(temp.path(), "dev", "demo", "default", "messaging-teams")
                .unwrap();
        assert_eq!(repeated.source, "generic");
        assert!(!repeated.legacy_removed);
        assert!(!repeated.setup_actions_removed);
        assert!(repeated.archive_path.is_none());
        assert!(repeated.setup_actions_archive_path.is_none());
    }
}
