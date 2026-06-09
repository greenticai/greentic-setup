//! Persist config and secrets from QA apply-answers output.
//!
//! After a provider's `apply-answers` op returns a config object, this module:
//! - Writes every visible answer to the dev secrets store under
//!   `secrets://<env>/<tenant>/<team>/<provider>/<key>` (legacy universal
//!   write — WASM components have historically read both secret and
//!   non-secret config values through the secrets API).
//! - Emits a new sibling `pack-config-input.v1` file via
//!   [`emit_pack_config_input`] when the wizard scope is known. This is the
//!   C7 producer for the `pack-config.v1.non_secret` channel: the greentic-
//!   deployer picks up the file at revision-create, stamps the active
//!   `revision_id`, and writes the final `pack-config.v1` that
//!   [`RuntimeConfigHost`](https://docs.rs/greentic-interfaces-wasmtime)
//!   reads (C4). The universal DevStore write stays alive for one release as
//!   the C4.2 compatibility shim — runtime reads of non-secret keys fall back
//!   to it with a once-per-process deprecation warning.
//! - Provides filtering to separate secret from non-secret fields.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use greentic_secrets_lib::{
    ApplyOptions, DevStore, SecretFormat, SecretsStore, SeedDoc, SeedEntry, SeedValue, apply_seed,
};
use qa_spec::{FormSpec, VisibilityMode, resolve_visibility};
use serde::{Deserialize, Serialize};
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

    let store_path = crate::secrets::ensure_path(bundle_root)?;
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

    tracing::info!(
        provider_id,
        env,
        tenant,
        team = team.unwrap_or("default"),
        store_path = %store_path.display(),
        entry_count = entries.len(),
        uris = ?entries.iter().map(|e| e.uri.as_str()).collect::<Vec<_>>(),
        "setup secrets persist: applying seed entries"
    );

    let verify_uris: Vec<String> = entries.iter().map(|e| e.uri.clone()).collect();
    let report = apply_seed(&store, &SeedDoc { entries }, ApplyOptions::default()).await;
    if !report.failed.is_empty() {
        tracing::warn!(
            provider_id,
            env,
            tenant,
            team = team.unwrap_or("default"),
            store_path = %store_path.display(),
            failed = ?report.failed,
            "setup secrets persist: apply_seed reported failures"
        );
        return Err(anyhow::anyhow!(
            "failed to persist {} secret(s): {:?}",
            report.failed.len(),
            report.failed
        ));
    }

    // Read-after-write verification so handoff issues are visible in setup logs.
    let mut verify_missing = Vec::new();
    for uri in &verify_uris {
        if store.get(uri).await.is_err() {
            verify_missing.push(uri.clone());
        }
    }
    if verify_missing.is_empty() {
        tracing::info!(
            provider_id,
            env,
            tenant,
            team = team.unwrap_or("default"),
            store_path = %store_path.display(),
            verified = report.ok,
            "setup secrets persist: post-write verification succeeded"
        );
    } else {
        tracing::warn!(
            provider_id,
            env,
            tenant,
            team = team.unwrap_or("default"),
            store_path = %store_path.display(),
            missing_uris = ?verify_missing,
            "setup secrets persist: post-write verification found missing entries"
        );
    }

    Ok(saved_keys)
}

/// Convenience function to persist both secrets and config from QA results.
///
/// Creates a `DevStore` from the bundle root and persists every answer there
/// (legacy universal-write — see module docs). Additionally emits a
/// `pack-config-input.v1` file (C7) so the deployer can populate the
/// `pack-config.v1.non_secret` channel at revision-create.
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

    let keys =
        persist_qa_secrets(&store, &env, tenant, team, provider_id, config, form_spec).await?;

    let bundle_id = infer_bundle_id(bundle_root);
    if let Err(err) = emit_pack_config_input(
        bundle_root,
        &env,
        &bundle_id,
        provider_id,
        config,
        form_spec,
    ) {
        // Soft-fail: the C4.2 compatibility shim still serves these keys
        // from DevStore (already populated above), so a wizard run does not
        // regress on emit failure. The deployer (C7 PR4) will report
        // missing-input at revision-create.
        tracing::warn!(
            provider_id,
            env = %env,
            bundle_id = %bundle_id,
            bundle_root = %bundle_root.display(),
            error = %err,
            "pack-config-input emission failed; runtime falls back to legacy DevStore reads via C4.2 compat shim",
        );
    }

    Ok(keys)
}

/// Derive a stable `bundle_id` from the bundle root path. Mirrors
/// `crate::bundle::infer_bundle_id` (which is private). Used by callers that
/// don't have an explicit `bundle_id` field in their context.
pub(crate) fn infer_bundle_id(root: &Path) -> String {
    root.file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "bundle".to_string())
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

// ── pack-config-input.v1 emitter (C7) ──────────────────────────────────────

/// Schema tag for the wizard-emitted intermediate file consumed by the
/// greentic-deployer at revision-create.
pub const PACK_CONFIG_INPUT_SCHEMA: &str = "greentic.pack-config-input.v1";

/// Directory under `bundle_root` where wizard-emitted pack-config inputs land.
/// The deployer joins on `<bundle_root>/<PACK_CONFIG_INPUT_DIR>/<pack_id>.json`
/// at revision-create.
pub const PACK_CONFIG_INPUT_DIR: &str = "state/pack-configs";

/// Wizard-emitted intermediate file the deployer picks up at revision-create
/// (C7). The deployer stamps the active `revision_id` and writes the final
/// `pack-config.v1` referenced by `pack_config_refs` in `runtime-config.v1`.
/// We keep `revision_id` OUT of this shape on purpose: revisions are minted
/// by the deployer, not the wizard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackConfigInput {
    pub schema: String,
    pub pack_id: String,
    pub env_id: String,
    pub bundle_id: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub non_secret: BTreeMap<String, Value>,
    /// `secret://<env>/<bundle>/<pack>/<question>` URIs (kept as plain
    /// strings here — `greentic-deploy-spec::SecretRef` validates at the
    /// deployer side when it materializes the final `pack-config.v1`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secret_refs: BTreeMap<String, String>,
}

/// Emit a `pack-config-input.v1` file at
/// `<bundle_root>/state/pack-configs/<pack_id>.json` carrying the FormSpec-
/// split of one provider's QA answers (C7). Idempotent: overwrites in place
/// so re-running the wizard produces the same on-disk shape.
///
/// Secret-marked answers are recorded as `secret://<env>/<bundle>/<pack>/<key>`
/// URI references (no plaintext); non-secret answers stay inline. Empty
/// `config` is a no-op (no file written).
///
/// Calling this directly from greentic-setup callers gives them a stable
/// public API surface; the existing `persist_qa_secrets` /
/// `persist_all_config_as_secrets` keep their universal DevStore writes as
/// the C4.2 compatibility-shim path until the deployer is fully wired and a
/// follow-up drops the redundant writes.
pub fn emit_pack_config_input(
    bundle_root: &Path,
    env_id: &str,
    bundle_id: &str,
    pack_id: &str,
    config: &Value,
    form_spec: &FormSpec,
) -> Result<Option<std::path::PathBuf>> {
    validate_segment("env_id", env_id)?;
    validate_segment("bundle_id", bundle_id)?;
    validate_segment("pack_id", pack_id)?;

    let Some(config_map) = config.as_object() else {
        return Ok(None);
    };
    if config_map.is_empty() {
        return Ok(None);
    }

    // Apply the same visibility filter that `persist_qa_secrets` uses so
    // that conditionally-invisible answers do not leak into the
    // pack-config-input file (and from there into runtime config).
    let visibility = resolve_visibility(form_spec, config, VisibilityMode::Visible);

    let secret_ids: std::collections::HashSet<&str> = form_spec
        .questions
        .iter()
        .filter(|q| q.secret)
        .map(|q| q.id.as_str())
        .collect();

    let visible_ids: std::collections::HashSet<&str> = form_spec
        .questions
        .iter()
        .filter(|q| visibility.get(&q.id).copied().unwrap_or(true))
        .map(|q| q.id.as_str())
        .collect();

    let mut non_secret = BTreeMap::new();
    let mut secret_refs = BTreeMap::new();
    for (key, value) in config_map {
        // Skip keys that are not visible according to the form spec's
        // visibility rules (matches `persist_qa_secrets` behavior).
        if !visible_ids.contains(key.as_str()) {
            continue;
        }
        let text = value_to_text(value);
        if text.is_empty() || text == "null" {
            continue;
        }
        if secret_ids.contains(key.as_str()) {
            validate_segment("question.id", key)?;
            let uri = format!("secret://{env_id}/{bundle_id}/{pack_id}/{key}");
            secret_refs.insert(key.clone(), uri);
        } else {
            non_secret.insert(key.clone(), value.clone());
        }
    }

    if non_secret.is_empty() && secret_refs.is_empty() {
        return Ok(None);
    }

    let input = PackConfigInput {
        schema: PACK_CONFIG_INPUT_SCHEMA.to_string(),
        pack_id: pack_id.to_string(),
        env_id: env_id.to_string(),
        bundle_id: bundle_id.to_string(),
        non_secret,
        secret_refs,
    };

    let dir = bundle_root.join(PACK_CONFIG_INPUT_DIR);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create pack-config-input dir {}", dir.display()))?;
    let path = dir.join(format!("{pack_id}.json"));
    let body = serde_json::to_string_pretty(&input).context("serialize pack-config-input.v1")?;
    std::fs::write(&path, format!("{body}\n"))
        .with_context(|| format!("write pack-config-input {}", path.display()))?;

    tracing::debug!(
        pack_id,
        env_id,
        bundle_id,
        non_secret_count = input.non_secret.len(),
        secret_ref_count = input.secret_refs.len(),
        path = %path.display(),
        "wizard emitted pack-config-input.v1 (C7) for deployer pickup",
    );
    Ok(Some(path))
}

/// Reject empty or `/`-bearing identifiers — these would silently corrupt the
/// `secret://<env>/<bundle>/<pack>/<question>` path structure or the
/// `<dir>/<pack_id>.json` file path.
fn validate_segment(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("{label} must not be empty for pack-config-input emission");
    }
    if value.contains('/') {
        anyhow::bail!(
            "{label} `{value}` contains '/' which would corrupt the pack-config-input layout"
        );
    }
    if value == "." || value == ".." {
        anyhow::bail!(
            "{label} `{value}` is a relative path component and would corrupt the pack-config-input layout"
        );
    }
    Ok(())
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

    // ── C7: pack-config-input.v1 emitter ──────────────────────────────────

    /// Secrets land as `secret://` URI refs (no plaintext); non-secrets stay
    /// inline. Empty config → no file written.
    #[test]
    fn emit_pack_config_input_splits_secret_vs_non_secret() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path();
        let form = make_form_spec(vec![
            question("enabled", false),
            question("bot_token", true),
            question("public_url", false),
        ]);
        let config = json!({
            "enabled": true,
            "bot_token": "shhh",
            "public_url": "https://example.com",
        });
        let path =
            emit_pack_config_input(root, "local", "test-bundle", "provider-a", &config, &form)
                .expect("emit")
                .expect("path");
        assert!(path.exists());
        let bytes = std::fs::read(&path).expect("read");
        let parsed: PackConfigInput = serde_json::from_slice(&bytes).expect("parse");
        assert_eq!(parsed.schema, PACK_CONFIG_INPUT_SCHEMA);
        assert_eq!(parsed.pack_id, "provider-a");
        assert_eq!(parsed.env_id, "local");
        assert_eq!(parsed.bundle_id, "test-bundle");
        assert_eq!(
            parsed.non_secret.get("enabled"),
            Some(&Value::Bool(true)),
            "non-secret inline"
        );
        assert_eq!(
            parsed.non_secret.get("public_url"),
            Some(&Value::String("https://example.com".into())),
        );
        assert!(
            !parsed.non_secret.contains_key("bot_token"),
            "secret must not be in non_secret"
        );
        assert_eq!(
            parsed.secret_refs.get("bot_token").map(String::as_str),
            Some("secret://local/test-bundle/provider-a/bot_token"),
            "secret recorded as URI ref"
        );
        // No plaintext for the secret anywhere in the file.
        let body = String::from_utf8(bytes).expect("utf8");
        assert!(
            !body.contains("shhh"),
            "plaintext secret leaked into pack-config-input: {body}"
        );
    }

    /// Same answers + same bundle_id + same provider_id, different env_id →
    /// different secret_refs. Pins the env-segment integrity of the URI.
    #[test]
    fn emit_pack_config_input_secret_refs_discriminate_on_env_id() {
        let tmp_a = tempfile::TempDir::new().expect("tempdir-a");
        let tmp_b = tempfile::TempDir::new().expect("tempdir-b");
        let form = make_form_spec(vec![question("api_token", true)]);
        let cfg = json!({"api_token": "x"});
        let pa = emit_pack_config_input(tmp_a.path(), "local", "b", "p", &cfg, &form)
            .expect("emit-a")
            .expect("path-a");
        let pb = emit_pack_config_input(tmp_b.path(), "staging", "b", "p", &cfg, &form)
            .expect("emit-b")
            .expect("path-b");
        let parsed_a: PackConfigInput =
            serde_json::from_slice(&std::fs::read(&pa).unwrap()).unwrap();
        let parsed_b: PackConfigInput =
            serde_json::from_slice(&std::fs::read(&pb).unwrap()).unwrap();
        assert_eq!(
            parsed_a.secret_refs.get("api_token").map(String::as_str),
            Some("secret://local/b/p/api_token")
        );
        assert_eq!(
            parsed_b.secret_refs.get("api_token").map(String::as_str),
            Some("secret://staging/b/p/api_token")
        );
    }

    /// Empty config → no file written (caller treats `Ok(None)` as no-op,
    /// not as a soft error). Matches the existing `persist_qa_secrets`
    /// short-circuit semantics.
    #[test]
    fn emit_pack_config_input_skips_empty_config() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path();
        let form = make_form_spec(vec![question("enabled", false)]);
        let empty = json!({});
        assert!(
            emit_pack_config_input(root, "local", "b", "p", &empty, &form)
                .expect("emit")
                .is_none()
        );
        assert!(!root.join(PACK_CONFIG_INPUT_DIR).exists());
    }

    /// Reject `/`-bearing or empty path segments — `secret://` URI integrity
    /// + on-disk `<dir>/<pack_id>.json` layout depend on it.
    #[test]
    fn emit_pack_config_input_rejects_invalid_segments() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path();
        let form = make_form_spec(vec![question("k", false)]);
        let cfg = json!({"k": "v"});
        assert!(
            emit_pack_config_input(root, "", "b", "p", &cfg, &form).is_err(),
            "empty env_id rejected"
        );
        assert!(
            emit_pack_config_input(root, "local", "b", "../p", &cfg, &form).is_err(),
            "pack_id with `/` rejected"
        );
        assert!(
            emit_pack_config_input(root, "local", "b/c", "p", &cfg, &form).is_err(),
            "bundle_id with `/` rejected"
        );
        assert!(
            emit_pack_config_input(root, "local", "b", "..", &cfg, &form).is_err(),
            "pack_id `..` rejected"
        );
        assert!(
            emit_pack_config_input(root, ".", "b", "p", &cfg, &form).is_err(),
            "env_id `.` rejected"
        );
    }

    /// Invisible questions (conditional `visible_if` that evaluates to false)
    /// must not leak into the pack-config-input file.
    #[test]
    fn emit_pack_config_input_respects_visibility() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path();
        let form = make_form_spec(vec![question("mode", false), {
            let mut q = question("advanced_url", false);
            q.visible_if = Some(qa_spec::Expr::Eq {
                left: Box::new(qa_spec::Expr::Answer {
                    path: "mode".into(),
                }),
                right: Box::new(qa_spec::Expr::Literal {
                    value: Value::String("advanced".into()),
                }),
            });
            q
        }]);
        // mode=basic → advanced_url should be invisible
        let config = json!({
            "mode": "basic",
            "advanced_url": "https://should-be-hidden.example.com",
        });
        let path = emit_pack_config_input(root, "local", "b", "p", &config, &form)
            .expect("emit")
            .expect("path");
        let parsed: PackConfigInput =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            !parsed.non_secret.contains_key("advanced_url"),
            "invisible question should not appear in non_secret: {parsed:?}"
        );
        assert_eq!(
            parsed.non_secret.get("mode"),
            Some(&Value::String("basic".into())),
        );
    }
}
