//! Persist config and secrets from QA apply-answers output.
//!
//! After a provider's `apply-answers` op returns a config object, this module:
//! - Extracts all fields (WASM components read both secret and non-secret config
//!   values via the secrets API) and writes them to the dev secrets store.
//! - Provides filtering to separate secret from non-secret fields.

use std::path::Path;

use anyhow::Result;
use greentic_secrets_lib::{
    ApplyOptions, DevStore, SecretFormat, SeedDoc, SeedEntry, SeedValue, apply_seed,
};
use qa_spec::{FormSpec, VisibilityMode, resolve_visibility};
use serde_json::{Map as JsonMap, Value};

use crate::canonical_secret_uri;

/// Extract all question fields from the QA config output and write them to the dev store.
///
/// All fields are persisted (not just secrets) because WASM components read
/// both secret and non-secret config values via the secrets API.
///
/// Returns a list of keys that were persisted.
pub async fn persist_qa_secrets(
    store: &DevStore,
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    config: &Value,
    form_spec: &FormSpec,
) -> Result<Vec<String>> {
    // Compute visibility to skip invisible/conditional questions.
    let visibility = resolve_visibility(form_spec, config, VisibilityMode::Visible);

    let visible_question_ids: Vec<&str> = form_spec
        .questions
        .iter()
        .filter(|q| visibility.get(&q.id).copied().unwrap_or(true))
        .map(|q| q.id.as_str())
        .collect();
    if visible_question_ids.is_empty() {
        return Ok(vec![]);
    }

    let Some(config_map) = config.as_object() else {
        return Ok(vec![]);
    };

    let mut entries = Vec::new();
    let mut saved_keys = Vec::new();

    for &key in &visible_question_ids {
        if let Some(value) = config_map.get(key) {
            let text = value_to_text(value);
            if text.is_empty() || text == "null" {
                continue;
            }
            let uri = canonical_secret_uri(env, tenant, team, provider_id, key);
            entries.push(SeedEntry {
                uri,
                format: SecretFormat::Text,
                value: SeedValue::Text { text },
                description: Some(format!("from QA setup for {provider_id}")),
            });
            saved_keys.push(key.to_string());
        }
    }

    if entries.is_empty() {
        return Ok(vec![]);
    }

    let report = apply_seed(store, &SeedDoc { entries }, ApplyOptions::default()).await;
    if !report.failed.is_empty() {
        return Err(anyhow::anyhow!(
            "failed to persist {} secret(s): {:?}",
            report.failed.len(),
            report.failed
        ));
    }

    Ok(saved_keys)
}

/// Remove secret fields from a config object.
pub fn filter_secrets(config: &Value, secret_ids: &[&str]) -> Value {
    let Some(map) = config.as_object() else {
        return config.clone();
    };
    let filtered: JsonMap<String, Value> = map
        .iter()
        .filter(|(key, _)| !secret_ids.contains(&key.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Value::Object(filtered)
}

/// Persist all config values as secrets without requiring a FormSpec.
///
/// Used by `demo start --setup-input` where the QA form spec may not
/// be available but WASM components still read config values via the secrets API.
///
/// Also reads the pack's `secret-requirements.json` (if a `pack_path` is
/// provided) and seeds aliases so that WASM components that look up secrets by
/// their canonical requirement key can find the value even when the answers
/// file uses a shorter key.
pub async fn persist_all_config_as_secrets(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    config: &Value,
    pack_path: Option<&Path>,
) -> Result<Vec<String>> {
    let Some(config_map) = config.as_object() else {
        return Ok(vec![]);
    };
    if config_map.is_empty() {
        return Ok(vec![]);
    }

    let store = crate::secrets::open_dev_store(bundle_root)?;

    let mut entries = Vec::new();
    let mut saved_keys = Vec::new();

    for (key, value) in config_map {
        let text = value_to_text(value);
        if text.is_empty() || text == "null" {
            continue;
        }
        let uri = canonical_secret_uri(env, tenant, team, provider_id, key);
        entries.push(SeedEntry {
            uri,
            format: SecretFormat::Text,
            value: SeedValue::Text { text },
            description: Some(format!("from setup-input for {provider_id}")),
        });
        saved_keys.push(key.to_string());
    }

    // Seed aliases from secret-requirements.json so WASM components can find
    // secrets by their canonical requirement key (e.g. WEBEX_BOT_TOKEN →
    // webex_bot_token) even when the answers file uses a shorter key (bot_token).
    if let Some(pp) = pack_path {
        seed_secret_requirement_aliases(
            &mut entries,
            config_map,
            env,
            tenant,
            team,
            provider_id,
            pp,
        );
    }

    if entries.is_empty() {
        return Ok(vec![]);
    }

    let report = apply_seed(&store, &SeedDoc { entries }, ApplyOptions::default()).await;
    if !report.failed.is_empty() {
        return Err(anyhow::anyhow!(
            "failed to persist {} secret(s): {:?}",
            report.failed.len(),
            report.failed
        ));
    }

    Ok(saved_keys)
}

/// Convenience function to persist both secrets and config from QA results.
///
/// Creates a `DevStore` from the bundle root and persists both.
#[allow(clippy::too_many_arguments)]
pub async fn persist_qa_results(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    config: &Value,
    form_spec: &FormSpec,
) -> Result<Vec<String>> {
    let env = crate::resolve_env(None);
    let store = crate::secrets::open_dev_store(bundle_root)?;

    persist_qa_secrets(&store, &env, tenant, team, provider_id, config, form_spec).await
}

/// OAuth authorization stub.
///
/// Prints the authorization URL and returns `None`. Placeholder for future
/// `greentic-oauth` integration.
pub fn oauth_authorize_stub(provider_id: &str, auth_url: Option<&str>) -> Option<String> {
    if let Some(url) = auth_url {
        println!("[oauth] Authorize {provider_id} at: {url}");
        println!("[oauth] After authorizing, re-run setup to complete configuration.");
    } else {
        println!("[oauth] Provider {provider_id} requires OAuth authorization.");
        println!("[oauth] OAuth integration is not yet implemented.");
    }
    None
}

// ── Alias seeding ───────────────────────────────────────────────────────────

/// Read `assets/secret-requirements.json` from a pack and seed alias entries
/// for any requirement key that differs from the answers key after
/// canonicalization.
fn seed_secret_requirement_aliases(
    entries: &mut Vec<SeedEntry>,
    config_map: &JsonMap<String, Value>,
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    pack_path: &Path,
) {
    let reqs = match read_secret_requirements(pack_path) {
        Ok(r) => r,
        Err(_) => return,
    };
    let normalize = crate::secret_name::canonical_secret_name;
    let existing_keys: std::collections::HashSet<String> = entries
        .iter()
        .filter_map(|e| e.uri.rsplit('/').next().map(String::from))
        .collect();

    for req in &reqs {
        let canonical_req_key = normalize(&req.key);
        if existing_keys.contains(&canonical_req_key) {
            continue;
        }
        let matched_value = config_map.iter().find_map(|(cfg_key, cfg_val)| {
            let norm_cfg = normalize(cfg_key);
            if canonical_req_key.ends_with(&norm_cfg) {
                let text = value_to_text(cfg_val);
                if text.is_empty() || text == "null" {
                    None
                } else {
                    Some(text)
                }
            } else {
                None
            }
        });
        if let Some(text) = matched_value {
            let uri = canonical_secret_uri(env, tenant, team, provider_id, &canonical_req_key);
            entries.push(SeedEntry {
                uri,
                format: SecretFormat::Text,
                value: SeedValue::Text { text },
                description: Some(format!("alias from {} for {provider_id}", req.key)),
            });
        }
    }
}

fn read_secret_requirements(
    pack_path: &Path,
) -> Result<Vec<crate::secrets::PackSecretRequirement>> {
    crate::secrets::load_secret_requirements_from_pack(pack_path)
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::open_dev_store;
    use greentic_secrets_lib::SecretsStore;
    use qa_spec::{QuestionSpec, QuestionType};
    use serde_json::json;
    use std::io::Write;
    use std::path::Path;
    use zip::write::SimpleFileOptions;

    fn make_form_spec(questions: Vec<QuestionSpec>) -> FormSpec {
        FormSpec {
            id: "test".into(),
            title: "Test".into(),
            version: "1.0.0".into(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions,
        }
    }

    fn question(id: &str, secret: bool) -> QuestionSpec {
        QuestionSpec {
            id: id.into(),
            kind: QuestionType::String,
            title: id.into(),
            title_i18n: None,
            description: None,
            description_i18n: None,
            required: false,
            choices: None,
            default_value: None,
            secret,
            visible_if: None,
            constraint: None,
            list: None,
            computed: None,
            policy: Default::default(),
            computed_overridable: false,
        }
    }

    #[test]
    fn filters_out_secret_fields() {
        let config = json!({
            "enabled": true,
            "bot_token": "secret123",
            "public_url": "https://example.com"
        });
        let secret_ids = vec!["bot_token"];
        let filtered = filter_secrets(&config, &secret_ids);
        assert!(filtered.get("enabled").is_some());
        assert!(filtered.get("public_url").is_some());
        assert!(filtered.get("bot_token").is_none());
    }

    #[test]
    fn no_secrets_returns_full_config() {
        let config = json!({"enabled": true, "url": "https://example.com"});
        let filtered = filter_secrets(&config, &[]);
        assert_eq!(filtered, config);
    }

    #[test]
    fn identifies_secret_questions() {
        let spec = make_form_spec(vec![
            question("enabled", false),
            question("bot_token", true),
            question("api_secret", true),
            question("url", false),
        ]);
        let secret_ids: Vec<&str> = spec
            .questions
            .iter()
            .filter(|q| q.secret)
            .map(|q| q.id.as_str())
            .collect();
        assert_eq!(secret_ids, vec!["bot_token", "api_secret"]);
    }

    fn write_pack_with_secret_requirements(path: &Path, req_json: &str) {
        let file = std::fs::File::create(path).expect("create pack");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "assets/secret-requirements.json",
            SimpleFileOptions::default(),
        )
        .expect("start entry");
        zip.write_all(req_json.as_bytes()).expect("write reqs");
        zip.finish().expect("finish zip");
    }

    #[tokio::test]
    async fn persist_qa_secrets_persists_visible_non_empty_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = open_dev_store(temp.path()).expect("open dev store");
        let spec = make_form_spec(vec![question("token", true), question("enabled", false)]);
        let config = json!({
            "token": "abc123",
            "enabled": true,
            "ignored": "not-in-form",
            "empty": ""
        });

        let saved = persist_qa_secrets(
            &store,
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-telegram",
            &config,
            &spec,
        )
        .await
        .expect("persist");
        assert_eq!(saved, vec!["token".to_string(), "enabled".to_string()]);

        let token_uri = crate::canonical_secret_uri(
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-telegram",
            "token",
        );
        let enabled_uri = crate::canonical_secret_uri(
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-telegram",
            "enabled",
        );
        let token_value =
            String::from_utf8(store.get(&token_uri).await.expect("token")).expect("token utf8");
        let enabled_value = String::from_utf8(store.get(&enabled_uri).await.expect("enabled"))
            .expect("enabled utf8");
        assert_eq!(token_value, "abc123");
        assert_eq!(enabled_value, "true");
    }

    #[tokio::test]
    async fn persist_all_config_as_secrets_seeds_aliases_from_requirements() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle_root = temp.path();
        let pack = bundle_root.join("messaging-webex.gtpack");
        write_pack_with_secret_requirements(&pack, r#"[{"key":"WEBEX_BOT_TOKEN"}]"#);

        let config = json!({
            "bot_token": "xyz"
        });
        let saved = persist_all_config_as_secrets(
            bundle_root,
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-webex",
            &config,
            Some(&pack),
        )
        .await
        .expect("persist all");
        assert_eq!(saved, vec!["bot_token".to_string()]);

        let store = open_dev_store(bundle_root).expect("open store");
        let base_uri = crate::canonical_secret_uri(
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-webex",
            "bot_token",
        );
        let alias_uri = crate::canonical_secret_uri(
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-webex",
            "WEBEX_BOT_TOKEN",
        );
        let base_value =
            String::from_utf8(store.get(&base_uri).await.expect("base")).expect("base utf8");
        let alias_value =
            String::from_utf8(store.get(&alias_uri).await.expect("alias")).expect("alias utf8");
        assert_eq!(base_value, "xyz");
        assert_eq!(alias_value, "xyz");
    }

    #[test]
    fn oauth_authorize_stub_returns_none() {
        assert!(
            oauth_authorize_stub("messaging-slack", Some("https://auth.example.com")).is_none()
        );
        assert!(oauth_authorize_stub("messaging-slack", None).is_none());
    }
}
