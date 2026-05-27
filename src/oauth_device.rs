//! Provider-agnostic OAuth device-code setup helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

use crate::setup_actions::{SetupActionKind, SetupActionStatus};

pub const DEFAULT_EXTENSION_KEY: &str = "messaging.oauth_device_code.v1";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthDeviceMetadata {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub tenant_alias: Option<String>,
    pub device_code_url: String,
    pub token_url: String,
    #[serde(default)]
    pub verification_uri: Option<String>,
    #[serde(default = "default_client_id_config_key")]
    pub client_id_config_key: String,
    #[serde(default)]
    pub client_id_secret_key: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub secrets_out: BTreeMap<String, String>,
    #[serde(default)]
    pub config_out: BTreeMap<String, String>,
    #[serde(default)]
    pub post_login_discovery: Vec<DiscoveryStep>,
    #[serde(default)]
    pub error_checklist: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryStep {
    pub id: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub url_template: Option<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub save: BTreeMap<String, String>,
    #[serde(default)]
    pub select: Option<DiscoverySelect>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverySelect {
    pub from: String,
    pub label: String,
    pub value: String,
    pub save_as: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthDeviceStartInput {
    pub provider_id: String,
    pub tenant: String,
    #[serde(default)]
    pub team: Option<String>,
    pub action_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthDevicePollInput {
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthDeviceStartReport {
    pub session_id: String,
    pub provider_id: String,
    pub tenant: String,
    pub team: String,
    pub action_id: String,
    pub verification_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_uri_complete: Option<String>,
    pub user_code: String,
    pub expires_at: u64,
    pub interval: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthDevicePollReport {
    pub status: OAuthDevicePollStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persisted_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthDevicePollStatus {
    Pending,
    SlowDown,
    Complete,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OAuthDeviceSessionState {
    session_id: String,
    provider_id: String,
    tenant: String,
    team: String,
    action_id: String,
    device_code: String,
    client_id: String,
    interval: u64,
    expires_at: u64,
    created_at: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(default)]
    verification_uri: Option<String>,
    #[serde(default)]
    verification_url: Option<String>,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

pub fn load_provider_device_metadata(
    bundle_root: &Path,
    provider_id: &str,
    extension_key: &str,
) -> Result<OAuthDeviceMetadata> {
    let discovered = crate::discovery::discover(bundle_root)
        .context("failed to discover providers for OAuth device-code setup")?;
    let provider = discovered
        .find_setup_target(provider_id)
        .ok_or_else(|| anyhow!("provider not found for OAuth device-code setup: {provider_id}"))?;
    let raw = crate::discovery::read_pack_extension(&provider.pack_path, extension_key)?
        .ok_or_else(|| anyhow!("provider missing OAuth device-code metadata: {extension_key}"))?;
    serde_json::from_value(raw).context("failed to parse provider OAuth device-code metadata")
}

pub fn device_code_request_form<'a>(
    metadata: &'a OAuthDeviceMetadata,
    client_id: &'a str,
) -> Vec<(&'a str, String)> {
    vec![
        ("client_id", client_id.to_string()),
        ("scope", metadata.scopes.join(" ")),
    ]
}

pub fn token_poll_request_form<'a>(
    client_id: &'a str,
    device_code: &'a str,
) -> Vec<(&'a str, String)> {
    vec![
        ("client_id", client_id.to_string()),
        (
            "grant_type",
            "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        ),
        ("device_code", device_code.to_string()),
    ]
}

pub fn start_oauth_device_code(
    bundle_root: &Path,
    input: &OAuthDeviceStartInput,
    extension_key: &str,
) -> Result<OAuthDeviceStartReport> {
    let team = team_segment(input.team.as_deref()).to_string();
    let action = crate::setup_actions::load_setup_action(
        bundle_root,
        &input.tenant,
        &team,
        &input.provider_id,
        &input.action_id,
    )?
    .ok_or_else(|| anyhow!("setup action not found: {}", input.action_id))?;
    if action.kind != SetupActionKind::OauthDeviceCode {
        bail!("setup action is not oauth_device_code");
    }
    if action.status != SetupActionStatus::Pending {
        bail!("setup action is not pending");
    }

    let metadata = load_provider_device_metadata(bundle_root, &input.provider_id, extension_key)?;
    let setup_answers = load_provider_setup_answers(bundle_root, &input.provider_id)?;
    let client_id = lookup_client_id(&metadata, &setup_answers)?;
    let request_form = device_code_request_form(&metadata, &client_id);
    let mut response = ureq::post(&metadata.device_code_url)
        .send_form(request_form)
        .context("OAuth device-code request failed")?;
    let response = response
        .body_mut()
        .read_json::<Value>()
        .context("failed to parse OAuth device-code response")?;
    start_oauth_device_code_with_response(bundle_root, input, &metadata, &client_id, &response)
}

pub fn start_oauth_device_code_with_response(
    bundle_root: &Path,
    input: &OAuthDeviceStartInput,
    metadata: &OAuthDeviceMetadata,
    client_id: &str,
    response: &Value,
) -> Result<OAuthDeviceStartReport> {
    let parsed: DeviceCodeResponse =
        serde_json::from_value(response.clone()).context("invalid OAuth device-code response")?;
    if parsed.device_code.trim().is_empty() {
        bail!("OAuth device-code response missing device_code");
    }
    let verification_uri = parsed
        .verification_uri
        .or(parsed.verification_url)
        .or_else(|| metadata.verification_uri.clone())
        .ok_or_else(|| anyhow!("OAuth device-code response missing verification URI"))?;
    let now = crate::setup_actions::current_epoch_secs();
    let expires_in = parsed.expires_in.unwrap_or(900);
    let interval = parsed.interval.unwrap_or(5).max(1);
    let session_id = new_session_id();
    let team = team_segment(input.team.as_deref()).to_string();
    let state = OAuthDeviceSessionState {
        session_id: session_id.clone(),
        provider_id: input.provider_id.clone(),
        tenant: input.tenant.clone(),
        team: team.clone(),
        action_id: input.action_id.clone(),
        device_code: parsed.device_code,
        client_id: client_id.to_string(),
        interval,
        expires_at: now + expires_in,
        created_at: now,
    };
    save_session(bundle_root, &state)?;
    Ok(OAuthDeviceStartReport {
        session_id,
        provider_id: input.provider_id.clone(),
        tenant: input.tenant.clone(),
        team,
        action_id: input.action_id.clone(),
        verification_uri,
        verification_uri_complete: parsed.verification_uri_complete,
        user_code: parsed.user_code,
        expires_at: now + expires_in,
        interval,
        checklist: metadata.error_checklist.clone(),
    })
}

pub async fn poll_oauth_device_code(
    bundle_root: &Path,
    env: &str,
    input: &OAuthDevicePollInput,
    extension_key: &str,
) -> Result<OAuthDevicePollReport> {
    let session = load_session(bundle_root, &input.session_id)?;
    if crate::setup_actions::current_epoch_secs() >= session.expires_at {
        return Ok(OAuthDevicePollReport {
            status: OAuthDevicePollStatus::Failed,
            message: Some("OAuth device code has expired; start the login again.".to_string()),
            persisted_keys: Vec::new(),
            checklist: Vec::new(),
            interval: None,
        });
    }
    let metadata = load_provider_device_metadata(bundle_root, &session.provider_id, extension_key)?;
    let request_form = token_poll_request_form(&session.client_id, &session.device_code);
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut response = agent
        .post(&metadata.token_url)
        .send_form(request_form)
        .context("OAuth device-code token polling failed")?;
    let response = response
        .body_mut()
        .read_json::<Value>()
        .context("failed to parse OAuth device-code token response")?;
    poll_oauth_device_code_with_token_response(bundle_root, env, &session, &metadata, &response)
        .await
}

async fn poll_oauth_device_code_with_token_response(
    bundle_root: &Path,
    env: &str,
    session: &OAuthDeviceSessionState,
    metadata: &OAuthDeviceMetadata,
    response: &Value,
) -> Result<OAuthDevicePollReport> {
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        return handle_poll_error(bundle_root, session, metadata, error, response);
    }

    let mut mapped = map_device_token_response(metadata, &session.client_id, response)?;
    if !metadata.post_login_discovery.is_empty() {
        let access_token = response
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("OAuth device-code discovery requires access_token"))?;
        let discovered = execute_post_login_discovery(metadata, access_token)?;
        mapped.extend(discovered);
    }
    let config = Value::Object(
        mapped
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<JsonMap<_, _>>(),
    );
    crate::qa::persist::persist_all_config_as_secrets(
        bundle_root,
        env,
        &session.tenant,
        Some(&session.team),
        &session.provider_id,
        &config,
        None,
    )
    .await?;
    crate::setup_actions::mark_setup_action_complete(
        bundle_root,
        &session.tenant,
        &session.team,
        &session.provider_id,
        &session.action_id,
    )?;
    let _ = std::fs::remove_file(session_path(bundle_root, &session.session_id));

    Ok(OAuthDevicePollReport {
        status: OAuthDevicePollStatus::Complete,
        message: None,
        persisted_keys: mapped.keys().cloned().collect(),
        checklist: Vec::new(),
        interval: None,
    })
}

fn handle_poll_error(
    bundle_root: &Path,
    session: &OAuthDeviceSessionState,
    metadata: &OAuthDeviceMetadata,
    error: &str,
    response: &Value,
) -> Result<OAuthDevicePollReport> {
    match error {
        "authorization_pending" => Ok(OAuthDevicePollReport {
            status: OAuthDevicePollStatus::Pending,
            message: response
                .get("error_description")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            persisted_keys: Vec::new(),
            checklist: Vec::new(),
            interval: Some(session.interval),
        }),
        "slow_down" => {
            let mut updated = session.clone();
            updated.interval = updated.interval.saturating_add(5).max(1);
            save_session(bundle_root, &updated)?;
            Ok(OAuthDevicePollReport {
                status: OAuthDevicePollStatus::SlowDown,
                message: response
                    .get("error_description")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                persisted_keys: Vec::new(),
                checklist: Vec::new(),
                interval: Some(updated.interval),
            })
        }
        "expired_token" | "authorization_declined" | "bad_verification_code" => {
            Ok(OAuthDevicePollReport {
                status: OAuthDevicePollStatus::Failed,
                message: Some(poll_error_message(error, response)),
                persisted_keys: Vec::new(),
                checklist: metadata.error_checklist.clone(),
                interval: None,
            })
        }
        other => Ok(OAuthDevicePollReport {
            status: OAuthDevicePollStatus::Failed,
            message: Some(poll_error_message(other, response)),
            persisted_keys: Vec::new(),
            checklist: metadata.error_checklist.clone(),
            interval: None,
        }),
    }
}

pub fn map_device_token_response(
    metadata: &OAuthDeviceMetadata,
    client_id: &str,
    response: &Value,
) -> Result<BTreeMap<String, String>> {
    let mut mapped = BTreeMap::new();
    for (response_key, output_key) in &metadata.secrets_out {
        let value = if response_key == "client_id" {
            Some(client_id.to_string())
        } else {
            response.get(response_key).and_then(value_to_string)
        };
        if let Some(value) = value {
            mapped.insert(output_key.clone(), value);
        }
    }
    for (response_key, output_key) in &metadata.config_out {
        let value = if response_key == "client_id" {
            Some(client_id.to_string())
        } else {
            response.get(response_key).and_then(value_to_string)
        };
        if let Some(value) = value {
            mapped.insert(output_key.clone(), value);
        }
    }
    if mapped.is_empty() {
        bail!("OAuth device-code token response did not contain mappable values");
    }
    Ok(mapped)
}

pub fn execute_post_login_discovery(
    metadata: &OAuthDeviceMetadata,
    access_token: &str,
) -> Result<BTreeMap<String, String>> {
    let mut responses = BTreeMap::new();
    let mut context = BTreeMap::new();
    for step in &metadata.post_login_discovery {
        let url = resolve_discovery_url(step, &context)?;
        let mut response = ureq::get(&url)
            .header("Authorization", &format!("Bearer {access_token}"))
            .call()
            .with_context(|| format!("OAuth device-code discovery request failed: {}", step.id))?;
        let json = response
            .body_mut()
            .read_json::<Value>()
            .with_context(|| format!("failed to parse OAuth discovery response: {}", step.id))?;
        let saved = apply_discovery_step(step, &json, |_| 0)?;
        context.extend(saved.clone());
        responses.extend(saved);
    }
    Ok(responses)
}

pub fn execute_post_login_discovery_with_responses<F>(
    metadata: &OAuthDeviceMetadata,
    responses: &BTreeMap<String, Value>,
    mut select_index: F,
) -> Result<BTreeMap<String, String>>
where
    F: FnMut(&DiscoveryStep, &[Value]) -> usize,
{
    let mut values = BTreeMap::new();
    for step in &metadata.post_login_discovery {
        for required in &step.requires {
            if !values.contains_key(required) {
                bail!(
                    "OAuth discovery step {} requires missing value {}",
                    step.id,
                    required
                );
            }
        }
        if step.url_template.is_some() {
            let _ = resolve_discovery_url(step, &values)?;
        }
        let response = responses
            .get(&step.id)
            .ok_or_else(|| anyhow!("missing OAuth discovery response for step {}", step.id))?;
        let saved = apply_discovery_step(step, response, |items| select_index(step, items))?;
        values.extend(saved);
    }
    Ok(values)
}

fn apply_discovery_step<F>(
    step: &DiscoveryStep,
    response: &Value,
    mut select_index: F,
) -> Result<BTreeMap<String, String>>
where
    F: FnMut(&[Value]) -> usize,
{
    let mut saved = BTreeMap::new();
    for (from, to) in &step.save {
        if let Some(value) = get_json_path(response, from).and_then(value_to_string) {
            saved.insert(to.clone(), value);
        }
    }
    if let Some(select) = &step.select {
        let items = get_json_path(response, &select.from)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                anyhow!(
                    "OAuth discovery step {} did not return selectable array",
                    step.id
                )
            })?;
        if items.is_empty() {
            bail!(
                "OAuth discovery step {} returned no selectable items",
                step.id
            );
        }
        let index = select_index(items).min(items.len() - 1);
        let item = &items[index];
        let value = get_json_path(item, &select.value)
            .and_then(value_to_string)
            .ok_or_else(|| {
                anyhow!(
                    "OAuth discovery step {} selected item missing value",
                    step.id
                )
            })?;
        saved.insert(select.save_as.clone(), value);
    }
    Ok(saved)
}

fn resolve_discovery_url(
    step: &DiscoveryStep,
    context: &BTreeMap<String, String>,
) -> Result<String> {
    if let Some(url) = &step.url {
        return Ok(url.clone());
    }
    let Some(template) = &step.url_template else {
        bail!(
            "OAuth discovery step {} missing url or url_template",
            step.id
        );
    };
    let mut resolved = template.clone();
    for required in &step.requires {
        let value = context.get(required).ok_or_else(|| {
            anyhow!(
                "OAuth discovery step {} requires missing value {}",
                step.id,
                required
            )
        })?;
        resolved = resolved.replace(&format!("{{{required}}}"), value);
    }
    Ok(resolved)
}

fn get_json_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

fn lookup_client_id(metadata: &OAuthDeviceMetadata, setup_answers: &Value) -> Result<String> {
    let keys = [
        metadata.client_id_config_key.as_str(),
        "client_id",
        "oauth_client_id",
    ];
    if let Some(obj) = setup_answers.as_object() {
        for key in keys {
            if let Some(value) = obj
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Ok(value.to_string());
            }
        }
    }
    bail!(
        "OAuth device-code client_id is missing from provider setup answers; configure {} first",
        metadata.client_id_config_key
    )
}

fn poll_error_message(error: &str, response: &Value) -> String {
    response
        .get("error_description")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("OAuth device-code polling failed: {error}"))
}

fn load_provider_setup_answers(bundle_root: &Path, provider_id: &str) -> Result<Value> {
    let path = bundle_root
        .join("state")
        .join("config")
        .join(provider_id)
        .join("setup-answers.json");
    if !path.exists() {
        return Ok(Value::Object(JsonMap::new()));
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn save_session(bundle_root: &Path, state: &OAuthDeviceSessionState) -> Result<()> {
    let path = session_path(bundle_root, &state.session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, payload).with_context(|| format!("failed to write {}", path.display()))
}

fn load_session(bundle_root: &Path, session_id: &str) -> Result<OAuthDeviceSessionState> {
    let path = session_path(bundle_root, session_id);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn session_path(bundle_root: &Path, session_id: &str) -> PathBuf {
    bundle_root
        .join(".greentic")
        .join("oauth-device-sessions")
        .join(format!("{session_id}.json"))
}

fn new_session_id() -> String {
    base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        rand::random::<[u8; 16]>(),
    )
}

fn team_segment(team: Option<&str>) -> &str {
    team.map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
}

fn default_client_id_config_key() -> String {
    "client_id".to_string()
}

fn default_method() -> String {
    "GET".to_string()
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata() -> OAuthDeviceMetadata {
        OAuthDeviceMetadata {
            device_code_url: "https://login.example/devicecode".into(),
            token_url: "https://login.example/token".into(),
            scopes: vec!["offline_access".into(), "User.Read".into()],
            secrets_out: BTreeMap::from([
                ("refresh_token".into(), "MS_GRAPH_REFRESH_TOKEN".into()),
                ("client_id".into(), "MS_GRAPH_CLIENT_ID".into()),
            ]),
            ..Default::default()
        }
    }

    #[test]
    fn device_code_request_form_omits_client_secret() {
        let metadata = metadata();
        let form = device_code_request_form(&metadata, "client-123");
        assert_eq!(
            form,
            vec![
                ("client_id", "client-123".to_string()),
                ("scope", "offline_access User.Read".to_string())
            ]
        );
        assert!(!form.iter().any(|(key, _)| *key == "client_secret"));
    }

    #[test]
    fn token_poll_request_form_omits_client_secret() {
        let form = token_poll_request_form("client-123", "device-secret");
        assert_eq!(
            form,
            vec![
                ("client_id", "client-123".to_string()),
                (
                    "grant_type",
                    "urn:ietf:params:oauth:grant-type:device_code".to_string()
                ),
                ("device_code", "device-secret".to_string())
            ]
        );
        assert!(!form.iter().any(|(key, _)| *key == "client_secret"));
    }

    #[test]
    fn token_response_maps_refresh_token_and_client_id() {
        let mapped =
            map_device_token_response(&metadata(), "client-123", &json!({"refresh_token": "rt"}))
                .unwrap();
        assert_eq!(
            mapped.get("MS_GRAPH_REFRESH_TOKEN").map(String::as_str),
            Some("rt")
        );
        assert_eq!(
            mapped.get("MS_GRAPH_CLIENT_ID").map(String::as_str),
            Some("client-123")
        );
    }

    #[test]
    fn start_report_excludes_raw_device_code() {
        let temp = tempfile::tempdir().unwrap();
        let input = OAuthDeviceStartInput {
            provider_id: "messaging-teams".into(),
            tenant: "demo".into(),
            team: None,
            action_id: "connect".into(),
        };
        let report = start_oauth_device_code_with_response(
            temp.path(),
            &input,
            &OAuthDeviceMetadata {
                verification_uri: Some("https://microsoft.com/devicelogin".into()),
                ..metadata()
            },
            "client-123",
            &json!({
                "device_code": "raw-device-code",
                "user_code": "ABCD-EFGH",
                "expires_in": 900,
                "interval": 5
            }),
        )
        .unwrap();
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(serialized.contains("ABCD-EFGH"));
        assert!(!serialized.contains("raw-device-code"));
    }

    #[test]
    fn discovery_saves_scalars_and_selected_values() {
        let mut metadata = metadata();
        metadata.post_login_discovery = vec![
            DiscoveryStep {
                id: "me".into(),
                method: "GET".into(),
                url: Some("https://graph.example/me".into()),
                url_template: None,
                requires: Vec::new(),
                save: BTreeMap::from([("id".into(), "user_id".into())]),
                select: None,
            },
            DiscoveryStep {
                id: "teams".into(),
                method: "GET".into(),
                url: Some("https://graph.example/joinedTeams".into()),
                url_template: None,
                requires: Vec::new(),
                save: BTreeMap::new(),
                select: Some(DiscoverySelect {
                    from: "value".into(),
                    label: "displayName".into(),
                    value: "id".into(),
                    save_as: "team_id".into(),
                }),
            },
            DiscoveryStep {
                id: "channels".into(),
                method: "GET".into(),
                url: None,
                url_template: Some("https://graph.example/teams/{team_id}/channels".into()),
                requires: vec!["team_id".into()],
                save: BTreeMap::new(),
                select: Some(DiscoverySelect {
                    from: "value".into(),
                    label: "displayName".into(),
                    value: "id".into(),
                    save_as: "channel_id".into(),
                }),
            },
        ];
        let responses = BTreeMap::from([
            ("me".into(), json!({"id": "user-1"})),
            (
                "teams".into(),
                json!({"value": [
                    {"id": "team-1", "displayName": "One"},
                    {"id": "team-2", "displayName": "Two"}
                ]}),
            ),
            (
                "channels".into(),
                json!({"value": [
                    {"id": "channel-1", "displayName": "General"},
                    {"id": "channel-2", "displayName": "Ops"}
                ]}),
            ),
        ]);
        let values =
            execute_post_login_discovery_with_responses(&metadata, &responses, |step, _| {
                if step.id == "teams" { 1 } else { 0 }
            })
            .unwrap();
        assert_eq!(values.get("user_id").map(String::as_str), Some("user-1"));
        assert_eq!(values.get("team_id").map(String::as_str), Some("team-2"));
        assert_eq!(
            values.get("channel_id").map(String::as_str),
            Some("channel-1")
        );
    }
}
