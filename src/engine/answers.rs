//! Answers handling for the setup engine.
//!
//! Contains functions for emitting, loading, encrypting, and prompting
//! for setup answers.

use std::path::Path;

use anyhow::{Context, anyhow};
use qa_spec::QuestionType;
use serde_json::{Map as JsonMap, Value};

use crate::plan::SetupPlan;
use crate::platform_setup::load_effective_static_routes_defaults;
use crate::{answers_crypto, discovery, setup_input};

use super::plan_builders::infer_default_value;
use super::types::{LoadedAnswers, SetupConfig};

/// Emit an answers template JSON file.
///
/// Discovers all packs in the bundle and generates a template with all
/// setup questions. Users fill this in and pass it via `--answers`.
pub fn emit_answers(
    config: &SetupConfig,
    plan: &SetupPlan,
    output_path: &Path,
    key: Option<&str>,
    interactive: bool,
) -> anyhow::Result<()> {
    let bundle = &plan.bundle;

    // Build the answers document structure.
    // `platform_setup.tunnel` is emitted as a placeholder so
    // `--non-interactive --answers` runs don't deadlock on a hidden
    // tunnel-mode TTY prompt — see complete_loaded_answers_with_prompts.
    let tunnel_value = match plan.metadata.tunnel.as_ref() {
        Some(t) => serde_json::to_value(t)?,
        None => serde_json::json!({ "mode": null }),
    };
    let mut answers_doc = serde_json::json!({
        "greentic_setup_version": "1.0.0",
        "bundle_source": bundle.display().to_string(),
        "tenant": config.tenant,
        "team": config.team,
        "env": config.env,
        "platform_setup": {
            "static_routes": plan.metadata.static_routes.to_answers(),
            "deployment_targets": plan.metadata.deployment_targets,
            "tunnel": tunnel_value
        },
        "setup_answers": {}
    });

    if !plan.metadata.static_routes.public_web_enabled
        && plan.metadata.static_routes.public_base_url.is_none()
        && let Some(existing) =
            load_effective_static_routes_defaults(bundle, &config.tenant, config.team.as_deref())?
    {
        answers_doc["platform_setup"]["static_routes"] =
            serde_json::to_value(existing.to_answers())?;
    }

    // Discover packs and extract their QA specs
    let setup_answers = answers_doc
        .get_mut("setup_answers")
        .and_then(|v| v.as_object_mut())
        .ok_or_else(|| anyhow!("internal error: setup_answers not an object"))?;

    // Add existing answers from the plan metadata
    for (provider_id, answers) in &plan.metadata.setup_answers {
        setup_answers.insert(provider_id.clone(), answers.clone());
    }

    // Discover packs and populate question templates for all providers.
    // If a provider entry already exists but is empty, merge in the
    // questions from setup.yaml so the user sees what needs to be filled.
    if bundle.exists() {
        let discovered = discovery::discover(bundle)?;
        for provider in discovered.setup_targets() {
            let provider_id = provider.provider_id.clone();
            let existing_is_empty = setup_answers
                .get(&provider_id)
                .and_then(|v| v.as_object())
                .is_some_and(|m| m.is_empty());
            if !setup_answers.contains_key(&provider_id) || existing_is_empty {
                let template = if let Some(form_spec) =
                    crate::setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider_id)
                {
                    template_from_form_spec(&form_spec)
                } else if let Some(spec) = setup_input::load_setup_spec(&provider.pack_path)? {
                    let mut entries = JsonMap::new();
                    for question in &spec.questions {
                        let default_value = infer_default_value(question);
                        entries.insert(question.name.clone(), default_value);
                    }
                    entries
                } else {
                    JsonMap::new()
                };
                setup_answers.insert(provider_id, Value::Object(template));
            }
        }
    }

    // Prompt for secret values if interactive
    if interactive {
        prompt_secret_answers(bundle, &mut answers_doc)?;
    }

    encrypt_secret_answers(bundle, &mut answers_doc, key, interactive)?;

    // Write the answers document to the output path
    let output_content = serde_json::to_string_pretty(&answers_doc)
        .context("failed to serialize answers document")?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }

    std::fs::write(output_path, output_content)
        .with_context(|| format!("failed to write answers to: {}", output_path.display()))?;

    println!("Answers template written to: {}", output_path.display());
    Ok(())
}

/// Load answers from a JSON/YAML file.
pub fn load_answers(
    answers_path: &Path,
    key: Option<&str>,
    interactive: bool,
) -> anyhow::Result<LoadedAnswers> {
    let raw = setup_input::load_setup_input(answers_path)?;
    let raw = if answers_crypto::has_encrypted_values(&raw) {
        let resolved_key = match key {
            Some(value) => value.to_string(),
            None if interactive => answers_crypto::prompt_for_key("decrypting answers")?,
            None => {
                return Err(anyhow!(
                    "answers file contains encrypted secret values; rerun with --key or interactive input"
                ));
            }
        };
        answers_crypto::decrypt_tree(&raw, &resolved_key)?
    } else {
        raw
    };
    match raw {
        Value::Object(map) => {
            fn parse_optional_string(
                map: &JsonMap<String, Value>,
                key: &str,
            ) -> anyhow::Result<Option<String>> {
                match map.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(Value::String(value)) => Ok(Some(value.clone())),
                    Some(_) => Err(anyhow!("answers field '{key}' must be a string or null")),
                }
            }

            let tenant = parse_optional_string(&map, "tenant")?;
            let team = parse_optional_string(&map, "team")?;
            let env = parse_optional_string(&map, "env")?;

            let platform_setup = map
                .get("platform_setup")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .context("parse platform_setup answers")?
                .unwrap_or_default();

            let providers: Vec<super::types::ProviderEntry> = map
                .get("providers")
                .cloned()
                .map(|v| {
                    serde_json::from_value::<Vec<super::types::ProviderEntry>>(v.clone())
                        .with_context(|| {
                            // Surface which entry is malformed by trying each element individually.
                            if let Some(arr) = v.as_array() {
                                for (i, entry) in arr.iter().enumerate() {
                                    if let Err(e) =
                                        serde_json::from_value::<super::types::ProviderEntry>(
                                            entry.clone(),
                                        )
                                    {
                                        let kind_hint = entry
                                            .get("kind")
                                            .and_then(|k| k.as_str())
                                            .unwrap_or("<unknown>");
                                        return format!(
                                            "parse providers[{i}] (kind={kind_hint}): {e}"
                                        );
                                    }
                                }
                            }
                            "parse providers array".to_string()
                        })
                })
                .transpose()?
                .unwrap_or_default();

            if let Some(Value::Object(setup_answers)) = map.get("setup_answers") {
                Ok(LoadedAnswers {
                    tenant,
                    team,
                    env,
                    platform_setup,
                    setup_answers: setup_answers.clone(),
                    providers,
                })
            } else if map.contains_key("bundle_source")
                || map.contains_key("tenant")
                || map.contains_key("team")
                || map.contains_key("env")
                || map.contains_key("platform_setup")
            {
                Ok(LoadedAnswers {
                    tenant,
                    team,
                    env,
                    platform_setup,
                    setup_answers: JsonMap::new(),
                    providers,
                })
            } else {
                Ok(LoadedAnswers {
                    tenant,
                    team,
                    env,
                    platform_setup,
                    setup_answers: map,
                    providers,
                })
            }
        }
        _ => Err(anyhow!("answers file must be a JSON/YAML object")),
    }
}

/// Prompt user to fill in secret values interactively.
///
/// Discovers all secret questions from packs and prompts user to enter
/// values using secure/hidden input. Updates the answers_doc in place.
pub fn prompt_secret_answers(bundle: &Path, answers_doc: &mut Value) -> anyhow::Result<()> {
    use rpassword::prompt_password;
    use std::io::{self, Write as _};

    let setup_answers = answers_doc
        .get_mut("setup_answers")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("internal error: setup_answers not an object"))?;

    let discovered = if bundle.exists() {
        discovery::discover(bundle)?
    } else {
        return Ok(());
    };

    // Collect all secret questions that need prompting
    let mut secret_questions: Vec<(String, String, String, bool)> = Vec::new(); // (provider_id, field_id, title, required)

    for provider in discovered.setup_targets() {
        let Some(form_spec) =
            crate::setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id)
        else {
            continue;
        };

        let provider_answers = setup_answers
            .get(&provider.provider_id)
            .and_then(Value::as_object);

        for question in form_spec.questions {
            if !question.secret {
                continue;
            }

            // Check if already has a non-empty value
            let has_value = provider_answers
                .and_then(|m| m.get(&question.id))
                .is_some_and(|v| !v.is_null() && v.as_str().map(|s| !s.is_empty()).unwrap_or(true));

            if !has_value {
                secret_questions.push((
                    provider.provider_id.clone(),
                    question.id.clone(),
                    question.title.clone(),
                    question.required,
                ));
            }
        }
    }

    if secret_questions.is_empty() {
        return Ok(());
    }

    println!();
    println!("── Secret Values ──");
    println!("Enter values for secret fields (input is hidden):");
    println!("(Press Enter to skip optional fields)\n");

    for (provider_id, field_id, title, required) in secret_questions {
        let display_provider = crate::setup_to_formspec::strip_domain_prefix(&provider_id);
        let marker = if required {
            " (required)"
        } else {
            " (optional)"
        };

        print!("  [{display_provider}] {title}{marker}: ");
        io::stdout().flush()?;

        let input = prompt_password("").unwrap_or_default();
        let trimmed = input.trim();

        if !trimmed.is_empty() {
            // Update the answers_doc with the inputted value
            if let Some(provider_answers) = setup_answers
                .get_mut(&provider_id)
                .and_then(Value::as_object_mut)
            {
                provider_answers.insert(field_id, Value::String(trimmed.to_string()));
            }
        } else if required {
            println!("    \x1b[33m⚠ Skipped (will need to be filled in later)\x1b[0m");
        }
    }

    println!();
    Ok(())
}

/// Encrypt secret values in the answers document.
///
/// Walks both `setup_answers` (per-provider maps keyed by provider_id)
/// and `providers[]` entries (declarative provider wiring). Secret
/// fields are identified via the pack's `FormSpec` (`secret: true`).
pub fn encrypt_secret_answers(
    bundle: &Path,
    answers_doc: &mut Value,
    key: Option<&str>,
    interactive: bool,
) -> anyhow::Result<()> {
    let discovered = if bundle.exists() {
        discovery::discover(bundle)?
    } else {
        return Ok(());
    };

    // Collect (location, value) pairs to encrypt. All reads happen first
    // (immutable borrows), then all writes happen (mutable borrows).
    // location: ("setup_answers", provider_id, field_id) or ("providers", idx as string, field_id)
    let mut secret_paths: Vec<(String, String, String, Value)> = Vec::new();

    // ── Walk setup_answers ───────────────────────────────────────────
    if let Some(setup_answers) = answers_doc.get("setup_answers").and_then(Value::as_object) {
        for provider in discovered.setup_targets() {
            let Some(form_spec) = crate::setup_to_formspec::pack_to_form_spec(
                &provider.pack_path,
                &provider.provider_id,
            ) else {
                continue;
            };
            let Some(provider_answers) = setup_answers
                .get(&provider.provider_id)
                .and_then(Value::as_object)
            else {
                continue;
            };
            for question in form_spec.questions {
                if !question.secret {
                    continue;
                }
                let Some(value) = provider_answers.get(&question.id).cloned() else {
                    continue;
                };
                if value.is_null() || value == Value::String(String::new()) {
                    continue;
                }
                secret_paths.push((
                    "setup_answers".to_string(),
                    provider.provider_id.clone(),
                    question.id.clone(),
                    value,
                ));
            }
        }
    }

    // ── Walk providers[] entries ──────────────────────────────────────
    if let Some(providers_arr) = answers_doc.get("providers").and_then(Value::as_array) {
        for (idx, entry) in providers_arr.iter().enumerate() {
            let Some(kind) = entry.get("kind").and_then(Value::as_str) else {
                continue;
            };
            let Some(entry_answers) = entry.get("answers").and_then(Value::as_object) else {
                continue;
            };

            let pack_path = resolve_provider_pack_path(kind, &discovered);
            let pack_path = match pack_path {
                Some(p) => p,
                None => continue,
            };

            let provider_id = entry
                .get("provider_id")
                .and_then(Value::as_str)
                .or_else(|| {
                    crate::provider_registry::lookup(kind).map(|info| info.default_provider_id)
                })
                .unwrap_or(kind);

            let Some(form_spec) =
                crate::setup_to_formspec::pack_to_form_spec(&pack_path, provider_id)
            else {
                continue;
            };

            for question in form_spec.questions {
                if !question.secret {
                    continue;
                }
                let Some(value) = entry_answers.get(&question.id).cloned() else {
                    continue;
                };
                if value.is_null() || value == Value::String(String::new()) {
                    continue;
                }
                secret_paths.push((
                    "providers".to_string(),
                    idx.to_string(),
                    question.id.clone(),
                    value,
                ));
            }
        }
    }

    if secret_paths.is_empty() {
        return Ok(());
    }

    let resolved_key = match key {
        Some(value) => value.to_string(),
        None if interactive => answers_crypto::prompt_for_key("encrypting answers")?,
        None => {
            return Err(anyhow!(
                "answer document includes secret values; rerun with --key or interactive input"
            ));
        }
    };

    // ── Apply encryptions (mutable borrows) ──────────────────────────
    for (section, id_or_idx, field_id, value) in secret_paths {
        let encrypted = answers_crypto::encrypt_value(&value, &resolved_key)?;
        if section == "setup_answers" {
            if let Some(provider_answers) = answers_doc
                .get_mut("setup_answers")
                .and_then(Value::as_object_mut)
                .and_then(|sa| sa.get_mut(&id_or_idx))
                .and_then(Value::as_object_mut)
            {
                provider_answers.insert(field_id, encrypted);
            }
        } else {
            let idx: usize = id_or_idx.parse().unwrap_or(0);
            if let Some(entry) = answers_doc
                .get_mut("providers")
                .and_then(Value::as_array_mut)
                .and_then(|arr| arr.get_mut(idx))
                && let Some(answers) = entry.get_mut("answers").and_then(Value::as_object_mut)
            {
                answers.insert(field_id, encrypted);
            }
        }
    }

    Ok(())
}

/// Find the on-disk pack path for a provider kind by searching discovered
/// packs. Maps `kind` -> `pack_name` via the provider registry, then matches
/// against `provider_id` of discovered packs.
fn resolve_provider_pack_path(
    kind: &str,
    discovered: &crate::discovery::DiscoveryResult,
) -> Option<std::path::PathBuf> {
    let info = crate::provider_registry::lookup(kind)?;
    // Search discovered packs for one whose provider_id matches the
    // registry's pack_name (e.g. "messaging-telegram") or default_provider_id.
    discovered
        .setup_targets()
        .into_iter()
        .find(|p| p.provider_id == info.pack_name || p.provider_id == info.default_provider_id)
        .map(|p| p.pack_path.clone())
}

fn template_from_form_spec(form_spec: &qa_spec::FormSpec) -> JsonMap<String, Value> {
    let mut entries = JsonMap::new();
    for question in &form_spec.questions {
        let value = question
            .default_value
            .as_ref()
            .map(|default| crate::qa::prompts::parse_typed_value(question.kind, default))
            .unwrap_or_else(|| empty_value_for_question(question.kind));
        entries.insert(question.id.clone(), value);
    }
    entries
}

fn empty_value_for_question(kind: QuestionType) -> Value {
    match kind {
        QuestionType::Boolean => Value::String(String::new()),
        QuestionType::Number => Value::String(String::new()),
        _ => Value::String(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{SetupConfig, SetupEngine, SetupRequest};
    use crate::plan::TenantSelection;
    use crate::platform_setup::StaticRoutesPolicy;
    use std::collections::BTreeSet;
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    fn write_app_pack(path: &Path, pack_id: &str, secret_key: &str) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": pack_id,
                "display_name": pack_id,
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/secret-requirements.json", options)?;
        writer.write_all(
            serde_json::json!([{ "key": secret_key }])
                .to_string()
                .as_bytes(),
        )?;
        writer.finish()?;
        Ok(())
    }

    /// Path to the REAL messaging-telegram setup.yaml in the sibling repo.
    /// Tests that anchor to the real artifact read the field name and
    /// `secret: true` flag from this file rather than inventing a synthetic
    /// FormSpec — a synthetic spec with a made-up field name would pass even
    /// if the real field name never matches.
    const REAL_TELEGRAM_SETUP_YAML: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../greentic-messaging-providers/packs/messaging-telegram/assets/setup.yaml"
    );

    /// Verbatim copy of that file.
    ///
    /// Vendored rather than read from the sibling repo: greentic-setup's CI does
    /// not check `greentic-messaging-providers` out, so reading it there panics
    /// and takes the secret-encryption regression test down with it. The point of
    /// anchoring to the real pack is that a synthetic FormSpec with an invented
    /// field name would pass even if the real name never matched — vendoring keeps
    /// that property while letting the tests run anywhere.
    /// `vendored_telegram_yaml_still_matches_real_pack` guards this copy against drift.
    const TELEGRAM_SETUP_YAML_FIXTURE: &str = r#"
provider_id: telegram
version: 1
title: Telegram provider setup
setup_actions:
  - id: add-to-telegram
    label: Add to Telegram
    kind: deep_link
    url_template: "https://t.me/{bot_username}"
    style: primary
    opens_new_window: true
    copyable: true
    requires:
      - bot_username
questions:
  - name: public_base_url
    title: Public base URL
    kind: string
    required: true
    help: "Public-facing URL for webhook callbacks. Use the runtime or tunnel URL shown by the setup host."
    group: Connection
    validate:
      regex: "^https://"
  - name: bot_username
    title: Bot username
    kind: string
    required: true
    help: "Telegram bot username from @BotFather, without the leading @. This is used for the final Add to Telegram link."
    group: Connection
  - name: default_chat_id
    title: Default chat ID
    kind: string
    required: false
    placeholder: "-1001234567890"
    group: Defaults
  - name: api_base_url
    title: API base URL
    kind: string
    required: true
    default: "https://api.telegram.org"
    help: "Telegram Bot API base URL (default: https://api.telegram.org)"
    placeholder: "https://api.telegram.org"
    group: Connection
    validate:
      regex: "^https?://"
  - name: telegram_bot_token
    title: Telegram bot token
    kind: string
    required: true
    secret: true
    help: "Token from @BotFather"
    group: Authentication
    docs_url: "https://core.telegram.org/bots#how-do-i-create-a-bot"
    create_url: "https://t.me/BotFather"
"#;

    /// Drift guard. Where the sibling repo IS checked out (local dev, the
    /// monorepo), the vendored copy above must still match the real pack.
    /// Skipping is correct here — it checks the fixture, not the encryption
    /// logic, which is covered unconditionally either way.
    #[test]
    fn vendored_telegram_yaml_still_matches_real_pack() {
        let Ok(real) = std::fs::read_to_string(REAL_TELEGRAM_SETUP_YAML) else {
            eprintln!("skipping drift check: greentic-messaging-providers not checked out");
            return;
        };
        assert_eq!(
            real.trim(),
            TELEGRAM_SETUP_YAML_FIXTURE.trim(),
            "the vendored TELEGRAM_SETUP_YAML_FIXTURE has drifted from the real \
             messaging-telegram setup.yaml — update it"
        );
    }

    /// Build a minimal .gtpack ZIP containing the real telegram setup.yaml.
    fn write_telegram_pack_from_real_yaml(path: &Path) -> anyhow::Result<()> {
        let yaml_contents = TELEGRAM_SETUP_YAML_FIXTURE;
        let file = std::fs::File::create(path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-telegram",
                "display_name": "Telegram",
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup.yaml", options)?;
        writer.write_all(yaml_contents.as_bytes())?;
        writer.finish()?;
        Ok(())
    }

    /// Read the secret field name from the REAL telegram setup.yaml so the
    /// test is anchored to the actual pack, not a hand-authored constant.
    fn real_telegram_secret_field_name() -> String {
        let spec: crate::setup_input::SetupSpec =
            serde_yaml_bw::from_str(TELEGRAM_SETUP_YAML_FIXTURE)
                .expect("parse telegram setup.yaml");
        let secret_q = spec
            .questions
            .iter()
            .find(|q| q.secret)
            .expect("real telegram setup.yaml must declare at least one secret question");
        secret_q.name.clone()
    }

    // ── BLOCKER 1 regression: providers[].answers secret encryption ──

    #[test]
    fn providers_secret_field_encrypted_on_disk() -> anyhow::Result<()> {
        let secret_field = real_telegram_secret_field_name();
        let plaintext_token = "1234567890:ABCdefGHIjklMNOpqrSTUvwxYZ";

        let temp = tempfile::tempdir()?;
        let bundle_root = temp.path().join("bundle");
        crate::bundle::create_demo_bundle_structure(&bundle_root, Some("test-bundle"))?;
        let pack_path = bundle_root.join("packs").join("messaging-telegram.gtpack");
        write_telegram_pack_from_real_yaml(&pack_path)?;

        // Build an answers doc with providers[] containing a secret value.
        let mut answers_doc = serde_json::json!({
            "greentic_setup_version": "1.0.0",
            "bundle_source": bundle_root.display().to_string(),
            "tenant": "demo",
            "env": "local",
            "setup_answers": {},
            "providers": [{
                "kind": "telegram",
                "display_name": "Telegram",
                "link_bundle": true,
                "answers": {
                    "public_base_url": "https://example.com",
                    secret_field.clone(): plaintext_token,
                }
            }]
        });

        let encryption_key = "test-encryption-key-42";
        encrypt_secret_answers(&bundle_root, &mut answers_doc, Some(encryption_key), false)?;

        // The on-disk JSON must NOT contain the plaintext token.
        let serialized = serde_json::to_string_pretty(&answers_doc)?;
        assert!(
            !serialized.contains(plaintext_token),
            "plaintext token must not appear in encrypted answers doc"
        );

        // The secret field must be an encrypted envelope, not a string.
        let encrypted_value = &answers_doc["providers"][0]["answers"][&secret_field];
        assert!(
            encrypted_value.is_object(),
            "secret field must be an encrypted envelope object, got: {encrypted_value}"
        );
        assert_eq!(
            encrypted_value
                .get("__greentic_encrypted__")
                .and_then(|v| v.as_str()),
            Some("aes-256-gcm-siv-v1"),
            "encrypted envelope must carry the encryption marker"
        );

        // Non-secret field must remain in plaintext.
        assert_eq!(
            answers_doc["providers"][0]["answers"]["public_base_url"],
            serde_json::json!("https://example.com"),
        );

        // Round-trip: decrypt and verify the value comes back.
        let decrypted = answers_crypto::decrypt_tree(&answers_doc, encryption_key)?;
        let recovered = decrypted["providers"][0]["answers"][&secret_field]
            .as_str()
            .expect("decrypted value must be a string");
        assert_eq!(
            recovered, plaintext_token,
            "decrypt must recover the original token"
        );

        Ok(())
    }

    // ── Backward compatibility: pre-existing answers without providers ──

    #[test]
    fn answers_without_providers_key_still_deserializes() -> anyhow::Result<()> {
        // This mirrors the real answers.json shape from scripts/demo.sh —
        // no `providers` key, which must default to an empty vec.
        let temp = tempfile::tempdir()?;
        let answers_path = temp.path().join("answers.json");
        let doc = serde_json::json!({
            "bundle_source": "./support-bot",
            "env": "production",
            "tenant": "acme-corp",
            "team": "support",
            "setup_answers": {
                "messaging-webchat": {
                    "public_base_url": "https://support.acme-corp.com",
                    "jwt_signing_key": "super-secret-jwt-key-2024"
                }
            }
        });
        std::fs::write(&answers_path, serde_json::to_string_pretty(&doc)?)?;

        let loaded = load_answers(&answers_path, None, false)?;
        assert_eq!(loaded.tenant.as_deref(), Some("acme-corp"));
        assert_eq!(loaded.team.as_deref(), Some("support"));
        assert_eq!(loaded.env.as_deref(), Some("production"));
        assert!(
            loaded.providers.is_empty(),
            "missing providers key must default to empty vec"
        );
        assert!(
            loaded.setup_answers.contains_key("messaging-webchat"),
            "setup_answers must be preserved"
        );
        Ok(())
    }

    // ── providers[] parsing ──

    #[test]
    fn load_answers_parses_providers_array() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let answers_path = temp.path().join("answers.json");
        let doc = serde_json::json!({
            "bundle_source": "./my-bundle",
            "tenant": "demo",
            "env": "local",
            "setup_answers": {},
            "providers": [{
                "kind": "telegram",
                "display_name": "Telegram",
                "link_bundle": true,
                "answers": {
                    "public_base_url": "https://example.com",
                    "telegram_bot_token": "fake-token"
                }
            }]
        });
        std::fs::write(&answers_path, serde_json::to_string_pretty(&doc)?)?;

        let loaded = load_answers(&answers_path, None, false)?;
        assert_eq!(loaded.providers.len(), 1);
        assert_eq!(loaded.providers[0].kind, "telegram");
        assert_eq!(
            loaded.providers[0].display_name.as_deref(),
            Some("Telegram")
        );
        assert!(loaded.providers[0].link_bundle);
        assert_eq!(
            loaded.providers[0]
                .answers
                .get("public_base_url")
                .and_then(|v| v.as_str()),
            Some("https://example.com"),
        );
        Ok(())
    }

    // ── Secret field detection anchored to real setup.yaml ──

    #[test]
    fn secret_detection_matches_real_telegram_field_name() {
        let secret_field = real_telegram_secret_field_name();
        // The real telegram setup.yaml declares `telegram_bot_token` as secret.
        // If the real field name ever changes, this test breaks — which is the
        // point: it forces updating the encryption walk and the provider wiring.
        assert_eq!(
            secret_field, "telegram_bot_token",
            "real telegram setup.yaml secret field must be telegram_bot_token"
        );
    }

    // ── Deterministic idempotency key ──

    #[test]
    fn deterministic_idempotency_key_is_stable() {
        let key1 = crate::provider_commands::deterministic_idempotency_key(
            "local", "telegram", "telegram",
        );
        let key2 = crate::provider_commands::deterministic_idempotency_key(
            "local", "telegram", "telegram",
        );
        assert_eq!(key1, key2, "same inputs must produce the same key");
        assert!(
            key1.starts_with("setup-provider-"),
            "key must carry the prefix, got: {key1}"
        );
    }

    #[test]
    fn deterministic_idempotency_key_differs_across_envs() {
        let key_local = crate::provider_commands::deterministic_idempotency_key(
            "local", "telegram", "telegram",
        );
        let key_prod =
            crate::provider_commands::deterministic_idempotency_key("prod", "telegram", "telegram");
        assert_ne!(
            key_local, key_prod,
            "different envs must produce different keys"
        );
    }

    // ── Finding 1: malformed providers[] must error, not silently drop ──

    #[test]
    fn malformed_provider_entry_returns_error() {
        let temp = tempfile::tempdir().unwrap();
        let answers_path = temp.path().join("answers.json");
        // "kid" is a typo for "kind" — the required field is missing.
        let doc = serde_json::json!({
            "bundle_source": "./my-bundle",
            "tenant": "demo",
            "env": "local",
            "setup_answers": {},
            "providers": [{
                "kid": "telegram",
                "answers": {
                    "telegram_bot_token": "fake-token"
                }
            }]
        });
        std::fs::write(&answers_path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let result = load_answers(&answers_path, None, false);
        assert!(
            result.is_err(),
            "malformed providers[] entry must produce an error, not an empty vec"
        );
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("providers[0]"),
            "error must identify the offending entry index, got: {err_msg}"
        );
    }

    #[test]
    fn misspelled_optional_provider_field_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let answers_path = temp.path().join("answers.json");
        // "anwers" is a typo for "answers". Without `deny_unknown_fields` this
        // parses cleanly, `answers` defaults to `{}`, and the provider is wired
        // with no bot token — registering fine and then failing to authenticate
        // at runtime with nothing in the logs to explain why.
        let doc = serde_json::json!({
            "bundle_source": "./my-bundle",
            "tenant": "demo",
            "env": "local",
            "setup_answers": {},
            "providers": [{
                "kind": "telegram",
                "anwers": {
                    "telegram_bot_token": "fake-token"
                }
            }]
        });
        std::fs::write(&answers_path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let result = load_answers(&answers_path, None, false);
        assert!(
            result.is_err(),
            "an unknown field in a providers[] entry must be rejected, not silently dropped"
        );
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(
            err_msg.contains("anwers"),
            "error must name the unknown field, got: {err_msg}"
        );
    }

    // ── Finding 2: link_bundle defaults to true, explicit false honoured ──

    #[test]
    fn link_bundle_defaults_to_true_when_omitted() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let answers_path = temp.path().join("answers.json");
        // No "link_bundle" key — must default to true.
        let doc = serde_json::json!({
            "bundle_source": "./my-bundle",
            "tenant": "demo",
            "env": "local",
            "setup_answers": {},
            "providers": [{
                "kind": "telegram",
                "answers": {}
            }]
        });
        std::fs::write(&answers_path, serde_json::to_string_pretty(&doc)?)?;

        let loaded = load_answers(&answers_path, None, false)?;
        assert_eq!(loaded.providers.len(), 1);
        assert!(
            loaded.providers[0].link_bundle,
            "link_bundle must default to true (matching interactive `provider add`)"
        );
        Ok(())
    }

    #[test]
    fn link_bundle_explicit_false_honoured() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let answers_path = temp.path().join("answers.json");
        let doc = serde_json::json!({
            "bundle_source": "./my-bundle",
            "tenant": "demo",
            "env": "local",
            "setup_answers": {},
            "providers": [{
                "kind": "telegram",
                "link_bundle": false,
                "answers": {}
            }]
        });
        std::fs::write(&answers_path, serde_json::to_string_pretty(&doc)?)?;

        let loaded = load_answers(&answers_path, None, false)?;
        assert_eq!(loaded.providers.len(), 1);
        assert!(
            !loaded.providers[0].link_bundle,
            "explicit link_bundle: false must be honoured"
        );
        Ok(())
    }

    #[test]
    fn emit_answers_includes_app_pack_secret_questions() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let bundle_root = temp.path().join("bundle");
        crate::bundle::create_demo_bundle_structure(&bundle_root, Some("weather-demo"))?;

        let pack_path = bundle_root.join("packs").join("weather-app.gtpack");
        write_app_pack(&pack_path, "weather-app", "WEATHER_API_KEY")?;

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".to_string(),
            team: None,
            env: "dev".to_string(),
            offline: false,
            verbose: false,
        });
        let request = SetupRequest {
            bundle: bundle_root.clone(),
            tenants: vec![TenantSelection {
                tenant: "demo".to_string(),
                team: None,
                allow_paths: Vec::new(),
            }],
            update_ops: BTreeSet::new(),
            static_routes: StaticRoutesPolicy::default(),
            ..Default::default()
        };
        let plan = engine.plan(crate::SetupMode::Create, &request, true)?;

        let answers_path = temp.path().join("answers.json");
        emit_answers(engine.config(), &plan, &answers_path, None, false)?;

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&answers_path)?)?;
        assert_eq!(
            doc.pointer("/setup_answers/weather-app/weather_api_key"),
            Some(&Value::String(String::new()))
        );
        Ok(())
    }
}
