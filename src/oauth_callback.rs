//! Provider-agnostic OAuth callback completion helpers.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

use crate::setup_actions::{OAuthMetadata, OAuthStatePayload, SetupAction, SetupActionKind};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthCallbackInput {
    pub code: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthCallbackReport {
    pub provider_id: String,
    pub tenant: String,
    pub team: String,
    pub action_id: String,
    pub persisted_secret_keys: Vec<String>,
}

pub fn load_provider_oauth_metadata(
    bundle_root: &Path,
    provider_id: &str,
    extension_key: &str,
) -> Result<OAuthMetadata> {
    let discovered = crate::discovery::discover(bundle_root)
        .context("failed to discover providers for OAuth callback")?;
    let provider = discovered
        .find_setup_target(provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider not found for OAuth callback: {provider_id}"))?;
    let raw = crate::discovery::read_pack_extension(&provider.pack_path, extension_key)?
        .ok_or_else(|| anyhow::anyhow!("provider missing OAuth metadata: {extension_key}"))?;
    let metadata = raw.get("inline").cloned().unwrap_or(raw);
    serde_json::from_value(metadata).context("failed to parse provider OAuth metadata")
}

pub async fn complete_oauth_callback_with_token_response(
    bundle_root: &Path,
    env: &str,
    input: &OAuthCallbackInput,
    token_response: &Value,
    extension_key: &str,
) -> Result<OAuthCallbackReport> {
    if input.code.trim().is_empty() {
        bail!("OAuth callback missing code");
    }
    let key = crate::setup_actions::load_or_create_signing_key(bundle_root)?;
    let state = crate::setup_actions::validate_oauth_state(
        &input.state,
        &key,
        None,
        None,
        None,
        crate::setup_actions::current_epoch_secs(),
    )?;
    let machine_result =
        crate::setup_machine::complete_setup_machine_oauth_authorization_code_with_token_response(
            bundle_root,
            env,
            &state,
            token_response,
        )
        .await;
    match machine_result {
        Ok(report) => Ok(report),
        Err(machine_err) => match complete_setup_action_oauth_callback_with_token_response(
            bundle_root,
            env,
            &state,
            token_response,
            extension_key,
        )
        .await?
        {
            Some(report) => Ok(report),
            None => Err(machine_err),
        },
    }
}

pub async fn complete_setup_action_oauth_callback_with_token_response(
    bundle_root: &Path,
    env: &str,
    state: &OAuthStatePayload,
    token_response: &Value,
    extension_key: &str,
) -> Result<Option<OAuthCallbackReport>> {
    let Some(action) = crate::setup_actions::load_setup_action(
        bundle_root,
        &state.tenant,
        &state.team,
        &state.provider_id,
        &state.action_id,
    )?
    else {
        return Ok(None);
    };
    if action.kind != SetupActionKind::OauthInstallButton {
        bail!("setup action is not an oauth_install_button: {}", action.id);
    }

    let metadata = load_provider_oauth_metadata(bundle_root, &state.provider_id, extension_key)?;
    persist_setup_action_oauth_token_response(
        bundle_root,
        env,
        state,
        &action,
        &metadata,
        token_response,
    )
    .await
    .map(Some)
}

/// The setup server's public base URL as declared by the operator's environment.
///
/// Used where no live setup-tunnel handle is available (e.g. the headless
/// `no_ui_oauth` callback listener). The UI path resolves the active tunnel first
/// and only falls back to this.
pub fn env_setup_callback_base() -> Option<String> {
    std::env::var("GREENTIC_SETUP_PUBLIC_BASE_URL")
        .ok()
        .or_else(|| std::env::var("GREENTIC_PUBLIC_BASE_URL").ok())
        .or_else(|| std::env::var("PUBLIC_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| value.starts_with("https://"))
}

/// `setup_callback_base`: the setup server's public base URL (live setup tunnel or
/// env override). The exchange's `redirect_uri` MUST equal the one the authorize
/// link used and the one registered in the app manifest, or the provider rejects
/// the exchange (Slack: `bad_redirect_uri`). `None` falls back to the legacy
/// `resolve_public_base_url` behaviour.
pub async fn complete_oauth_callback(
    bundle_root: &Path,
    env: &str,
    input: &OAuthCallbackInput,
    extension_key: &str,
    setup_callback_base: Option<&str>,
) -> Result<OAuthCallbackReport> {
    if input.code.trim().is_empty() {
        bail!("OAuth callback missing code");
    }
    let key = crate::setup_actions::load_or_create_signing_key(bundle_root)?;
    let state = crate::setup_actions::validate_oauth_state(
        &input.state,
        &key,
        None,
        None,
        None,
        crate::setup_actions::current_epoch_secs(),
    )?;
    eprintln!(
        "[oauth-token] callback received: provider={} tenant={}/{} action={} code_len={}",
        state.provider_id,
        state.tenant,
        state.team,
        state.action_id,
        input.code.trim().len()
    );
    let machine_result = crate::setup_machine::complete_setup_machine_oauth_authorization_code(
        bundle_root,
        env,
        &state,
        input.code.trim(),
    )
    .await;
    match machine_result {
        Ok(report) => Ok(report),
        Err(machine_err) => match complete_setup_action_oauth_callback(
            bundle_root,
            env,
            &state,
            input.code.trim(),
            extension_key,
            setup_callback_base,
        )
        .await?
        {
            Some(report) => Ok(report),
            None => Err(machine_err),
        },
    }
}

pub async fn complete_setup_action_oauth_callback(
    bundle_root: &Path,
    env: &str,
    state: &OAuthStatePayload,
    code: &str,
    extension_key: &str,
    setup_callback_base: Option<&str>,
) -> Result<Option<OAuthCallbackReport>> {
    let Some(action) = crate::setup_actions::load_setup_action(
        bundle_root,
        &state.tenant,
        &state.team,
        &state.provider_id,
        &state.action_id,
    )?
    else {
        return Ok(None);
    };
    if action.kind != SetupActionKind::OauthInstallButton {
        bail!("setup action is not an oauth_install_button: {}", action.id);
    }

    let metadata = load_provider_oauth_metadata(bundle_root, &state.provider_id, extension_key)?;
    eprintln!(
        "[oauth-token] {extension_key} metadata: token_url={} secret_keys={:?} response_secret_map_keys={:?}",
        metadata.token_url,
        metadata.secret_keys,
        metadata.response_secret_map.keys().collect::<Vec<_>>()
    );
    let client_id = load_setup_action_secret(
        bundle_root,
        env,
        state,
        &setup_action_client_id_keys(&action),
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("setup action OAuth callback missing client id"))?;
    let client_secret = load_setup_action_secret(
        bundle_root,
        env,
        state,
        &setup_action_client_secret_keys(&action),
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("setup action OAuth callback missing client secret"))?;
    // The callback is served by the setup server, so the exchange's redirect_uri must
    // be built from the setup server's base — the same one the authorize link used and
    // the app manifest registered. Only fall back to the provider's (runtime)
    // public_base_url when no setup base was resolved.
    let public_base_url = match setup_callback_base
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(base) => base.to_string(),
        None => {
            match resolve_public_base_url(bundle_root, &state.tenant, Some(&state.team), &state.provider_id)
            {
                Ok(value) => value,
                Err(_) => load_setup_action_secret(
                    bundle_root,
                    env,
                    state,
                    &["public_base_url".to_string()],
                )
                .await?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "This provider requires a public_base_url to generate OAuth callback and webhook URLs."
                    )
                })?,
            }
        }
    };
    let redirect_path = setup_action_string(&action, "redirect_path")
        .or_else(|| action.callback_path.clone())
        .or_else(|| metadata.redirect_path.clone())
        .ok_or_else(|| anyhow::anyhow!("setup action OAuth callback missing redirect path"))?;
    let redirect_uri = format!(
        "{}{}",
        public_base_url.trim().trim_end_matches('/'),
        ensure_leading_slash(&redirect_path)
    );
    eprintln!(
        "[oauth-token] exchanging code at {} (redirect_uri={redirect_uri}, client_id={}, client_secret_present={})",
        metadata.token_url,
        client_id.trim(),
        !client_secret.trim().is_empty()
    );
    let token_response = exchange_oauth_code(
        &metadata,
        code.trim(),
        &redirect_uri,
        client_id.trim(),
        client_secret.trim(),
    )?;
    let tr_ok = token_response.get("ok").and_then(Value::as_bool);
    let tr_err = token_response.get("error").and_then(Value::as_str);
    let token_prefix = token_response
        .get("access_token")
        .and_then(Value::as_str)
        .map(|t| t.chars().take(5).collect::<String>());
    eprintln!(
        "[oauth-token] exchange response: ok={tr_ok:?} error={tr_err:?} access_token_prefix={token_prefix:?}"
    );
    persist_setup_action_oauth_token_response(
        bundle_root,
        env,
        state,
        &action,
        &metadata,
        &token_response,
    )
    .await
    .map(Some)
}

pub fn exchange_oauth_code(
    metadata: &OAuthMetadata,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<Value> {
    let mut response = crate::http_client::api_agent()
        .post(&metadata.token_url)
        .send_form([
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("client_secret", client_secret),
        ])
        .context("OAuth token exchange failed")?;
    response
        .body_mut()
        .read_json::<Value>()
        .context("failed to parse OAuth token response")
}

pub fn resolve_public_base_url(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
) -> Result<String> {
    if let Some(value) = load_provider_setup_answers(bundle_root, provider_id)?
        .get("public_base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(value.to_string());
    }

    if let Some(policy) =
        crate::platform_setup::load_effective_static_routes_defaults(bundle_root, tenant, team)?
        && let Some(value) = policy.public_base_url
    {
        return Ok(value);
    }

    bail!("This provider requires a public_base_url to generate OAuth callback and webhook URLs.")
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

async fn persist_setup_action_oauth_token_response(
    bundle_root: &Path,
    env: &str,
    state: &OAuthStatePayload,
    action: &SetupAction,
    metadata: &OAuthMetadata,
    token_response: &Value,
) -> Result<OAuthCallbackReport> {
    let mapped = crate::setup_actions::map_oauth_token_response(metadata, token_response)?;
    eprintln!(
        "[oauth-token] mapped token response -> secret key(s): {:?}",
        mapped.keys().collect::<Vec<_>>()
    );
    let config = mapped
        .into_iter()
        .map(|(key, value)| (key, Value::String(value)))
        .collect::<JsonMap<String, Value>>();
    let persisted_keys = crate::qa::persist::persist_all_config_as_secrets(
        bundle_root,
        env,
        &state.tenant,
        Some(&state.team),
        &state.provider_id,
        &Value::Object(config),
        None,
    )
    .await?;
    eprintln!(
        "[oauth-token] persisted {:?} for {} ({}/{}); action {} -> complete",
        persisted_keys, state.provider_id, state.tenant, state.team, action.id
    );
    crate::setup_actions::mark_setup_action_complete(
        bundle_root,
        &state.tenant,
        &state.team,
        &state.provider_id,
        &action.id,
    )?;
    Ok(OAuthCallbackReport {
        provider_id: state.provider_id.clone(),
        tenant: state.tenant.clone(),
        team: state.team.clone(),
        action_id: action.id.clone(),
        persisted_secret_keys: persisted_keys,
    })
}

async fn load_setup_action_secret(
    bundle_root: &Path,
    env: &str,
    state: &OAuthStatePayload,
    keys: &[String],
) -> Result<Option<String>> {
    use greentic_secrets_lib::SecretsStore;

    let store = crate::secrets::open_dev_store(bundle_root)?;
    for key in keys {
        let uri = crate::canonical_secret_uri(
            env,
            &state.tenant,
            Some(&state.team),
            &state.provider_id,
            key,
        );
        if let Ok(bytes) = store.get(&uri).await {
            let value = String::from_utf8(bytes).context("setup action secret is not utf-8")?;
            if !value.trim().is_empty() {
                return Ok(Some(value));
            }
        }
    }
    Ok(None)
}

fn setup_action_client_id_keys(action: &SetupAction) -> Vec<String> {
    setup_action_key_candidates(
        action,
        &["client_id_field", "client_id_output"],
        &["client_id", "oauth_client_id"],
    )
}

fn setup_action_client_secret_keys(action: &SetupAction) -> Vec<String> {
    setup_action_key_candidates(
        action,
        &["client_secret_field", "client_secret_output"],
        &["client_secret", "oauth_client_secret"],
    )
}

fn setup_action_key_candidates(
    action: &SetupAction,
    direct_keys: &[&str],
    defaults: &[&str],
) -> Vec<String> {
    let mut keys = Vec::new();
    for key in direct_keys {
        if let Some(value) = setup_action_string(action, key) {
            push_unique(&mut keys, value);
        }
        if let Some(value) = action
            .extra
            .get("registration")
            .and_then(|registration| registration.get(*key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
        {
            push_unique(&mut keys, value);
        }
    }
    for key in defaults {
        push_unique(&mut keys, (*key).to_string());
    }
    keys
}

fn setup_action_string(action: &SetupAction, key: &str) -> Option<String> {
    action
        .extra
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[cfg(test)]
fn first_nonempty(value: &Value, keys: &[&str]) -> Option<String> {
    let obj = value.as_object()?;
    keys.iter().find_map(|key| {
        obj.get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn ensure_leading_slash(value: &str) -> String {
    if value.starts_with('/') {
        value.to_string()
    } else {
        format!("/{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_secrets_lib::SecretsStore;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;
    use zip::write::{FileOptions, ZipWriter};

    fn write_provider_pack_with_manifest(
        path: &Path,
        manifest: serde_json::Value,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(manifest.to_string().as_bytes())?;
        writer.finish()?;
        Ok(())
    }

    fn persist_provider_answers(
        bundle: &Path,
        provider_id: &str,
        answers: serde_json::Value,
    ) -> anyhow::Result<()> {
        let dir = bundle.join("state/config").join(provider_id);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("setup-answers.json"), answers.to_string())?;
        Ok(())
    }

    fn signed_state(bundle: &Path, action_id: &str) -> anyhow::Result<String> {
        let key = crate::setup_actions::load_or_create_signing_key(bundle)?;
        let state_payload = crate::setup_actions::OAuthStatePayload {
            provider_id: "messaging-example".into(),
            tenant: "demo".into(),
            team: "default".into(),
            action_id: action_id.into(),
            nonce: "nonce".into(),
            expires_at: crate::setup_actions::current_epoch_secs() + 60,
        };
        crate::setup_actions::sign_oauth_state(&state_payload, &key)
    }

    #[test]
    fn load_provider_oauth_metadata_reports_missing_provider_and_extension() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack_with_manifest(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
            json!({"pack_id": "messaging-example"}),
        )?;

        let missing_provider =
            load_provider_oauth_metadata(bundle, "messaging-missing", "messaging.oauth.v1")
                .unwrap_err()
                .to_string();
        assert!(missing_provider.contains("provider not found"));

        let missing_extension =
            load_provider_oauth_metadata(bundle, "messaging-example", "messaging.oauth.v1")
                .unwrap_err()
                .to_string();
        assert!(missing_extension.contains("missing OAuth metadata"));
        Ok(())
    }

    #[test]
    fn load_provider_oauth_metadata_accepts_inline_extension_wrapper() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack_with_manifest(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
            json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "messaging.oauth.v1": {
                        "kind": "messaging.oauth.v1",
                        "inline": {
                            "token_url": "https://example.com/token",
                            "secret_keys": ["EXAMPLE_TOKEN"]
                        }
                    }
                }
            }),
        )?;

        let metadata =
            load_provider_oauth_metadata(bundle, "messaging-example", "messaging.oauth.v1")?;

        assert_eq!(metadata.token_url, "https://example.com/token");
        assert_eq!(metadata.secret_keys, vec!["EXAMPLE_TOKEN"]);
        Ok(())
    }

    #[test]
    fn resolve_public_base_url_prefers_provider_answer() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        persist_provider_answers(
            bundle,
            "messaging-example",
            json!({"public_base_url": "https://provider.example.com"}),
        )?;

        let resolved =
            resolve_public_base_url(bundle, "demo", Some("default"), "messaging-example")?;
        assert_eq!(resolved, "https://provider.example.com");
        Ok(())
    }

    #[test]
    fn resolve_public_base_url_uses_static_routes_and_runtime_fallback() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        crate::platform_setup::persist_static_routes_artifact(
            bundle,
            &crate::platform_setup::StaticRoutesPolicy {
                public_base_url: Some("https://static.example.com".into()),
                ..crate::platform_setup::StaticRoutesPolicy::default()
            },
        )?;
        let resolved =
            resolve_public_base_url(bundle, "demo", Some("default"), "messaging-example")?;
        assert_eq!(resolved, "https://static.example.com");

        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        let runtime_dir = bundle.join("state/runtime/demo.default");
        std::fs::create_dir_all(&runtime_dir)?;
        std::fs::write(
            runtime_dir.join("endpoints.json"),
            json!({"public_base_url": "https://runtime.example.com"}).to_string(),
        )?;
        let resolved =
            resolve_public_base_url(bundle, "demo", Some("default"), "messaging-example")?;
        assert_eq!(resolved, "https://runtime.example.com");
        Ok(())
    }

    #[test]
    fn resolve_public_base_url_errors_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let err =
            resolve_public_base_url(temp.path(), "demo", Some("default"), "messaging-example")
                .unwrap_err()
                .to_string();
        assert!(err.contains("requires a public_base_url"));
    }

    #[test]
    fn setup_answer_helpers_handle_missing_file_and_nonempty_aliases() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let empty = load_provider_setup_answers(temp.path(), "messaging-example")?;
        assert_eq!(empty, Value::Object(JsonMap::new()));

        let answers = json!({"client_id": "  ", "oauth_client_id": "client"});
        assert_eq!(
            first_nonempty(&answers, &["client_id", "oauth_client_id"]).as_deref(),
            Some("client")
        );
        assert_eq!(ensure_leading_slash("oauth/callback"), "/oauth/callback");
        assert_eq!(ensure_leading_slash("/oauth/callback"), "/oauth/callback");
        Ok(())
    }

    #[tokio::test]
    async fn callback_rejects_empty_code_and_missing_setup_machine() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        let state = signed_state(bundle, "missing")?;
        let err = complete_oauth_callback_with_token_response(
            bundle,
            "dev",
            &OAuthCallbackInput {
                code: " ".into(),
                state: state.clone(),
            },
            &json!({"access_token": "token"}),
            "messaging.oauth.v1",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing code"));

        let err = complete_oauth_callback_with_token_response(
            bundle,
            "dev",
            &OAuthCallbackInput {
                code: "code".into(),
                state,
            },
            &json!({"access_token": "token"}),
            "messaging.oauth.v1",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("provider not found for setup-machine OAuth callback"));
        Ok(())
    }

    #[tokio::test]
    async fn callback_without_legacy_action_completes_setup_machine_oauth_step()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack_with_manifest(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
            json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "greentic.setup.machine.v1": {
                        "inline": {
                            "version": 1,
                            "id": "example-machine",
                            "entry_step": "oauth",
                            "steps": [
                                {
                                    "id": "oauth",
                                    "kind": "oauth_authorization_code",
                                    "authorize_url": "https://login.example.com/oauth2/v2.0/authorize",
                                    "token_url": "https://login.example.com/oauth2/v2.0/token",
                                    "token_store_key": "EXAMPLE_TOKEN",
                                    "output_key": "oauth_result",
                                    "on_success": "complete"
                                }
                            ]
                        }
                    }
                }
            }),
        )?;
        let machine = crate::setup_machine::load_setup_machine_from_pack(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
        )?
        .expect("setup machine");
        crate::setup_machine::write_setup_machine_state(
            bundle,
            &crate::setup_machine::SetupMachineState {
                schema_version: 1,
                provider_id: "messaging-example".to_string(),
                tenant: "demo".to_string(),
                team: "default".to_string(),
                machine_id: machine.id.clone(),
                machine_version: machine.version,
                status: crate::setup_machine::SetupMachineStatus::Paused,
                current_step: Some("oauth".to_string()),
                completed_steps: Vec::new(),
                failed_step: None,
                answers_hash: None,
                pack_fingerprint: None,
                outputs: json!({}),
                created_resources: Vec::new(),
                last_error: None,
                updated_at: None,
            },
        )?;
        let state = signed_state(bundle, "oauth")?;

        let report = complete_oauth_callback_with_token_response(
            bundle,
            "dev",
            &OAuthCallbackInput {
                code: "code".into(),
                state,
            },
            &json!({"access_token": "token-value"}),
            "messaging.oauth.v1",
        )
        .await?;

        assert_eq!(report.persisted_secret_keys, vec!["EXAMPLE_TOKEN"]);
        let state = crate::setup_machine::load_setup_machine_state(
            &crate::setup_machine::setup_machine_state_path(
                bundle,
                "demo",
                "default",
                "messaging-example",
            ),
        )?;
        assert_eq!(
            state.status,
            crate::setup_machine::SetupMachineStatus::Complete
        );
        let store = crate::secrets::open_dev_store(bundle)?;
        let uri = crate::canonical_secret_uri(
            "dev",
            "demo",
            Some("default"),
            "messaging-example",
            "EXAMPLE_TOKEN",
        );
        let bytes = store.get(&uri).await?;
        assert_eq!(String::from_utf8(bytes)?, "token-value");
        Ok(())
    }

    #[tokio::test]
    async fn callback_completes_setup_action_oauth_install_without_setup_machine()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack_with_manifest(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
            json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "messaging.oauth.v1": {
                        "inline": {
                            "token_url": "https://example.com/token",
                            "secret_keys": ["EXAMPLE_TOKEN"]
                        }
                    }
                }
            }),
        )?;
        crate::setup_actions::persist_setup_actions(
            bundle,
            &[crate::setup_actions::SetupAction {
                id: "install".to_string(),
                kind: crate::setup_actions::SetupActionKind::OauthInstallButton,
                label: "Install app".to_string(),
                provider_id: "messaging-example".to_string(),
                tenant: "demo".to_string(),
                team: Some("default".to_string()),
                authorize_url: None,
                callback_path: None,
                state: None,
                status: crate::setup_actions::SetupActionStatus::Pending,
                created_at: None,
                completed_at: None,
                extra: JsonMap::new(),
            }],
        )?;
        let state = signed_state(bundle, "install")?;

        let report = complete_oauth_callback_with_token_response(
            bundle,
            "dev",
            &OAuthCallbackInput {
                code: "code".into(),
                state,
            },
            &json!({"access_token": "install-token"}),
            "messaging.oauth.v1",
        )
        .await?;

        assert_eq!(report.provider_id, "messaging-example");
        assert_eq!(report.action_id, "install");
        assert_eq!(report.persisted_secret_keys, vec!["EXAMPLE_TOKEN"]);
        let action = crate::setup_actions::load_setup_action(
            bundle,
            "demo",
            "default",
            "messaging-example",
            "install",
        )?
        .expect("setup action");
        assert_eq!(
            action.status,
            crate::setup_actions::SetupActionStatus::Complete
        );
        let store = crate::secrets::open_dev_store(bundle)?;
        let uri = crate::canonical_secret_uri(
            "dev",
            "demo",
            Some("default"),
            "messaging-example",
            "EXAMPLE_TOKEN",
        );
        let bytes = store.get(&uri).await?;
        assert_eq!(String::from_utf8(bytes)?, "install-token");
        Ok(())
    }

    #[tokio::test]
    async fn live_callback_without_legacy_action_exchanges_setup_machine_oauth_code()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let token_url = format!("http://{}/token", listener.local_addr()?);
        let server_handle = thread::spawn(move || -> anyhow::Result<()> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut buffer = [0_u8; 8192];
            let mut bytes = Vec::new();
            loop {
                let read = match stream.read(&mut buffer) {
                    Ok(read) => read,
                    Err(err)
                        if matches!(
                            err.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break;
                    }
                    Err(err) => return Err(err.into()),
                };
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
                let request = String::from_utf8_lossy(&bytes);
                let Some((headers, body)) = request.split_once("\r\n\r\n") else {
                    continue;
                };
                let content_length = headers
                    .lines()
                    .find_map(|line| line.split_once(':'))
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(usize::MAX);
                if body.len() >= content_length
                    || (body.contains("grant_type=")
                        && body.contains("code=callback-code")
                        && body.contains("client_id=client-123"))
                {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&bytes);
            assert!(request.starts_with("POST /token "));
            assert!(request.contains("grant_type=authorization_code"));
            assert!(request.contains("code=callback-code"));
            assert!(request.contains("client_id=client-123"));
            let body = r#"{"access_token":"live-token"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes())?;
            Ok(())
        });

        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack_with_manifest(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
            json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "greentic.setup.machine.v1": {
                        "inline": {
                            "version": 1,
                            "id": "example-machine",
                            "entry_step": "oauth",
                            "steps": [
                                {
                                    "id": "oauth",
                                    "kind": "oauth_authorization_code",
                                    "authorize_url": "https://login.example.com/oauth2/v2.0/authorize",
                                    "token_url": token_url,
                                    "client_id": "client-123",
                                    "redirect_uri": "https://runtime.example.com/oauth/callback",
                                    "token_store_key": "EXAMPLE_TOKEN",
                                    "on_success": "complete"
                                }
                            ]
                        }
                    }
                }
            }),
        )?;
        let machine = crate::setup_machine::load_setup_machine_from_pack(
            &bundle.join("providers/messaging/messaging-example.gtpack"),
        )?
        .expect("setup machine");
        crate::setup_machine::write_setup_machine_state(
            bundle,
            &crate::setup_machine::SetupMachineState {
                schema_version: 1,
                provider_id: "messaging-example".to_string(),
                tenant: "demo".to_string(),
                team: "default".to_string(),
                machine_id: machine.id.clone(),
                machine_version: machine.version,
                status: crate::setup_machine::SetupMachineStatus::Paused,
                current_step: Some("oauth".to_string()),
                completed_steps: Vec::new(),
                failed_step: None,
                answers_hash: None,
                pack_fingerprint: None,
                outputs: json!({}),
                created_resources: Vec::new(),
                last_error: None,
                updated_at: None,
            },
        )?;
        let state = signed_state(bundle, "oauth")?;

        let report = complete_oauth_callback(
            bundle,
            "dev",
            &OAuthCallbackInput {
                code: "callback-code".into(),
                state,
            },
            "messaging.oauth.v1",
            None,
        )
        .await?;
        server_handle.join().unwrap()?;

        assert_eq!(report.persisted_secret_keys, vec!["EXAMPLE_TOKEN"]);
        let store = crate::secrets::open_dev_store(bundle)?;
        let uri = crate::canonical_secret_uri(
            "dev",
            "demo",
            Some("default"),
            "messaging-example",
            "EXAMPLE_TOKEN",
        );
        let bytes = store.get(&uri).await?;
        assert_eq!(String::from_utf8(bytes)?, "live-token");
        Ok(())
    }
}
