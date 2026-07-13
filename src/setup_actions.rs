//! Provider-agnostic setup actions and OAuth setup helpers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupActionKind {
    OauthInstallButton,
    OauthDeviceCode,
    OpenUrl,
    CopySecret,
    ManualStep,
    DownloadFile,
    AdminConsentButton,
    #[serde(untagged)]
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupActionStatus {
    Pending,
    Complete,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupAction {
    pub id: String,
    pub kind: SetupActionKind,
    pub label: String,
    pub provider_id: String,
    pub tenant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorize_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub status: SetupActionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(flatten)]
    pub extra: JsonMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupActionStateFile {
    pub provider_id: String,
    pub tenant: String,
    pub team: String,
    pub actions: Vec<SetupAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthStatePayload {
    pub provider_id: String,
    pub tenant: String,
    pub team: String,
    pub action_id: String,
    pub nonce: String,
    pub expires_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthMetadata {
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub authorize_url: Option<String>,
    pub token_url: String,
    #[serde(default)]
    pub redirect_path: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub secret_keys: Vec<String>,
    #[serde(default)]
    pub response_secret_map: BTreeMap<String, String>,
}

pub fn extract_setup_actions(
    provider_id: &str,
    tenant: &str,
    team: Option<&str>,
    value: &Value,
) -> Result<Vec<SetupAction>> {
    let Some(actions) = value.get("setup_actions").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    actions
        .iter()
        .map(|raw| parse_setup_action(provider_id, tenant, team, raw))
        .collect()
}

pub fn strip_setup_actions(value: &Value) -> Value {
    let mut cloned = value.clone();
    if let Some(obj) = cloned.as_object_mut() {
        obj.remove("setup_actions");
        obj.remove("pending_setup_actions");
    }
    cloned
}

pub fn persist_setup_actions(bundle_root: &Path, actions: &[SetupAction]) -> Result<Vec<PathBuf>> {
    let mut grouped: BTreeMap<(String, String, String), Vec<SetupAction>> = BTreeMap::new();
    for action in actions {
        grouped
            .entry((
                action.provider_id.clone(),
                action.tenant.clone(),
                team_segment(action.team.as_deref()).to_string(),
            ))
            .or_default()
            .push(action.clone());
    }

    let mut paths = Vec::new();
    for ((provider_id, tenant, team), new_actions) in grouped {
        let path = setup_actions_state_path(bundle_root, &tenant, &team, &provider_id);
        let mut file = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str::<SetupActionStateFile>(&raw)
                .with_context(|| format!("failed to parse {}", path.display()))?
        } else {
            SetupActionStateFile {
                provider_id: provider_id.clone(),
                tenant: tenant.clone(),
                team: team.clone(),
                actions: Vec::new(),
            }
        };

        for mut action in new_actions {
            if action.created_at.is_none() {
                action.created_at = Some(now_stamp());
            }
            if let Some(existing) = file.actions.iter_mut().find(|a| a.id == action.id) {
                // Persisted action state must never get POORER. Multiple writers
                // persist here (the UI action handler with a complete OAuth
                // authorize URL, and the engine apply/finish flow which
                // regenerates actions without a redirect_uri); writer order must
                // not decide which survives.
                //
                // 1. A `complete` action stays complete — a captured credential
                //    is never rolled back to pending by a later re-apply.
                if existing.status == SetupActionStatus::Complete
                    && action.status == SetupActionStatus::Pending
                {
                    continue;
                }
                // 2. An authorize URL carrying redirect_uri (and its matching
                //    signed state) is never replaced by one without it — a
                //    stripped URL sends the user through the provider's consent
                //    flow with no way back to our callback.
                let existing_has_redirect = existing
                    .authorize_url
                    .as_deref()
                    .is_some_and(|url| url.contains("redirect_uri="));
                let new_has_redirect = action
                    .authorize_url
                    .as_deref()
                    .is_some_and(|url| url.contains("redirect_uri="));
                // ...unless the preserved URL's signed state has expired — an
                // expired state fails callback validation, so a fresh (if
                // stripped) URL is strictly better than a dead complete one.
                let existing_state_live = existing
                    .state
                    .as_deref()
                    .is_some_and(|state| state_not_expired(state, current_epoch_secs()));
                if existing_has_redirect && !new_has_redirect && existing_state_live {
                    action.authorize_url = existing.authorize_url.clone();
                    action.state = existing.state.clone();
                }
                let created_at = existing.created_at.clone().or(action.created_at.clone());
                *existing = action;
                existing.created_at = created_at;
            } else {
                file.actions.push(action);
            }
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let payload = serde_json::to_string_pretty(&file)?;
        std::fs::write(&path, payload)
            .with_context(|| format!("failed to write {}", path.display()))?;
        paths.push(path);
    }
    Ok(paths)
}

/// Whether a signed OAuth `state` token's embedded `expires_at` is still in the
/// future. Signature is NOT checked here — this is only used to decide whether a
/// persisted authorize URL is still clickable; the callback re-validates fully.
fn state_not_expired(state: &str, now_epoch_secs: u64) -> bool {
    let Some(payload_b64) = state.split('.').next() else {
        return false;
    };
    let mut padded = payload_b64.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()
        .or_else(|| {
            base64::engine::general_purpose::URL_SAFE
                .decode(&padded)
                .ok()
        })
        .and_then(|bytes| serde_json::from_slice::<OAuthStatePayload>(&bytes).ok())
        .is_some_and(|payload| payload.expires_at > now_epoch_secs)
}

pub fn sign_pending_oauth_actions(bundle_root: &Path, actions: &mut [SetupAction]) -> Result<()> {
    let key = load_or_create_signing_key(bundle_root)?;
    for action in actions {
        if action.status != SetupActionStatus::Pending
            || action.kind != SetupActionKind::OauthInstallButton
            || action.state.is_some()
        {
            continue;
        }
        let team = team_segment(action.team.as_deref()).to_string();
        let payload = OAuthStatePayload {
            provider_id: action.provider_id.clone(),
            tenant: action.tenant.clone(),
            team,
            action_id: action.id.clone(),
            nonce: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>()),
            expires_at: current_epoch_secs() + 15 * 60,
        };
        let state = sign_oauth_state(&payload, &key)?;
        if let Some(authorize_url) = action.authorize_url.as_mut()
            && !authorize_url_contains_state(authorize_url)
            && let Ok(mut parsed) = url::Url::parse(authorize_url)
        {
            parsed.query_pairs_mut().append_pair("state", &state);
            *authorize_url = parsed.to_string();
        }
        action.state = Some(state);
    }
    Ok(())
}

pub fn load_setup_action(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    action_id: &str,
) -> Result<Option<SetupAction>> {
    let path = setup_actions_state_path(bundle_root, tenant, team, provider_id);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let file: SetupActionStateFile = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(file.actions.into_iter().find(|a| a.id == action_id))
}

pub fn mark_setup_action_complete(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    action_id: &str,
) -> Result<()> {
    let path = setup_actions_state_path(bundle_root, tenant, team, provider_id);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut file: SetupActionStateFile = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let Some(action) = file.actions.iter_mut().find(|a| a.id == action_id) else {
        bail!("setup action not found: {action_id}");
    };
    action.status = SetupActionStatus::Complete;
    action.completed_at = Some(now_stamp());
    let payload = serde_json::to_string_pretty(&file)?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn setup_actions_state_path(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> PathBuf {
    bundle_root
        .join("state")
        .join("config")
        .join("setup-actions")
        .join(tenant)
        .join(team_segment(Some(team)))
        .join(format!("{provider_id}.json"))
}

pub fn signing_key_path(bundle_root: &Path) -> PathBuf {
    bundle_root.join(".greentic").join("setup-oauth-state-key")
}

pub fn load_or_create_signing_key(bundle_root: &Path) -> Result<Vec<u8>> {
    let path = signing_key_path(bundle_root);
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        return URL_SAFE_NO_PAD
            .decode(raw.trim())
            .context("failed to decode setup OAuth state signing key");
    }
    let bytes: [u8; 32] = rand::random();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, URL_SAFE_NO_PAD.encode(bytes))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(bytes.to_vec())
}

pub fn sign_oauth_state(payload: &OAuthStatePayload, key: &[u8]) -> Result<String> {
    let payload_json = serde_json::to_vec(payload)?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json);
    let mut mac = HmacSha256::new_from_slice(key).context("invalid HMAC key")?;
    mac.update(payload_b64.as_bytes());
    let sig = mac.finalize().into_bytes();
    Ok(format!("{payload_b64}.{}", URL_SAFE_NO_PAD.encode(sig)))
}

pub fn validate_oauth_state(
    token: &str,
    key: &[u8],
    expected_provider_id: Option<&str>,
    expected_tenant: Option<&str>,
    expected_team: Option<&str>,
    now_epoch: u64,
) -> Result<OAuthStatePayload> {
    let (payload_b64, sig_b64) = token
        .split_once('.')
        .ok_or_else(|| anyhow!("invalid OAuth state format"))?;
    let sig = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .context("invalid OAuth state signature encoding")?;
    let mut mac = HmacSha256::new_from_slice(key).context("invalid HMAC key")?;
    mac.update(payload_b64.as_bytes());
    mac.verify_slice(&sig)
        .map_err(|_| anyhow!("invalid OAuth state signature"))?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .context("invalid OAuth state payload encoding")?;
    let payload: OAuthStatePayload =
        serde_json::from_slice(&payload_bytes).context("invalid OAuth state payload")?;
    if payload.expires_at <= now_epoch {
        bail!("OAuth state has expired");
    }
    if let Some(expected) = expected_provider_id
        && payload.provider_id != expected
    {
        bail!("OAuth state provider mismatch");
    }
    if let Some(expected) = expected_tenant
        && payload.tenant != expected
    {
        bail!("OAuth state tenant mismatch");
    }
    if let Some(expected) = expected_team
        && payload.team != expected
    {
        bail!("OAuth state team mismatch");
    }
    Ok(payload)
}

pub fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn map_oauth_token_response(
    metadata: &OAuthMetadata,
    response: &Value,
) -> Result<BTreeMap<String, String>> {
    let mut mapped = BTreeMap::new();
    for (secret_key, response_key) in &metadata.response_secret_map {
        if let Some(value) = response.get(response_key).and_then(value_to_string) {
            mapped.insert(secret_key.clone(), value);
        }
    }
    if mapped.is_empty()
        && let Some(token) = response.get("access_token").and_then(value_to_string)
    {
        for key in &metadata.secret_keys {
            mapped.insert(key.clone(), token.clone());
        }
    }
    if mapped.is_empty() {
        bail!("OAuth token response did not contain mappable secrets");
    }
    Ok(mapped)
}

fn parse_setup_action(
    provider_id: &str,
    tenant: &str,
    team: Option<&str>,
    raw: &Value,
) -> Result<SetupAction> {
    let mut obj = raw
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("setup action must be an object"))?;
    let id = take_string(&mut obj, "id").ok_or_else(|| anyhow!("setup action missing id"))?;
    let kind = match take_string(&mut obj, "kind")
        .ok_or_else(|| anyhow!("setup action missing kind"))?
        .as_str()
    {
        "oauth_install_button" => SetupActionKind::OauthInstallButton,
        "oauth_device_code" => SetupActionKind::OauthDeviceCode,
        "open_url" => SetupActionKind::OpenUrl,
        "copy_secret" => SetupActionKind::CopySecret,
        "manual_step" => SetupActionKind::ManualStep,
        "download_file" => SetupActionKind::DownloadFile,
        "admin_consent_button" => SetupActionKind::AdminConsentButton,
        other => SetupActionKind::Other(other.to_string()),
    };
    let label = take_string(&mut obj, "label").unwrap_or_else(|| id.clone());
    let provider_id =
        take_string(&mut obj, "provider_id").unwrap_or_else(|| provider_id.to_string());
    let tenant = take_string(&mut obj, "tenant").unwrap_or_else(|| tenant.to_string());
    let team = take_string(&mut obj, "team").or_else(|| team.map(ToString::to_string));
    let status = match take_string(&mut obj, "status").as_deref() {
        Some("complete") => SetupActionStatus::Complete,
        Some("failed") => SetupActionStatus::Failed,
        _ => SetupActionStatus::Pending,
    };
    Ok(SetupAction {
        id,
        kind,
        label,
        provider_id,
        tenant,
        team,
        authorize_url: take_string(&mut obj, "authorize_url"),
        callback_path: take_string(&mut obj, "callback_path"),
        state: take_string(&mut obj, "state"),
        status,
        created_at: take_string(&mut obj, "created_at"),
        completed_at: take_string(&mut obj, "completed_at"),
        extra: obj,
    })
}

fn take_string(obj: &mut JsonMap<String, Value>, key: &str) -> Option<String> {
    obj.remove(key).and_then(|value| match value {
        Value::String(text) if !text.trim().is_empty() => Some(text),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    })
}

fn team_segment(team: Option<&str>) -> &str {
    team.map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
}

fn now_stamp() -> String {
    current_epoch_secs().to_string()
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn authorize_url_contains_state(value: &str) -> bool {
    url::Url::parse(value)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .any(|(key, _)| key == "state")
                .then_some(())
        })
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oauth_action(id: &str, authorize_url: Option<&str>, state: Option<&str>) -> SetupAction {
        SetupAction {
            id: id.to_string(),
            kind: SetupActionKind::OauthInstallButton,
            label: "Install".to_string(),
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: Some("default".to_string()),
            authorize_url: authorize_url.map(ToString::to_string),
            callback_path: None,
            state: state.map(ToString::to_string),
            status: SetupActionStatus::Pending,
            created_at: None,
            completed_at: None,
            extra: JsonMap::new(),
        }
    }

    fn live_state(bundle: &Path) -> String {
        let key = load_or_create_signing_key(bundle).unwrap();
        sign_oauth_state(
            &OAuthStatePayload {
                provider_id: "messaging-example".into(),
                tenant: "demo".into(),
                team: "default".into(),
                action_id: "install".into(),
                nonce: "n".into(),
                expires_at: current_epoch_secs() + 600,
            },
            &key,
        )
        .unwrap()
    }

    #[test]
    fn persist_never_replaces_complete_authorize_url_with_stripped_one() {
        let temp = tempfile::tempdir().unwrap();
        let state = live_state(temp.path());
        let complete_url = format!(
            "https://slack.com/oauth/v2/authorize?client_id=c&redirect_uri=https%3A%2F%2Fx%2Fcb&state={state}"
        );
        persist_setup_actions(
            temp.path(),
            &[oauth_action("install", Some(&complete_url), Some(&state))],
        )
        .unwrap();
        // Engine re-apply persists a stripped regeneration (no redirect_uri).
        persist_setup_actions(
            temp.path(),
            &[oauth_action(
                "install",
                Some("https://slack.com/oauth/v2/authorize?client_id=c&state=other"),
                Some("other"),
            )],
        )
        .unwrap();
        let action = load_setup_action(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
            "install",
        )
        .unwrap()
        .unwrap();
        assert_eq!(action.authorize_url.as_deref(), Some(complete_url.as_str()));
        assert_eq!(action.state.as_deref(), Some(state.as_str()));
    }

    #[test]
    fn persist_replaces_expired_complete_url_with_fresh_one() {
        let temp = tempfile::tempdir().unwrap();
        let key = load_or_create_signing_key(temp.path()).unwrap();
        let expired = sign_oauth_state(
            &OAuthStatePayload {
                provider_id: "messaging-example".into(),
                tenant: "demo".into(),
                team: "default".into(),
                action_id: "install".into(),
                nonce: "n".into(),
                expires_at: current_epoch_secs().saturating_sub(1),
            },
            &key,
        )
        .unwrap();
        let stale_url =
            format!("https://x/authorize?redirect_uri=https%3A%2F%2Fx%2Fcb&state={expired}");
        persist_setup_actions(
            temp.path(),
            &[oauth_action("install", Some(&stale_url), Some(&expired))],
        )
        .unwrap();
        persist_setup_actions(
            temp.path(),
            &[oauth_action(
                "install",
                Some("https://x/authorize?state=fresh"),
                Some("fresh"),
            )],
        )
        .unwrap();
        let action = load_setup_action(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
            "install",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            action.authorize_url.as_deref(),
            Some("https://x/authorize?state=fresh")
        );
    }

    #[test]
    fn persist_never_downgrades_complete_action_to_pending() {
        let temp = tempfile::tempdir().unwrap();
        let mut done = oauth_action("install", Some("https://x/a"), None);
        done.status = SetupActionStatus::Complete;
        persist_setup_actions(temp.path(), &[done]).unwrap();
        persist_setup_actions(
            temp.path(),
            &[oauth_action("install", Some("https://x/b"), None)],
        )
        .unwrap();
        let action = load_setup_action(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
            "install",
        )
        .unwrap()
        .unwrap();
        assert_eq!(action.status, SetupActionStatus::Complete);
        assert_eq!(action.authorize_url.as_deref(), Some("https://x/a"));
    }

    #[test]
    fn extract_setup_actions_fills_scope_defaults() {
        let value = json!({
            "setup_actions": [{
                "id": "install",
                "kind": "oauth_install_button",
                "label": "Add to Example",
                "authorize_url": "https://example.com/auth"
            }]
        });
        let actions =
            extract_setup_actions("messaging-example", "demo", Some("default"), &value).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].provider_id, "messaging-example");
        assert_eq!(actions[0].tenant, "demo");
        assert_eq!(actions[0].team.as_deref(), Some("default"));
    }

    #[test]
    fn extract_setup_actions_supports_oauth_device_code() {
        let value = json!({
            "setup_actions": [{
                "id": "connect",
                "kind": "oauth_device_code",
                "label": "Connect"
            }]
        });
        let actions =
            extract_setup_actions("messaging-teams", "demo", Some("default"), &value).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, SetupActionKind::OauthDeviceCode);
        assert_eq!(actions[0].provider_id, "messaging-teams");
    }

    #[test]
    fn persist_setup_actions_upserts_by_id() {
        let temp = tempfile::tempdir().unwrap();
        let mut action = SetupAction {
            id: "install".into(),
            kind: SetupActionKind::OauthInstallButton,
            label: "Add".into(),
            provider_id: "messaging-example".into(),
            tenant: "demo".into(),
            team: Some("default".into()),
            authorize_url: Some("https://example.com/one".into()),
            callback_path: None,
            state: None,
            status: SetupActionStatus::Pending,
            created_at: None,
            completed_at: None,
            extra: JsonMap::new(),
        };
        persist_setup_actions(temp.path(), &[action.clone()]).unwrap();
        action.authorize_url = Some("https://example.com/two".into());
        persist_setup_actions(temp.path(), &[action]).unwrap();
        let path = setup_actions_state_path(temp.path(), "demo", "default", "messaging-example");
        let file: SetupActionStateFile =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(file.actions.len(), 1);
        assert_eq!(
            file.actions[0].authorize_url.as_deref(),
            Some("https://example.com/two")
        );
    }

    #[test]
    fn oauth_state_rejects_bad_signature_and_expiry() {
        let key = b"test-key";
        let payload = OAuthStatePayload {
            provider_id: "messaging-example".into(),
            tenant: "demo".into(),
            team: "default".into(),
            action_id: "install".into(),
            nonce: "n".into(),
            expires_at: 100,
        };
        let token = sign_oauth_state(&payload, key).unwrap();
        assert!(validate_oauth_state(&token, key, None, None, None, 99).is_ok());
        assert!(validate_oauth_state(&token, b"other", None, None, None, 99).is_err());
        assert!(validate_oauth_state(&token, key, None, None, None, 100).is_err());
    }

    #[test]
    fn sign_pending_oauth_actions_adds_state_to_action_and_url() {
        let temp = tempfile::tempdir().unwrap();
        let mut actions = vec![SetupAction {
            id: "install".into(),
            kind: SetupActionKind::OauthInstallButton,
            label: "Add".into(),
            provider_id: "messaging-example".into(),
            tenant: "demo".into(),
            team: Some("default".into()),
            authorize_url: Some("https://example.com/oauth?client_id=abc".into()),
            callback_path: Some("/oauth/callback/example".into()),
            state: None,
            status: SetupActionStatus::Pending,
            created_at: None,
            completed_at: None,
            extra: JsonMap::new(),
        }];
        sign_pending_oauth_actions(temp.path(), &mut actions).unwrap();
        let state = actions[0].state.as_deref().unwrap();
        assert!(
            actions[0]
                .authorize_url
                .as_deref()
                .unwrap()
                .contains("state=")
        );
        let key = load_or_create_signing_key(temp.path()).unwrap();
        let payload =
            validate_oauth_state(state, &key, Some("messaging-example"), None, None, 0).unwrap();
        assert_eq!(payload.action_id, "install");
    }

    #[test]
    fn token_response_maps_access_token_to_secret_keys() {
        let metadata = OAuthMetadata {
            token_url: "https://example.com/token".into(),
            secret_keys: vec!["EXAMPLE_TOKEN".into()],
            ..Default::default()
        };
        let mapped = map_oauth_token_response(&metadata, &json!({"access_token": "xoxb"})).unwrap();
        assert_eq!(
            mapped.get("EXAMPLE_TOKEN").map(String::as_str),
            Some("xoxb")
        );
    }
}
