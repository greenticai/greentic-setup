//! Provider-agnostic OAuth device-code setup helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value, json};

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
    pub setup_modes: BTreeMap<String, OAuthDeviceSetupMode>,
    #[serde(default)]
    pub error_checklist: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthDeviceSetupMode {
    #[serde(default)]
    pub provisioning: BTreeMap<String, OAuthDeviceProvisioning>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthDeviceProvisioning {
    #[serde(default)]
    pub component_ref: String,
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub output_keys: BTreeMap<String, String>,
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
    #[serde(default)]
    pub label_save_as: Option<String>,
    #[serde(default)]
    pub default_label: Option<String>,
    #[serde(default)]
    pub default_filter: Option<String>,
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
    let metadata = raw.get("inline").cloned().unwrap_or(raw);
    let mut metadata: OAuthDeviceMetadata = serde_json::from_value(metadata)
        .context("failed to parse provider OAuth device-code metadata")?;
    let bundle_name = crate::bundle::read_bundle_name(bundle_root).ok().flatten();
    apply_bundle_name_templates(&mut metadata, bundle_name.as_deref());
    Ok(metadata)
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
    let mut response = crate::http_client::api_agent()
        .post(&metadata.device_code_url)
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
    let agent = crate::http_client::api_agent_any_status();
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
    let final_mapped =
        finalize_provider_apply_answers(bundle_root, env, session, metadata, response, &mapped)
            .await?;
    let final_mapped = final_mapped.as_ref().unwrap_or(&mapped);
    persist_device_config_outputs(bundle_root, &session.provider_id, metadata, final_mapped)?;
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
        persisted_keys: final_mapped.keys().cloned().collect(),
        checklist: Vec::new(),
        interval: None,
    })
}

async fn finalize_provider_apply_answers(
    bundle_root: &Path,
    env: &str,
    session: &OAuthDeviceSessionState,
    metadata: &OAuthDeviceMetadata,
    response: &Value,
    mapped: &BTreeMap<String, String>,
) -> Result<Option<BTreeMap<String, String>>> {
    let Some(provisioning) = apply_answers_provisioning(metadata) else {
        return Ok(None);
    };
    let discovered = crate::discovery::discover(bundle_root)
        .context("failed to discover providers for OAuth device-code apply-answers")?;
    let provider = discovered
        .find_setup_target(&session.provider_id)
        .ok_or_else(|| {
            anyhow!(
                "provider not found for OAuth device-code apply-answers: {}",
                session.provider_id
            )
        })?;
    let answers = load_provider_setup_answers(bundle_root, &session.provider_id)?;
    let request =
        json_apply_answers_request(&answers, metadata, response, &session.client_id, mapped)?;
    let config = crate::engine::SetupConfig {
        tenant: session.tenant.clone(),
        team: Some(session.team.clone()),
        env: env.to_string(),
        offline: false,
        verbose: false,
    };
    let pack_path = provider.pack_path.clone();
    let component_ref = provisioning.component_ref.clone();
    let op = provisioning.op.clone();
    let bundle_root_owned = bundle_root.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        crate::engine::invoke_setup_component_operation(
            &bundle_root_owned,
            &pack_path,
            &component_ref,
            &op,
            &request,
            &config,
        )
    })
    .await
    .context("OAuth device-code apply-answers task failed")?
    .with_context(|| {
        format!(
            "OAuth device-code apply-answers failed for {}",
            session.provider_id
        )
    })?;
    let Some(config) = apply_answers_result_config(&result)? else {
        return Ok(None);
    };
    crate::qa::persist::persist_all_config_as_secrets(
        bundle_root,
        env,
        &session.tenant,
        Some(&session.team),
        &session.provider_id,
        &config,
        Some(&provider.pack_path),
    )
    .await?;
    Ok(Some(map_config_object(&config)))
}

fn apply_answers_provisioning(metadata: &OAuthDeviceMetadata) -> Option<&OAuthDeviceProvisioning> {
    metadata
        .setup_modes
        .values()
        .flat_map(|mode| mode.provisioning.values())
        .find(|provisioning| {
            provisioning.op == "apply-answers" && !provisioning.component_ref.trim().is_empty()
        })
}

fn json_apply_answers_request(
    existing_answers: &Value,
    metadata: &OAuthDeviceMetadata,
    response: &Value,
    client_id: &str,
    mapped: &BTreeMap<String, String>,
) -> Result<Value> {
    let mut answers = existing_answers.as_object().cloned().unwrap_or_default();
    for (key, value) in mapped {
        answers.insert(key.clone(), Value::String(value.clone()));
    }
    for response_key in metadata
        .secrets_out
        .keys()
        .chain(metadata.config_out.keys())
    {
        let value = if response_key == "client_id" {
            Some(client_id.to_string())
        } else {
            oauth_response_value(response, response_key)
        };
        if let Some(value) = value {
            answers.insert(response_key.clone(), Value::String(value));
        }
    }
    Ok(json!({
        "mode": "setup",
        "answers": Value::Object(answers)
    }))
}

fn apply_answers_result_config(result: &Value) -> Result<Option<Value>> {
    if result.get("ok").and_then(Value::as_bool) == Some(false) {
        let message = result
            .get("error")
            .or_else(|| result.get("message"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("provider apply-answers returned ok:false");
        bail!("OAuth device-code apply-answers failed: {message}");
    }
    Ok(result.get("config").cloned().or_else(|| {
        result
            .as_object()
            .is_some_and(|object| !object.contains_key("ok"))
            .then(|| result.clone())
    }))
}

fn map_config_object(config: &Value) -> BTreeMap<String, String> {
    config
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter_map(|(key, value)| value_to_string(value).map(|value| (key.clone(), value)))
        .collect()
}

fn oauth_response_value(response: &Value, key: &str) -> Option<String> {
    response
        .get(key)
        .and_then(value_to_string)
        .or_else(|| oauth_token_claim_value(response, key))
}

fn oauth_token_claim_value(response: &Value, key: &str) -> Option<String> {
    for token_key in ["id_token", "access_token"] {
        let Some(token) = response
            .get(token_key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some(claims) = decode_unverified_jwt_claims(token) else {
            continue;
        };
        if let Some(value) = claims.get(key).and_then(value_to_string) {
            return Some(value);
        }
        for alias in oauth_claim_aliases(key) {
            if let Some(value) = claims.get(alias).and_then(value_to_string) {
                return Some(value);
            }
        }
    }
    None
}

fn oauth_claim_aliases(key: &str) -> &'static [&'static str] {
    match key {
        "tenant_id" => &["tid"],
        "user_id" => &["oid", "sub"],
        _ => &[],
    }
}

fn decode_unverified_jwt_claims(token: &str) -> Option<Value> {
    let claims = token.split('.').nth(1)?;
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, claims).ok()?;
    serde_json::from_slice(&bytes).ok()
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
            oauth_response_value(response, response_key)
        };
        if let Some(value) = value {
            mapped.insert(output_key.clone(), value);
        }
    }
    for (response_key, output_key) in &metadata.config_out {
        let value = if response_key == "client_id" {
            Some(client_id.to_string())
        } else {
            oauth_response_value(response, response_key)
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

fn persist_device_config_outputs(
    bundle_root: &Path,
    provider_id: &str,
    metadata: &OAuthDeviceMetadata,
    mapped: &BTreeMap<String, String>,
) -> Result<()> {
    let config_outputs: JsonMap<String, Value> = mapped
        .iter()
        .filter(|(key, _)| !is_sensitive_device_output_key(metadata, key))
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    if config_outputs.is_empty() {
        return Ok(());
    }

    let path = provider_setup_answers_path(bundle_root, provider_id);
    let mut answers = load_provider_setup_answers(bundle_root, provider_id)?;
    let Some(answer_map) = answers.as_object_mut() else {
        bail!(
            "provider setup answers must be a JSON object: {}",
            path.display()
        );
    };
    for (key, value) in config_outputs {
        answer_map.insert(key, value);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(&answers)?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;

    let verified = load_provider_setup_answers(bundle_root, provider_id)?;
    for (key, value) in mapped {
        if is_sensitive_device_output_key(metadata, key) {
            continue;
        }
        let actual = verified.get(key).and_then(Value::as_str);
        if actual != Some(value.as_str()) {
            bail!("failed to verify persisted device-code config output {key}");
        }
    }
    Ok(())
}

fn is_sensitive_device_output_key(metadata: &OAuthDeviceMetadata, key: &str) -> bool {
    metadata.secrets_out.values().any(|value| value == key)
        || metadata
            .secrets_out
            .keys()
            .any(|value| value == key && value != "client_id")
        || matches!(
            key.to_ascii_lowercase().as_str(),
            "access_token" | "refresh_token" | "client_secret" | "ms_bot_app_password"
        )
}

pub fn execute_post_login_discovery(
    metadata: &OAuthDeviceMetadata,
    access_token: &str,
) -> Result<BTreeMap<String, String>> {
    let mut responses = BTreeMap::new();
    let mut context = BTreeMap::new();
    for step in &metadata.post_login_discovery {
        let url = resolve_discovery_url(step, &context)?;
        let mut response = crate::http_client::api_agent()
            .get(&url)
            .header("Authorization", &format!("Bearer {access_token}"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()
            .with_context(|| format!("OAuth device-code discovery request failed: {}", step.id))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.body_mut().read_to_string().unwrap_or_default();
            bail!(
                "{}",
                discovery_http_error_message(step, &url, status, &body)
            );
        }
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

fn discovery_http_error_message(
    step: &DiscoveryStep,
    url: &str,
    status: u16,
    body: &str,
) -> String {
    let mut message = format!(
        "OAuth device-code discovery request failed: {} (HTTP {status} from {url})",
        step.id
    );
    let body = compact_error_body(body);
    if !body.is_empty() {
        message.push_str(": ");
        message.push_str(&body);
    }
    message
}

fn compact_error_body(body: &str) -> String {
    const MAX_BODY_CHARS: usize = 2000;
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= MAX_BODY_CHARS {
        return compact;
    }
    let truncated = compact.chars().take(MAX_BODY_CHARS).collect::<String>();
    format!("{truncated}...")
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
        let index = select_index_with_default(select, items, &mut select_index);
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
        if let Some(label) = get_json_path(item, &select.label).and_then(value_to_string) {
            saved.insert(discovery_label_save_key(select), label);
        }
    }
    Ok(saved)
}

fn select_index_with_default<F>(
    select: &DiscoverySelect,
    items: &[Value],
    select_index: &mut F,
) -> usize
where
    F: FnMut(&[Value]) -> usize,
{
    if let Some(index) = preferred_discovery_item_index(select, items) {
        return index;
    }
    select_index(items).min(items.len() - 1)
}

fn preferred_discovery_item_index(select: &DiscoverySelect, items: &[Value]) -> Option<usize> {
    if let Some(default_label) = select
        .default_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let default_label = default_label.to_ascii_lowercase();
        if let Some(index) = items.iter().position(|item| {
            get_json_path(item, &select.label)
                .and_then(value_to_string)
                .is_some_and(|label| label.trim().eq_ignore_ascii_case(&default_label))
        }) {
            return Some(index);
        }
    }

    let filter = select
        .default_filter
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_ascii_lowercase();
    items.iter().position(|item| {
        get_json_path(item, &select.label)
            .and_then(value_to_string)
            .is_some_and(|label| label.to_ascii_lowercase().contains(&filter))
    })
}

fn discovery_label_save_key(select: &DiscoverySelect) -> String {
    select
        .label_save_as
        .clone()
        .unwrap_or_else(|| inferred_label_save_key(&select.save_as))
}

fn inferred_label_save_key(save_as: &str) -> String {
    save_as
        .strip_suffix("_id")
        .map(|prefix| format!("{prefix}_name"))
        .unwrap_or_else(|| format!("{save_as}_label"))
}

fn apply_bundle_name_templates(metadata: &mut OAuthDeviceMetadata, bundle_name: Option<&str>) {
    let Some(bundle_name) = bundle_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    for step in &mut metadata.post_login_discovery {
        let Some(select) = step.select.as_mut() else {
            continue;
        };
        if let Some(value) = select.default_label.as_mut() {
            *value = render_bundle_name_template(value, bundle_name);
        }
        if let Some(value) = select.default_filter.as_mut() {
            *value = render_bundle_name_template(value, bundle_name);
        }
    }
}

fn render_bundle_name_template(template: &str, bundle_name: &str) -> String {
    template
        .replace("{{ bundle_name }}", bundle_name)
        .replace("{{bundle_name}}", bundle_name)
        .replace("{bundle_name}", bundle_name)
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
    let path = provider_setup_answers_path(bundle_root, provider_id);
    if !path.exists() {
        return Ok(Value::Object(JsonMap::new()));
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn provider_setup_answers_path(bundle_root: &Path, provider_id: &str) -> PathBuf {
    bundle_root
        .join("state")
        .join("config")
        .join(provider_id)
        .join("setup-answers.json")
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
    use greentic_secrets_lib::SecretsStore;
    use serde_json::json;
    use std::io::Write;
    use std::path::Path;
    use zip::write::{FileOptions, ZipWriter};

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

    fn metadata_with_apply_answers() -> OAuthDeviceMetadata {
        let mut metadata = metadata();
        metadata
            .secrets_out
            .insert("access_token".into(), "MS_GRAPH_ACCESS_TOKEN".into());
        metadata.config_out = BTreeMap::from([
            ("tenant_id".into(), "tenant_id".into()),
            ("user_id".into(), "user_id".into()),
            ("team_id".into(), "team_id".into()),
            ("team_name".into(), "team_name".into()),
            ("channel_id".into(), "channel_id".into()),
            ("channel_name".into(), "channel_name".into()),
            ("desired_channel_name".into(), "desired_channel_name".into()),
        ]);
        metadata.setup_modes = BTreeMap::from([(
            "graph_channel".into(),
            OAuthDeviceSetupMode {
                provisioning: BTreeMap::from([(
                    "teams_channel".into(),
                    OAuthDeviceProvisioning {
                        component_ref: "provision".into(),
                        op: "apply-answers".into(),
                        output_keys: BTreeMap::from([
                            ("channel_id".into(), "channel_id".into()),
                            ("channel_name".into(), "channel_name".into()),
                        ]),
                    },
                )]),
            },
        )]);
        metadata
    }

    fn unsigned_jwt_claims(claims: Value) -> String {
        let header = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"alg":"none"}"#,
        );
        let claims = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            claims.to_string(),
        );
        format!("{header}.{claims}.")
    }

    fn setup_pending_oauth_action(
        bundle: &Path,
        provider_id: &str,
        tenant: &str,
        team: &str,
        action_id: &str,
    ) {
        crate::setup_actions::persist_setup_actions(
            bundle,
            &[crate::setup_actions::SetupAction {
                id: action_id.into(),
                kind: crate::setup_actions::SetupActionKind::OauthDeviceCode,
                label: "Connect Teams".into(),
                provider_id: provider_id.into(),
                tenant: tenant.into(),
                team: Some(team.into()),
                authorize_url: None,
                callback_path: None,
                state: None,
                status: crate::setup_actions::SetupActionStatus::Pending,
                created_at: None,
                completed_at: None,
                extra: JsonMap::new(),
            }],
        )
        .unwrap();
    }

    fn session_state(
        provider_id: &str,
        tenant: &str,
        team: &str,
        action_id: &str,
    ) -> OAuthDeviceSessionState {
        OAuthDeviceSessionState {
            session_id: "session-1".into(),
            provider_id: provider_id.into(),
            tenant: tenant.into(),
            team: team.into(),
            action_id: action_id.into(),
            device_code: "device-code".into(),
            client_id: "client-123".into(),
            interval: 5,
            expires_at: crate::setup_actions::current_epoch_secs() + 900,
            created_at: crate::setup_actions::current_epoch_secs(),
        }
    }

    fn write_setup_answers(bundle: &Path, provider_id: &str, answers: Value) {
        let config_dir = bundle.join("state/config").join(provider_id);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("setup-answers.json"),
            serde_json::to_string_pretty(&answers).unwrap(),
        )
        .unwrap();
    }

    fn write_mock_provider_pack(
        bundle: &Path,
        provider_id: &str,
        result: Value,
    ) -> anyhow::Result<()> {
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack_with_files(
            &bundle
                .join("providers/messaging")
                .join(format!("{provider_id}.gtpack")),
            json!({
                "pack_id": provider_id,
                "extensions": {}
            }),
            [(
                "components/provision.json",
                json!({
                    "operations": {
                        "apply-answers": {
                            "result": result
                        }
                    }
                }),
            )],
        )
    }

    fn write_provider_pack_with_manifest(
        path: &Path,
        manifest: serde_json::Value,
    ) -> anyhow::Result<()> {
        write_provider_pack_with_files(
            path,
            manifest,
            std::iter::empty::<(&str, serde_json::Value)>(),
        )
    }

    fn write_provider_pack_with_files<I, P>(
        path: &Path,
        manifest: serde_json::Value,
        files: I,
    ) -> anyhow::Result<()>
    where
        I: IntoIterator<Item = (P, serde_json::Value)>,
        P: AsRef<str>,
    {
        let file = std::fs::File::create(path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(manifest.to_string().as_bytes())?;
        for (file_path, value) in files {
            writer.start_file(file_path.as_ref(), options)?;
            writer.write_all(value.to_string().as_bytes())?;
        }
        writer.finish()?;
        Ok(())
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
    fn load_provider_device_metadata_accepts_inline_extension_wrapper() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        crate::bundle::create_demo_bundle_structure(bundle, Some("Acme Support"))?;
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack_with_manifest(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
            json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "messaging.oauth_device_code.v1": {
                        "kind": "messaging.oauth_device_code.v1",
                        "version": "1",
                        "inline": {
                            "device_code_url": "https://login.example/devicecode",
                            "token_url": "https://login.example/token",
                            "verification_uri": "https://login.example/device",
                            "client_id_config_key": "client_id",
                            "scopes": ["User.Read"],
                            "post_login_discovery": [{
                                "id": "rooms",
                                "url": "https://api.example/rooms",
                                "select": {
                                    "from": "value",
                                    "label": "displayName",
                                    "value": "id",
                                    "save_as": "room_id",
                                    "default_filter": "{{ bundle_name }}"
                                }
                            }]
                        }
                    }
                }
            }),
        )?;

        let metadata = load_provider_device_metadata(
            bundle,
            "messaging-example",
            "messaging.oauth_device_code.v1",
        )?;

        assert_eq!(metadata.device_code_url, "https://login.example/devicecode");
        assert_eq!(metadata.token_url, "https://login.example/token");
        assert_eq!(metadata.scopes, vec!["User.Read"]);
        assert_eq!(
            metadata.post_login_discovery[0]
                .select
                .as_ref()
                .and_then(|select| select.default_filter.as_deref()),
            Some("Acme Support")
        );
        Ok(())
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
    fn start_report_uses_response_verification_url_and_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let input = OAuthDeviceStartInput {
            provider_id: "messaging-teams".into(),
            tenant: "demo".into(),
            team: Some("".into()),
            action_id: "connect".into(),
        };

        let report = start_oauth_device_code_with_response(
            temp.path(),
            &input,
            &metadata(),
            "client-123",
            &json!({
                "device_code": "raw-device-code",
                "user_code": "ABCD-EFGH",
                "verification_url": "https://login.example/verify",
                "verification_uri_complete": "https://login.example/verify?code=ABCD-EFGH",
                "interval": 0
            }),
        )
        .unwrap();

        assert_eq!(report.team, "default");
        assert_eq!(report.interval, 1);
        assert_eq!(report.verification_uri, "https://login.example/verify");
        assert_eq!(
            report.verification_uri_complete.as_deref(),
            Some("https://login.example/verify?code=ABCD-EFGH")
        );
    }

    #[test]
    fn start_report_rejects_missing_device_code_or_verification_uri() {
        let temp = tempfile::tempdir().unwrap();
        let input = OAuthDeviceStartInput {
            provider_id: "messaging-teams".into(),
            tenant: "demo".into(),
            team: None,
            action_id: "connect".into(),
        };

        let missing_device_code = start_oauth_device_code_with_response(
            temp.path(),
            &input,
            &metadata(),
            "client-123",
            &json!({
                "device_code": "",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://login.example/verify"
            }),
        )
        .unwrap_err()
        .to_string();
        assert!(missing_device_code.contains("missing device_code"));

        let missing_verification_uri = start_oauth_device_code_with_response(
            temp.path(),
            &input,
            &metadata(),
            "client-123",
            &json!({
                "device_code": "raw-device-code",
                "user_code": "ABCD-EFGH"
            }),
        )
        .unwrap_err()
        .to_string();
        assert!(missing_verification_uri.contains("missing verification URI"));
    }

    #[test]
    fn poll_error_states_are_provider_neutral() {
        let temp = tempfile::tempdir().unwrap();
        let mut metadata = metadata();
        metadata.error_checklist = vec!["Try again".into()];
        let session = OAuthDeviceSessionState {
            session_id: "session-1".into(),
            provider_id: "messaging-teams".into(),
            tenant: "demo".into(),
            team: "default".into(),
            action_id: "connect".into(),
            device_code: "device-code".into(),
            client_id: "client-123".into(),
            interval: 5,
            expires_at: crate::setup_actions::current_epoch_secs() + 900,
            created_at: crate::setup_actions::current_epoch_secs(),
        };
        save_session(temp.path(), &session).unwrap();

        let pending = handle_poll_error(
            temp.path(),
            &session,
            &metadata,
            "authorization_pending",
            &json!({"error_description": "not ready"}),
        )
        .unwrap();
        assert_eq!(pending.status, OAuthDevicePollStatus::Pending);
        assert_eq!(pending.message.as_deref(), Some("not ready"));
        assert_eq!(pending.interval, Some(5));

        let slow_down =
            handle_poll_error(temp.path(), &session, &metadata, "slow_down", &json!({})).unwrap();
        assert_eq!(slow_down.status, OAuthDevicePollStatus::SlowDown);
        assert_eq!(slow_down.interval, Some(10));
        assert_eq!(load_session(temp.path(), "session-1").unwrap().interval, 10);

        let failed = handle_poll_error(
            temp.path(),
            &session,
            &metadata,
            "authorization_declined",
            &json!({}),
        )
        .unwrap();
        assert_eq!(failed.status, OAuthDevicePollStatus::Failed);
        assert_eq!(failed.checklist, vec!["Try again"]);
        assert_eq!(
            failed.message.as_deref(),
            Some("OAuth device-code polling failed: authorization_declined")
        );
    }

    #[test]
    fn token_response_maps_config_scalars_and_rejects_empty_mapping() {
        let mut metadata = metadata();
        metadata.secrets_out.clear();
        metadata.config_out = BTreeMap::from([
            ("expires_in".into(), "token_expires_in".into()),
            ("enabled".into(), "token_enabled".into()),
            ("client_id".into(), "client_id_copy".into()),
        ]);

        let mapped = map_device_token_response(
            &metadata,
            "client-123",
            &json!({"expires_in": 3600, "enabled": true}),
        )
        .unwrap();

        assert_eq!(
            mapped.get("token_expires_in").map(String::as_str),
            Some("3600")
        );
        assert_eq!(
            mapped.get("token_enabled").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            mapped.get("client_id_copy").map(String::as_str),
            Some("client-123")
        );

        let mut empty_metadata = metadata;
        empty_metadata.config_out.clear();
        let error = map_device_token_response(&empty_metadata, "client-123", &json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("did not contain mappable values"));
    }

    #[test]
    fn token_response_maps_oidc_claim_aliases() {
        let mut metadata = metadata();
        metadata.secrets_out.clear();
        metadata.config_out = BTreeMap::from([
            ("tenant_id".into(), "tenant_id".into()),
            ("user_id".into(), "user_id".into()),
        ]);

        let mapped = map_device_token_response(
            &metadata,
            "client-123",
            &json!({
                "id_token": unsigned_jwt_claims(json!({
                    "tid": "tenant-123",
                    "oid": "user-123"
                }))
            }),
        )
        .unwrap();

        assert_eq!(
            mapped.get("tenant_id").map(String::as_str),
            Some("tenant-123")
        );
        assert_eq!(mapped.get("user_id").map(String::as_str), Some("user-123"));
    }

    #[tokio::test]
    async fn poll_persists_device_outputs_to_runtime_config_and_secrets() {
        let _store_iso_dir = tempfile::tempdir().expect("store isolation dir");
        let _store_iso = crate::secrets::test_support::StoreOverride::in_dir(_store_iso_dir.path());
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path();
        let provider_id = "messaging-teams";
        let tenant = "demo";
        let team = "default";
        let action_id = "teams-device-code";
        let config_dir = bundle.join("state/config").join(provider_id);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("setup-answers.json"),
            serde_json::to_string_pretty(&json!({
                "client_id": "client-123",
                "public_base_url": "https://tunnel.example"
            }))
            .unwrap(),
        )
        .unwrap();
        crate::setup_actions::persist_setup_actions(
            bundle,
            &[crate::setup_actions::SetupAction {
                id: action_id.into(),
                kind: crate::setup_actions::SetupActionKind::OauthDeviceCode,
                label: "Connect Teams".into(),
                provider_id: provider_id.into(),
                tenant: tenant.into(),
                team: Some(team.into()),
                authorize_url: None,
                callback_path: None,
                state: None,
                status: crate::setup_actions::SetupActionStatus::Pending,
                created_at: None,
                completed_at: None,
                extra: JsonMap::new(),
            }],
        )
        .unwrap();

        let session = OAuthDeviceSessionState {
            session_id: "session-1".into(),
            provider_id: provider_id.into(),
            tenant: tenant.into(),
            team: team.into(),
            action_id: action_id.into(),
            device_code: "device-code".into(),
            client_id: "client-123".into(),
            interval: 5,
            expires_at: crate::setup_actions::current_epoch_secs() + 900,
            created_at: crate::setup_actions::current_epoch_secs(),
        };
        save_session(bundle, &session).unwrap();

        let mut metadata = metadata();
        metadata
            .secrets_out
            .insert("access_token".into(), "MS_GRAPH_ACCESS_TOKEN".into());
        metadata.config_out = BTreeMap::from([
            ("tenant_id".into(), "tenant_id".into()),
            ("team_id".into(), "team_id".into()),
            ("team_name".into(), "team_name".into()),
            ("channel_id".into(), "channel_id".into()),
            ("channel_name".into(), "channel_name".into()),
        ]);

        let report = poll_oauth_device_code_with_token_response(
            bundle,
            "dev",
            &session,
            &metadata,
            &json!({
                "refresh_token": "refresh-123",
                "access_token": "access-123",
                "tenant_id": "tenant-123",
                "team_id": "team-123",
                "team_name": "Support",
                "channel_id": "channel-123",
                "channel_name": "Greentic"
            }),
        )
        .await
        .unwrap();

        assert_eq!(report.status, OAuthDevicePollStatus::Complete);

        let answers = load_provider_setup_answers(bundle, provider_id).unwrap();
        assert_eq!(answers["client_id"], json!("client-123"));
        assert_eq!(answers["public_base_url"], json!("https://tunnel.example"));
        assert_eq!(answers["tenant_id"], json!("tenant-123"));
        assert_eq!(answers["team_id"], json!("team-123"));
        assert_eq!(answers["team_name"], json!("Support"));
        assert_eq!(answers["channel_id"], json!("channel-123"));
        assert_eq!(answers["channel_name"], json!("Greentic"));
        assert!(answers.get("MS_GRAPH_REFRESH_TOKEN").is_none());
        assert!(answers.get("MS_GRAPH_ACCESS_TOKEN").is_none());

        let store = crate::secrets::open_dev_store_for_env(bundle, "dev").unwrap();
        let refresh_uri = crate::canonical_secret_uri(
            "dev",
            tenant,
            Some(team),
            provider_id,
            "MS_GRAPH_REFRESH_TOKEN",
        );
        let refresh = String::from_utf8(store.get(&refresh_uri).await.unwrap()).unwrap();
        assert_eq!(refresh, "refresh-123");
    }

    #[tokio::test]
    async fn poll_invokes_apply_answers_before_completing_and_persists_provider_config() {
        let _store_iso_dir = tempfile::tempdir().expect("store isolation dir");
        let _store_iso = crate::secrets::test_support::StoreOverride::in_dir(_store_iso_dir.path());
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path();
        let provider_id = "messaging-teams";
        let tenant = "demo";
        let team = "default";
        let action_id = "teams-device-code";
        write_setup_answers(
            bundle,
            provider_id,
            json!({
                "client_id": "client-123",
                "public_base_url": "https://tunnel.example",
                "desired_channel_name": "hr onboarding"
            }),
        );
        setup_pending_oauth_action(bundle, provider_id, tenant, team, action_id);
        write_mock_provider_pack(
            bundle,
            provider_id,
            json!({
                "ok": true,
                "config": {
                    "client_id": "client-123",
                    "user_id": "user-123",
                    "tenant_id": "tenant-123",
                    "team_id": "team-123",
                    "team_name": "Greentic AI Ltd",
                    "channel_id": "hr-channel-123",
                    "channel_name": "hr onboarding",
                    "desired_channel_name": "hr onboarding",
                    "refresh_token": "refresh-123",
                    "access_token": "access-123"
                }
            }),
        )
        .unwrap();

        let session = session_state(provider_id, tenant, team, action_id);
        save_session(bundle, &session).unwrap();
        let report = poll_oauth_device_code_with_token_response(
            bundle,
            "dev",
            &session,
            &metadata_with_apply_answers(),
            &json!({
                "refresh_token": "refresh-123",
                "access_token": "access-123",
                "id_token": unsigned_jwt_claims(json!({"tid": "tenant-123"})),
                "user_id": "user-123",
                "team_id": "team-123",
                "team_name": "Greentic AI Ltd",
                "channel_id": "general-channel-123",
                "channel_name": "General"
            }),
        )
        .await
        .unwrap();

        assert_eq!(report.status, OAuthDevicePollStatus::Complete);
        let action =
            crate::setup_actions::load_setup_action(bundle, tenant, team, provider_id, action_id)
                .unwrap()
                .unwrap();
        assert_eq!(
            action.status,
            crate::setup_actions::SetupActionStatus::Complete
        );

        let answers = load_provider_setup_answers(bundle, provider_id).unwrap();
        assert_eq!(answers["client_id"], json!("client-123"));
        assert_eq!(answers["user_id"], json!("user-123"));
        assert_eq!(answers["team_id"], json!("team-123"));
        assert_eq!(answers["team_name"], json!("Greentic AI Ltd"));
        assert_eq!(answers["channel_id"], json!("hr-channel-123"));
        assert_eq!(answers["channel_name"], json!("hr onboarding"));
        assert_eq!(answers["desired_channel_name"], json!("hr onboarding"));
        assert!(answers.get("refresh_token").is_none());
        assert!(answers.get("access_token").is_none());
        assert!(answers.get("MS_GRAPH_REFRESH_TOKEN").is_none());
        assert!(answers.get("MS_GRAPH_ACCESS_TOKEN").is_none());

        let store = crate::secrets::open_dev_store_for_env(bundle, "dev").unwrap();
        let refresh_uri = crate::canonical_secret_uri(
            "dev",
            tenant,
            Some(team),
            provider_id,
            "MS_GRAPH_REFRESH_TOKEN",
        );
        let access_uri = crate::canonical_secret_uri(
            "dev",
            tenant,
            Some(team),
            provider_id,
            "MS_GRAPH_ACCESS_TOKEN",
        );
        let refresh = String::from_utf8(store.get(&refresh_uri).await.unwrap()).unwrap();
        let access = String::from_utf8(store.get(&access_uri).await.unwrap()).unwrap();
        assert_eq!(refresh, "refresh-123");
        assert_eq!(access, "access-123");
    }

    #[tokio::test]
    async fn poll_does_not_complete_when_apply_answers_returns_not_ok() {
        let _store_iso_dir = tempfile::tempdir().expect("store isolation dir");
        let _store_iso = crate::secrets::test_support::StoreOverride::in_dir(_store_iso_dir.path());
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path();
        let provider_id = "messaging-teams";
        let tenant = "demo";
        let team = "default";
        let action_id = "teams-device-code";
        write_setup_answers(
            bundle,
            provider_id,
            json!({
                "client_id": "client-123",
                "desired_channel_name": "hr onboarding"
            }),
        );
        setup_pending_oauth_action(bundle, provider_id, tenant, team, action_id);
        write_mock_provider_pack(
            bundle,
            provider_id,
            json!({
                "ok": false,
                "error": "cannot create channel"
            }),
        )
        .unwrap();

        let session = session_state(provider_id, tenant, team, action_id);
        save_session(bundle, &session).unwrap();
        let error = poll_oauth_device_code_with_token_response(
            bundle,
            "dev",
            &session,
            &metadata_with_apply_answers(),
            &json!({
                "refresh_token": "refresh-123",
                "access_token": "access-123",
                "tenant_id": "tenant-123",
                "user_id": "user-123",
                "team_id": "team-123",
                "team_name": "Greentic AI Ltd",
                "channel_id": "general-channel-123",
                "channel_name": "General"
            }),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(error.contains("cannot create channel"));
        let action =
            crate::setup_actions::load_setup_action(bundle, tenant, team, provider_id, action_id)
                .unwrap()
                .unwrap();
        assert_eq!(
            action.status,
            crate::setup_actions::SetupActionStatus::Pending
        );
    }

    #[test]
    fn apply_answers_request_includes_existing_answers_discovery_and_raw_tokens() {
        let request = json_apply_answers_request(
            &json!({
                "client_id": "client-123",
                "desired_channel_name": "hr onboarding"
            }),
            &metadata_with_apply_answers(),
            &json!({
                "refresh_token": "refresh-123",
                "access_token": "access-123",
                "tenant_id": "tenant-123",
                "team_id": "team-123",
                "channel_id": "general-channel-123",
                "channel_name": "General"
            }),
            "client-123",
            &BTreeMap::from([
                ("MS_GRAPH_REFRESH_TOKEN".into(), "refresh-123".into()),
                ("MS_GRAPH_ACCESS_TOKEN".into(), "access-123".into()),
                ("tenant_id".into(), "tenant-123".into()),
                ("team_id".into(), "team-123".into()),
                ("channel_id".into(), "general-channel-123".into()),
                ("channel_name".into(), "General".into()),
            ]),
        )
        .unwrap();

        let answers = &request["answers"];
        assert_eq!(answers["desired_channel_name"], json!("hr onboarding"));
        assert_eq!(answers["refresh_token"], json!("refresh-123"));
        assert_eq!(answers["access_token"], json!("access-123"));
        assert_eq!(answers["MS_GRAPH_REFRESH_TOKEN"], json!("refresh-123"));
        assert_eq!(answers["team_id"], json!("team-123"));
        assert_eq!(answers["channel_name"], json!("General"));
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
                    label_save_as: None,
                    default_label: None,
                    default_filter: None,
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
                    label_save_as: None,
                    default_label: Some("Ops".into()),
                    default_filter: None,
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
        assert_eq!(values.get("team_name").map(String::as_str), Some("Two"));
        assert_eq!(
            values.get("channel_id").map(String::as_str),
            Some("channel-2")
        );
        assert_eq!(values.get("channel_name").map(String::as_str), Some("Ops"));
    }

    #[test]
    fn discovery_reports_missing_requirements_and_bad_selects() {
        let mut metadata = metadata();
        metadata.post_login_discovery = vec![DiscoveryStep {
            id: "channels".into(),
            method: "GET".into(),
            url: None,
            url_template: Some("https://graph.example/teams/{team_id}/channels".into()),
            requires: vec!["team_id".into()],
            save: BTreeMap::new(),
            select: None,
        }];
        let error =
            execute_post_login_discovery_with_responses(&metadata, &BTreeMap::new(), |_, _| 0)
                .unwrap_err()
                .to_string();
        assert!(error.contains("requires missing value team_id"));

        let step = DiscoveryStep {
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
                label_save_as: None,
                default_label: None,
                default_filter: None,
            }),
        };
        let error = apply_discovery_step(&step, &json!({"value": []}), |_| 0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("returned no selectable items"));

        let error = apply_discovery_step(&step, &json!({"value": [{"displayName": "One"}]}), |_| 0)
            .unwrap_err()
            .to_string();
        assert!(error.contains("selected item missing value"));
    }

    #[test]
    fn discovery_http_error_includes_step_status_url_and_body() {
        let step = DiscoveryStep {
            id: "joined_teams".into(),
            method: "GET".into(),
            url: Some("https://graph.example/me/joinedTeams".into()),
            url_template: None,
            requires: Vec::new(),
            save: BTreeMap::new(),
            select: None,
        };

        let message = discovery_http_error_message(
            &step,
            "https://graph.example/me/joinedTeams",
            403,
            r#"{
                "error": {
                    "code": "Authorization_RequestDenied",
                    "message": "Insufficient privileges to complete the operation."
                }
            }"#,
        );

        assert!(message.contains("joined_teams"));
        assert!(message.contains("HTTP 403"));
        assert!(message.contains("https://graph.example/me/joinedTeams"));
        assert!(message.contains("Authorization_RequestDenied"));
        assert!(message.contains("Insufficient privileges"));
    }
}
