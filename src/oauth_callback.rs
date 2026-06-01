//! Provider-agnostic OAuth callback completion helpers.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

use crate::setup_actions::{OAuthMetadata, SetupActionStatus};

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
    let action = crate::setup_actions::load_setup_action(
        bundle_root,
        &state.tenant,
        &state.team,
        &state.provider_id,
        &state.action_id,
    )?
    .ok_or_else(|| anyhow::anyhow!("setup action not found: {}", state.action_id))?;
    if action.status != SetupActionStatus::Pending {
        bail!("setup action is not pending");
    }

    let metadata = load_provider_oauth_metadata(bundle_root, &state.provider_id, extension_key)?;
    let mapped = crate::setup_actions::map_oauth_token_response(&metadata, token_response)?;
    let config = Value::Object(
        mapped
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect::<JsonMap<_, _>>(),
    );

    crate::qa::persist::persist_all_config_as_secrets(
        bundle_root,
        env,
        &state.tenant,
        Some(&state.team),
        &state.provider_id,
        &config,
        None,
    )
    .await?;
    crate::setup_actions::mark_setup_action_complete(
        bundle_root,
        &state.tenant,
        &state.team,
        &state.provider_id,
        &state.action_id,
    )?;

    Ok(OAuthCallbackReport {
        provider_id: state.provider_id,
        tenant: state.tenant,
        team: state.team,
        action_id: state.action_id,
        persisted_secret_keys: mapped.keys().cloned().collect(),
    })
}

pub async fn complete_oauth_callback(
    bundle_root: &Path,
    env: &str,
    input: &OAuthCallbackInput,
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
    let action = crate::setup_actions::load_setup_action(
        bundle_root,
        &state.tenant,
        &state.team,
        &state.provider_id,
        &state.action_id,
    )?
    .ok_or_else(|| anyhow::anyhow!("setup action not found: {}", state.action_id))?;
    if action.status != SetupActionStatus::Pending {
        bail!("setup action is not pending");
    }

    let metadata = load_provider_oauth_metadata(bundle_root, &state.provider_id, extension_key)?;
    let callback_path = action
        .callback_path
        .as_deref()
        .or(metadata.redirect_path.as_deref())
        .ok_or_else(|| anyhow::anyhow!("OAuth callback path is missing"))?;
    let public_base_url = resolve_public_base_url(
        bundle_root,
        &state.tenant,
        Some(&state.team),
        &state.provider_id,
    )?;
    let redirect_uri = format!(
        "{}{}",
        public_base_url.trim_end_matches('/'),
        ensure_leading_slash(callback_path)
    );
    let setup_answers = load_provider_setup_answers(bundle_root, &state.provider_id)?;
    let client_id = first_nonempty(&setup_answers, &["client_id", "oauth_client_id"])
        .ok_or_else(|| anyhow::anyhow!("OAuth client_id is missing from provider setup answers"))?;
    let client_secret = first_nonempty(&setup_answers, &["client_secret", "oauth_client_secret"])
        .ok_or_else(|| {
        anyhow::anyhow!("OAuth client_secret is missing from provider setup answers")
    })?;

    let token_response = exchange_oauth_code(
        &metadata,
        input.code.trim(),
        &redirect_uri,
        &client_id,
        &client_secret,
    )?;

    complete_oauth_callback_with_token_response(
        bundle_root,
        env,
        input,
        &token_response,
        extension_key,
    )
    .await
}

pub fn exchange_oauth_code(
    metadata: &OAuthMetadata,
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    client_secret: &str,
) -> Result<Value> {
    let mut response = ureq::post(&metadata.token_url)
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
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    fn write_provider_pack(path: &Path) -> anyhow::Result<()> {
        write_provider_pack_with_manifest(
            path,
            json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "messaging.oauth.v1": {
                        "token_url": "https://example.com/token",
                        "secret_keys": ["EXAMPLE_TOKEN"]
                    }
                }
            }),
        )
    }

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

    fn persist_action(
        bundle: &Path,
        action_id: &str,
        status: crate::setup_actions::SetupActionStatus,
        callback_path: Option<&str>,
    ) -> anyhow::Result<String> {
        let state = signed_state(bundle, action_id)?;
        let mut actions = crate::setup_actions::extract_setup_actions(
            "messaging-example",
            "demo",
            Some("default"),
            &json!({
                "setup_actions": [{
                    "id": action_id,
                    "kind": "oauth_install_button",
                    "label": "Add",
                    "authorize_url": "https://example.com/auth",
                    "callback_path": callback_path,
                    "state": state,
                    "status": status
                }]
            }),
        )?;
        actions[0].status = status;
        crate::setup_actions::persist_setup_actions(bundle, &actions)?;
        Ok(state)
    }

    #[tokio::test]
    async fn callback_maps_token_to_secret_and_marks_action_complete() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack(&bundle.join("providers/messaging/messaging-example.gtpack"))?;

        let state = persist_action(
            bundle,
            "install",
            crate::setup_actions::SetupActionStatus::Pending,
            None,
        )?;

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
        let action = crate::setup_actions::load_setup_action(
            bundle,
            "demo",
            "default",
            "messaging-example",
            "install",
        )?
        .unwrap();
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
        assert_eq!(String::from_utf8(bytes)?, "token-value");
        Ok(())
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
    async fn callback_rejects_empty_code_missing_action_and_completed_action() -> anyhow::Result<()>
    {
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
        assert!(err.contains("setup action not found"));

        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack(&bundle.join("providers/messaging/messaging-example.gtpack"))?;
        let state = persist_action(
            bundle,
            "install",
            crate::setup_actions::SetupActionStatus::Complete,
            None,
        )?;
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
        assert!(err.contains("not pending"));
        Ok(())
    }

    #[tokio::test]
    async fn live_callback_validates_before_network_exchange() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle = temp.path();
        std::fs::create_dir_all(bundle.join("providers/messaging"))?;
        write_provider_pack(&bundle.join("providers/messaging/messaging-example.gtpack"))?;
        let state = persist_action(
            bundle,
            "install",
            crate::setup_actions::SetupActionStatus::Pending,
            None,
        )?;

        let err = complete_oauth_callback(
            bundle,
            "dev",
            &OAuthCallbackInput {
                code: " ".into(),
                state: state.clone(),
            },
            "messaging.oauth.v1",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("missing code"));

        let err = complete_oauth_callback(
            bundle,
            "dev",
            &OAuthCallbackInput {
                code: "code".into(),
                state,
            },
            "messaging.oauth.v1",
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(err.contains("callback path is missing"));
        Ok(())
    }
}
