//! CLI helper functions for greentic-setup.

mod bundle;
mod env_vars;
mod prompts;

use std::path::Path;

use anyhow::Result;
use qa_spec::{VisibilityMode, resolve_visibility};
use serde_json::Value;

use crate::deployment_targets::DeploymentTargetRecord;
use crate::discovery;
use crate::engine::LoadedAnswers;
use crate::platform_setup::{
    PlatformSetupAnswers, StaticRoutesPolicy, TunnelAnswers, load_effective_static_routes_defaults,
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

    // If tenant is still the CLI default ("demo") and it did not come from the
    // answers file, detect the actual tenant from existing directories.
    if tenant == "demo"
        && loaded.tenant.is_none()
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

fn has_cloud_deployment_target(targets: &[DeploymentTargetRecord]) -> bool {
    targets
        .iter()
        .any(|record| matches!(record.target.as_str(), "aws" | "gcp" | "azure"))
}

fn default_no_tunnel_answers() -> TunnelAnswers {
    TunnelAnswers {
        mode: Some("off".to_string()),
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
    let deployer_candidates =
        crate::deployment_targets::discover_deployer_pack_candidates(bundle_path)?;
    let deployment_targets =
        crate::deployment_targets::prompt_deployment_targets(&deployer_candidates)?;

    // Prompt for tunnel mode when no deployer packs are present (local dev).
    let tunnel = if has_cloud_deployment_target(&deployment_targets) {
        Some(default_no_tunnel_answers())
    } else if deployer_candidates.is_empty() {
        Some(crate::platform_setup::prompt_tunnel_mode(None)?)
    } else {
        None
    };

    let discovered = discovery::discover(bundle_path)?;
    let setup_targets = discovered.setup_targets();

    if setup_targets.is_empty() {
        println!("No setup packs found in bundle. Nothing to configure.");
        return Ok(LoadedAnswers {
            tenant: None,
            team: None,
            env: None,
            platform_setup: PlatformSetupAnswers {
                static_routes: Some(static_routes.to_answers()),
                deployment_targets,
                tunnel,
            },
            setup_answers: all_answers,
        });
    }

    println!("Found {} pack(s) to configure:", setup_targets.len());
    for provider in &setup_targets {
        println!("  - {} ({})", provider.provider_id, provider.domain);
    }
    println!();

    // ── Collect and prompt shared questions once ────────────────────────────
    // Build FormSpecs for all providers to identify shared questions
    let provider_form_specs: Vec<wizard::ProviderFormSpec> = setup_targets
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
    for provider in &setup_targets {
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
            tunnel,
        },
        setup_answers: all_answers,
    })
}

/// Complete loaded answers by prompting for missing values.
///
/// When `non_interactive` is true, prompts are skipped so a missing
/// `platform_setup` field doesn't deadlock automation runs on a hidden
/// TTY prompt — the value is left for the runtime defaults (or for
/// `ensure_required_setup_answers_present` to flag downstream).
pub fn complete_loaded_answers_with_prompts(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    advanced: bool,
    non_interactive: bool,
    mut loaded: LoadedAnswers,
) -> Result<LoadedAnswers> {
    let existing_static_routes = load_effective_static_routes_defaults(bundle_path, tenant, team)?;
    let static_routes_need_prompt = match loaded.platform_setup.static_routes.as_ref() {
        None => true,
        Some(answers) => StaticRoutesPolicy::normalize(Some(answers), env).is_err(),
    };
    if static_routes_need_prompt && !non_interactive {
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
    let deployer_candidates =
        crate::deployment_targets::discover_deployer_pack_candidates(bundle_path)?;
    if loaded.platform_setup.deployment_targets.is_empty() && !non_interactive {
        loaded.platform_setup.deployment_targets =
            crate::deployment_targets::prompt_deployment_targets(&deployer_candidates)?;
    }
    if has_cloud_deployment_target(&loaded.platform_setup.deployment_targets) {
        loaded.platform_setup.tunnel = Some(default_no_tunnel_answers());
    } else if deployer_candidates.is_empty()
        && loaded.platform_setup.tunnel.is_none()
        && !non_interactive
    {
        loaded.platform_setup.tunnel = Some(crate::platform_setup::prompt_tunnel_mode(None)?);
    }

    // ── Confirm environment variable placeholders ────────────────────────────
    // Skip in non-interactive mode: leave any unresolved `${VAR}` placeholders
    // in place. `answer_satisfies_question` accepts placeholder strings as
    // valid runtime-resolved values, and `ensure_required_setup_answers_present`
    // will fail-fast downstream if anything truly required is missing.
    if !non_interactive {
        let env_placeholders = collect_env_var_placeholders(&loaded);
        if !env_placeholders.is_empty() {
            let resolved_env_vars = confirm_env_var_placeholders(&env_placeholders)?;

            // Apply resolved env vars to the loaded answers
            if !resolved_env_vars.is_empty() {
                apply_resolved_env_vars(&mut loaded, &resolved_env_vars);
            }
        }
    }

    let discovered = discovery::discover(bundle_path)?;
    let setup_targets = discovered.setup_targets();

    // ── Collect and prompt shared questions once ────────────────────────────
    // Build FormSpecs for ALL providers to identify shared questions
    let all_provider_form_specs: Vec<wizard::ProviderFormSpec> = setup_targets
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
    // Pass existing values so already-answered questions are skipped.
    // Skip entirely in non-interactive mode: the loaded answers file is
    // expected to provide everything required, and the engine's
    // `ensure_required_setup_answers_present()` will fail-fast downstream
    // if anything required is missing.
    let shared_answers = if !non_interactive {
        if let Some(ref result) = shared_result {
            if !result.shared_questions.is_empty() {
                let existing = serde_json::Value::Object(existing_shared_values);
                wizard::prompt_shared_questions(result, advanced, &existing)?
            } else {
                serde_json::Value::Object(serde_json::Map::new())
            }
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        }
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // ── Complete answers for each provider ──────────────────────────────────
    for provider in &setup_targets {
        let provider_id = &provider.provider_id;
        let existing = loaded
            .setup_answers
            .get(provider_id)
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

        // In non-interactive mode, never prompt. Preserve whatever was loaded
        // from the answers file as-is; downstream validation fails fast on
        // missing required fields.
        if non_interactive {
            loaded.setup_answers.insert(provider_id.clone(), existing);
            continue;
        }

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

/// Ensure loaded answers satisfy all visible required setup questions.
pub fn ensure_required_setup_answers_present(
    bundle_path: &Path,
    loaded: &LoadedAnswers,
) -> Result<()> {
    let discovered = discovery::discover(bundle_path)?;
    for provider in discovered.setup_targets() {
        let Some(spec) =
            setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id)
        else {
            continue;
        };
        if spec.questions.is_empty() {
            continue;
        }

        let answers = loaded
            .setup_answers
            .get(&provider.provider_id)
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let answer_map = answers.as_object().ok_or_else(|| {
            anyhow::anyhow!("answers for {} must be an object", provider.provider_id)
        })?;
        let visibility = resolve_visibility(&spec, &answers, VisibilityMode::Visible);

        for question in spec.questions.iter().filter(|question| question.required) {
            if !visibility.get(&question.id).copied().unwrap_or(true) {
                continue;
            }
            let Some(value) = answer_map.get(&question.id) else {
                anyhow::bail!(
                    "missing required setup answer for {}.{}",
                    provider.provider_id,
                    question.id
                );
            };
            if !wizard::answer_satisfies_question(question, value) {
                anyhow::bail!(
                    "missing required setup answer for {}.{}",
                    provider.provider_id,
                    question.id
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        default_no_tunnel_answers, has_cloud_deployment_target, resolve_setup_scope,
        resolve_setup_scope_with_bundle,
    };
    use crate::deployment_targets::DeploymentTargetRecord;
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

    #[test]
    fn bundle_detection_does_not_override_answers_tenant_demo() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(temp.path().join("tenants").join("default")).expect("tenant dir");

        let loaded = LoadedAnswers {
            tenant: Some("demo".to_string()),
            ..Default::default()
        };

        let (tenant, team, env) = resolve_setup_scope_with_bundle(
            "demo".to_string(),
            None,
            "dev".to_string(),
            &loaded,
            temp.path(),
        );

        assert_eq!(tenant, "demo");
        assert_eq!(team, None);
        assert_eq!(env, "dev");
    }

    #[test]
    fn detects_cloud_deployment_targets() {
        assert!(has_cloud_deployment_target(&[
            DeploymentTargetRecord {
                target: "aws".to_string(),
                provider_pack: None,
                default: None,
            },
            DeploymentTargetRecord {
                target: "runtime".to_string(),
                provider_pack: None,
                default: None,
            },
        ]));
        assert!(!has_cloud_deployment_target(&[
            DeploymentTargetRecord {
                target: "runtime".to_string(),
                provider_pack: None,
                default: None,
            },
            DeploymentTargetRecord {
                target: "single-vm".to_string(),
                provider_pack: None,
                default: None,
            },
        ]));
    }

    #[test]
    fn default_no_tunnel_answers_for_cloud_sets_off_mode() {
        assert_eq!(default_no_tunnel_answers().mode.as_deref(), Some("off"));
    }
}
