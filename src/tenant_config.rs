//! Tenant config synchronization for webchat-gui OAuth settings.
//!
//! After setup persists OAuth answers to secrets, this module updates the
//! static tenant config JSON (`assets/webchat-gui/config/tenants/<tenant>.json`)
//! to enable/disable OAuth providers and set client IDs. This ensures the
//! webchat-gui runtime serves the correct auth config without manual editing.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

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
            return update_tenant_config(&default_path, tenant, oauth_enabled, answers_obj);
        }
        return Ok(false);
    }

    update_tenant_config(&config_path, tenant, oauth_enabled, answers_obj)
}

fn update_tenant_config(
    config_path: &Path,
    tenant: &str,
    oauth_enabled: bool,
    answers: &Map<String, Value>,
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

    let public_base_url = answers
        .get("public_base_url")
        .and_then(Value::as_str)
        .unwrap_or("http://localhost:8080");

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
        let redirect_uri = format!(
            "{}/v1/web/webchat/{}/",
            public_base_url.trim_end_matches('/'),
            tenant,
        );

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
                obj.insert("redirectUri".to_string(), Value::String(redirect_uri));
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
            entry.insert("redirectUri".to_string(), Value::String(redirect_uri));
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
        let redirect_uri = format!(
            "{}/v1/web/webchat/{}/",
            public_base_url.trim_end_matches('/'),
            tenant,
        );

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
                obj.insert("redirectUri".to_string(), Value::String(redirect_uri));
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
            entry.insert("redirectUri".to_string(), Value::String(redirect_uri));
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
