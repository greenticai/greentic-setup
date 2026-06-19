//! Provider-agnostic OAuth callback completion helpers.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};

use crate::setup_actions::OAuthMetadata;

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
    let _ = extension_key;
    crate::setup_machine::complete_setup_machine_oauth_authorization_code_with_token_response(
        bundle_root,
        env,
        &state,
        token_response,
    )
    .await
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
    let _ = extension_key;
    crate::setup_machine::complete_setup_machine_oauth_authorization_code(
        bundle_root,
        env,
        &state,
        input.code.trim(),
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

#[cfg(test)]
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
