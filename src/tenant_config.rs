//! Tenant config synchronization for webchat-gui OAuth settings.
//!
//! After setup persists OAuth answers to secrets, this module updates the
//! static tenant config JSON (`assets/webchat-gui/config/tenants/<tenant>.json`)
//! to enable/disable OAuth providers and set client IDs. This ensures the
//! webchat-gui runtime serves the correct auth config without manual editing.

use std::path::Path;

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

/// Synchronize webchat-gui OAuth answers to the tenant config JSON.
///
/// Only runs for `messaging-webchat-gui` providers. Updates the tenant config
/// at `<bundle>/assets/webchat-gui/config/tenants/<tenant>.json`.
pub fn sync_oauth_to_tenant_config(
    bundle_path: &Path,
    tenant: &str,
    provider_id: &str,
    answers: &Value,
) -> Result<bool> {
    // Only apply to webchat-gui providers
    if !provider_id.contains("webchat-gui") {
        return Ok(false);
    }

    let answers_obj = match answers.as_object() {
        Some(m) => m,
        None => return Ok(false),
    };

    // Check if OAuth is configured in answers
    let oauth_enabled = answers_obj
        .get("oauth_enabled")
        .and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true")))
        .unwrap_or(false);

    // Find tenant config file
    let config_path = bundle_path
        .join("assets/webchat-gui/config/tenants")
        .join(format!("{tenant}.json"));

    if !config_path.exists() {
        // Try default.json as fallback
        let default_path = bundle_path.join("assets/webchat-gui/config/tenants/default.json");
        if default_path.exists() {
            return update_tenant_config(
                &default_path,
                tenant,
                oauth_enabled,
                answers_obj,
                resolve_public_base_url(bundle_path, tenant, answers_obj)?,
            );
        }
        return Ok(false);
    }

    update_tenant_config(
        &config_path,
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

    let tenant_path = bundle_path
        .join("assets/webchat-gui/config/tenants")
        .join(format!("{tenant}.json"));
    let target = if tenant_path.exists() {
        tenant_path
    } else {
        bundle_path.join("assets/webchat-gui/config/tenants/default.json")
    };

    if !target.exists() {
        return Ok(false);
    }

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

#[cfg(test)]
mod tests {
    use super::{
        is_placeholder_public_base_url, resolve_public_base_url, sync_skin_to_tenant_config,
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
    fn sync_skin_falls_back_to_default_json_when_tenant_missing() {
        let temp = tempfile::tempdir().unwrap();
        let tenants_dir = temp.path().join("assets/webchat-gui/config/tenants");
        std::fs::create_dir_all(&tenants_dir).unwrap();
        let default_file = tenants_dir.join("default.json");
        std::fs::write(&default_file, r#"{"tenant_id":"default"}"#).unwrap();

        let answers = json!({ "skin": "3aigent" });
        let changed =
            sync_skin_to_tenant_config(temp.path(), "demo", "messaging-webchat-gui", &answers)
                .unwrap();
        assert!(changed);

        let updated: Value =
            serde_json::from_str(&std::fs::read_to_string(&default_file).unwrap()).unwrap();
        assert_eq!(updated["skin"].as_str(), Some("3aigent"));
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
}
