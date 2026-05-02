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

            if let Some(Value::Object(setup_answers)) = map.get("setup_answers") {
                Ok(LoadedAnswers {
                    tenant,
                    team,
                    env,
                    platform_setup,
                    setup_answers: setup_answers.clone(),
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
                })
            } else {
                Ok(LoadedAnswers {
                    tenant,
                    team,
                    env,
                    platform_setup,
                    setup_answers: map,
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
pub fn encrypt_secret_answers(
    bundle: &Path,
    answers_doc: &mut Value,
    key: Option<&str>,
    interactive: bool,
) -> anyhow::Result<()> {
    let setup_answers = answers_doc
        .get_mut("setup_answers")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("internal error: setup_answers not an object"))?;
    let discovered = if bundle.exists() {
        discovery::discover(bundle)?
    } else {
        return Ok(());
    };

    let mut secret_paths = Vec::new();
    for provider in discovered.setup_targets() {
        let Some(form_spec) =
            crate::setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id)
        else {
            continue;
        };
        let Some(provider_answers) = setup_answers
            .get_mut(&provider.provider_id)
            .and_then(Value::as_object_mut)
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
            secret_paths.push((provider.provider_id.clone(), question.id.clone(), value));
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

    for (provider_id, field_id, value) in secret_paths {
        let encrypted = answers_crypto::encrypt_value(&value, &resolved_key)?;
        if let Some(provider_answers) = setup_answers
            .get_mut(&provider_id)
            .and_then(Value::as_object_mut)
        {
            provider_answers.insert(field_id, encrypted);
        }
    }

    Ok(())
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
