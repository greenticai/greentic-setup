//! CLI helper functions for greentic-setup.

mod bundle;
mod env_vars;
mod prompts;

use std::path::Path;

use anyhow::Result;

use crate::discovery;
use crate::engine::LoadedAnswers;
use crate::platform_setup::{
    PlatformSetupAnswers, StaticRoutesPolicy, load_effective_static_routes_defaults,
    prompt_static_routes_policy, prompt_static_routes_policy_with_answers,
};
use crate::qa::wizard;
use crate::setup_to_formspec;

// Re-export from submodules
pub use bundle::{
    SetupOutputTarget, copy_dir_recursive, detect_domain_from_filename, resolve_bundle_dir,
    resolve_bundle_source, resolve_pack_source, setup_output_target,
};
pub use env_vars::{
    EnvVarPlaceholder, apply_resolved_env_vars, collect_env_var_placeholders,
    confirm_env_var_placeholders,
};
pub use prompts::{SetupParams, prompt_setup_params};

/// Resolve tenant/team/env for setup.
///
/// When CLI values are still defaults (`demo`, unset team, `dev`) and an answers
/// file includes tenant/team/env metadata, prefer metadata values.
/// Also detects tenant from existing bundle `tenants/` directory when neither
/// CLI nor answers provide a tenant.
pub fn resolve_setup_scope(
    tenant: String,
    team: Option<String>,
    env: String,
    loaded: &LoadedAnswers,
) -> (String, Option<String>, String) {
    let tenant = if tenant == "demo" {
        loaded.tenant.clone().unwrap_or(tenant)
    } else {
        tenant
    };
    let team = if team.is_none() {
        loaded.team.clone()
    } else {
        team
    };
    let env = if env == "dev" {
        loaded.env.clone().unwrap_or(env)
    } else {
        env
    };
    (tenant, team, env)
}

/// Like [`resolve_setup_scope`] but also checks the bundle's `tenants/` directory
/// for existing tenants when the CLI value is still the default.
pub fn resolve_setup_scope_with_bundle(
    tenant: String,
    team: Option<String>,
    env: String,
    loaded: &LoadedAnswers,
    bundle_dir: &std::path::Path,
) -> (String, Option<String>, String) {
    let (mut tenant, team, env) = resolve_setup_scope(tenant, team, env, loaded);

    // If tenant is still the CLI default ("demo") and the bundle has a tenants/
    // directory, detect the actual tenant from existing directories.
    if tenant == "demo"
        && let Some(detected) = detect_tenant_from_bundle(bundle_dir)
    {
        tenant = detected;
    }

    (tenant, team, env)
}

/// Detect tenant from the bundle's `tenants/` directory.
/// Returns the single tenant if exactly one exists, or the first non-"demo"
/// tenant if multiple exist.
fn detect_tenant_from_bundle(bundle_dir: &std::path::Path) -> Option<String> {
    let tenants_dir = bundle_dir.join("tenants");
    let entries: Vec<String> = std::fs::read_dir(&tenants_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();

    match entries.len() {
        0 => None,
        1 => Some(entries[0].clone()),
        _ => {
            // Multiple tenants — prefer non-"demo" if exists
            entries
                .iter()
                .find(|t| t.as_str() != "demo")
                .cloned()
                .or_else(|| entries.first().cloned())
        }
    }
}

/// Run interactive wizard for all discovered packs in the bundle.
pub fn run_interactive_wizard(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    advanced: bool,
) -> Result<LoadedAnswers> {
    use serde_json::Value;

    let mut all_answers = serde_json::Map::new();
    let existing_static_routes = load_effective_static_routes_defaults(bundle_path, tenant, team)?;
    let static_routes = prompt_static_routes_policy(env, existing_static_routes.as_ref())?;
    let deployment_targets = crate::deployment_targets::prompt_deployment_targets(
        &crate::deployment_targets::discover_deployer_pack_candidates(bundle_path)?,
    )?;

    let discovered = discovery::discover(bundle_path)?;

    if discovered.providers.is_empty() {
        println!("No providers found in bundle. Nothing to configure.");
        return Ok(LoadedAnswers {
            tenant: None,
            team: None,
            env: None,
            platform_setup: PlatformSetupAnswers {
                static_routes: Some(static_routes.to_answers()),
                deployment_targets,
                tunnel: None,
            },
            setup_answers: all_answers,
        });
    }

    println!(
        "Found {} provider(s) to configure:",
        discovered.providers.len()
    );
    for provider in &discovered.providers {
        println!("  - {} ({})", provider.provider_id, provider.domain);
    }
    println!();

    // ── Collect and prompt shared questions once ────────────────────────────
    // Build FormSpecs for all providers to identify shared questions
    let provider_form_specs: Vec<wizard::ProviderFormSpec> = discovered
        .providers
        .iter()
        .filter_map(|provider| {
            setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id).map(
                |form_spec| wizard::ProviderFormSpec {
                    provider_id: provider.provider_id.clone(),
                    form_spec,
                },
            )
        })
        .collect();

    // Prompt for shared questions (like public_base_url) once at the start
    // In interactive mode, we have no existing answers so pass empty Value
    let shared_answers = if provider_form_specs.len() > 1 {
        let shared_result = wizard::collect_shared_questions(&provider_form_specs);
        if !shared_result.shared_questions.is_empty() {
            let empty = Value::Object(serde_json::Map::new());
            wizard::prompt_shared_questions(&shared_result, advanced, &empty)?
        } else {
            Value::Object(serde_json::Map::new())
        }
    } else {
        Value::Object(serde_json::Map::new())
    };

    // ── Configure each provider ─────────────────────────────────────────────
    for provider in &discovered.providers {
        let provider_id = &provider.provider_id;
        let form_spec = setup_to_formspec::pack_to_form_spec(&provider.pack_path, provider_id);

        if let Some(spec) = form_spec {
            if spec.questions.is_empty() {
                println!("Provider {}: No configuration required.", provider_id);
                all_answers.insert(provider_id.clone(), Value::Object(serde_json::Map::new()));
                continue;
            }

            // Use shared answers as initial values - already-answered questions will be skipped
            let answers = wizard::prompt_form_spec_answers_with_existing(
                &spec,
                provider_id,
                advanced,
                &shared_answers,
            )?;
            all_answers.insert(provider_id.clone(), answers);
        } else {
            println!(
                "Provider {}: No setup questions found (may use flow-based setup).",
                provider_id
            );
            all_answers.insert(provider_id.clone(), Value::Object(serde_json::Map::new()));
        }

        println!();
    }

    Ok(LoadedAnswers {
        tenant: None,
        team: None,
        env: None,
        platform_setup: PlatformSetupAnswers {
            static_routes: Some(static_routes.to_answers()),
            deployment_targets,
            tunnel: None,
        },
        setup_answers: all_answers,
    })
}

/// Complete loaded answers by prompting for missing values.
pub fn complete_loaded_answers_with_prompts(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    advanced: bool,
    mut loaded: LoadedAnswers,
) -> Result<LoadedAnswers> {
    let existing_static_routes = load_effective_static_routes_defaults(bundle_path, tenant, team)?;
    let static_routes_need_prompt = match loaded.platform_setup.static_routes.as_ref() {
        None => true,
        Some(answers) => StaticRoutesPolicy::normalize(Some(answers), env).is_err(),
    };
    if static_routes_need_prompt {
        let static_routes =
            if let Some(current_answers) = loaded.platform_setup.static_routes.as_ref() {
                prompt_static_routes_policy_with_answers(
                    env,
                    Some(current_answers),
                    existing_static_routes.as_ref(),
                )?
            } else {
                prompt_static_routes_policy(env, existing_static_routes.as_ref())?
            };
        loaded.platform_setup.static_routes = Some(static_routes.to_answers());
    }
    if loaded.platform_setup.deployment_targets.is_empty() {
        loaded.platform_setup.deployment_targets =
            crate::deployment_targets::prompt_deployment_targets(
                &crate::deployment_targets::discover_deployer_pack_candidates(bundle_path)?,
            )?;
    }

    // ── Confirm environment variable placeholders ────────────────────────────
    let env_placeholders = collect_env_var_placeholders(&loaded);
    if !env_placeholders.is_empty() {
        let resolved_env_vars = confirm_env_var_placeholders(&env_placeholders)?;

        // Apply resolved env vars to the loaded answers
        if !resolved_env_vars.is_empty() {
            apply_resolved_env_vars(&mut loaded, &resolved_env_vars);
        }
    }

    let discovered = discovery::discover(bundle_path)?;

    // ── Collect and prompt shared questions once ────────────────────────────
    // Build FormSpecs for ALL providers to identify shared questions
    let all_provider_form_specs: Vec<wizard::ProviderFormSpec> = discovered
        .providers
        .iter()
        .filter_map(|provider| {
            setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id).map(
                |form_spec| wizard::ProviderFormSpec {
                    provider_id: provider.provider_id.clone(),
                    form_spec,
                },
            )
        })
        .collect();

    // Extract existing shared values from loaded answers
    // Look for values across all providers that might have shared questions
    let mut existing_shared_values = serde_json::Map::new();
    let shared_result = if all_provider_form_specs.len() > 1 {
        let result = wizard::collect_shared_questions(&all_provider_form_specs);
        // Find existing values for shared questions from any provider
        for question in &result.shared_questions {
            for (_provider_id, provider_answers) in &loaded.setup_answers {
                if let Some(value) = provider_answers.get(&question.id) {
                    // Use first non-empty value found
                    if !(value.is_null() || value.is_string() && value.as_str() == Some("")) {
                        existing_shared_values.insert(question.id.clone(), value.clone());
                        break;
                    }
                }
            }
        }
        Some(result)
    } else {
        None
    };

    // Prompt for shared questions (like public_base_url) once at the start
    // Pass existing values so already-answered questions are skipped
    let shared_answers = if let Some(ref result) = shared_result {
        if !result.shared_questions.is_empty() {
            let existing = serde_json::Value::Object(existing_shared_values);
            wizard::prompt_shared_questions(result, advanced, &existing)?
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        }
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // ── Complete answers for each provider ──────────────────────────────────
    for provider in &discovered.providers {
        let provider_id = &provider.provider_id;
        let existing = loaded
            .setup_answers
            .get(provider_id)
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

        // Merge shared answers with existing answers.
        // Shared answers (user just entered) take precedence over existing values.
        let mut merged = existing.as_object().cloned().unwrap_or_default();
        if let Some(shared_obj) = shared_answers.as_object() {
            for (key, value) in shared_obj {
                // Only apply shared answer if it's non-empty
                let is_non_empty =
                    !(value.is_null() || value.is_string() && value.as_str() == Some(""));
                if is_non_empty {
                    merged.insert(key.clone(), value.clone());
                }
            }
        }
        let merged_value = serde_json::Value::Object(merged);

        let form_spec = setup_to_formspec::pack_to_form_spec(&provider.pack_path, provider_id);
        let completed = if let Some(spec) = form_spec {
            if spec.questions.is_empty() {
                existing
            } else {
                wizard::prompt_form_spec_answers_with_existing(
                    &spec,
                    provider_id,
                    advanced,
                    &merged_value,
                )?
            }
        } else {
            existing
        };
        loaded.setup_answers.insert(provider_id.clone(), completed);
    }

    Ok(loaded)
}

/// Ensure deployment targets are present if bundle has deployer packs.
pub fn ensure_deployment_targets_present(bundle_path: &Path, loaded: &LoadedAnswers) -> Result<()> {
    if !loaded.platform_setup.deployment_targets.is_empty() {
        return Ok(());
    }
    let candidates = crate::deployment_targets::discover_deployer_pack_candidates(bundle_path)?;
    if candidates.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "bundle contains deployer packs ({}) but answers did not define platform_setup.deployment_targets",
        candidates
            .iter()
            .map(|value| value.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::resolve_setup_scope;
    use crate::engine::LoadedAnswers;

    #[test]
    fn resolve_setup_scope_prefers_answers_when_cli_is_default() {
        let loaded = LoadedAnswers {
            tenant: Some("acme".to_string()),
            team: Some("core".to_string()),
            env: Some("prod".to_string()),
            ..Default::default()
        };
        let resolved = resolve_setup_scope("demo".to_string(), None, "dev".to_string(), &loaded);
        assert_eq!(resolved.0, "acme");
        assert_eq!(resolved.1.as_deref(), Some("core"));
        assert_eq!(resolved.2, "prod");
    }

    #[test]
    fn resolve_setup_scope_keeps_explicit_cli_values() {
        let loaded = LoadedAnswers {
            tenant: Some("acme".to_string()),
            team: Some("core".to_string()),
            env: Some("prod".to_string()),
            ..Default::default()
        };
        let resolved = resolve_setup_scope(
            "sandbox".to_string(),
            Some("ops".to_string()),
            "staging".to_string(),
            &loaded,
        );
        assert_eq!(resolved.0, "sandbox");
        assert_eq!(resolved.1.as_deref(), Some("ops"));
        assert_eq!(resolved.2, "staging");
    }
}
