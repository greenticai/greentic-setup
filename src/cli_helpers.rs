//! CLI helper functions for greentic-setup.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::cli_args::Cli;
use crate::cli_i18n::CliI18n;
use crate::discovery;
use crate::engine::LoadedAnswers;
use crate::platform_setup::{
    PlatformSetupAnswers, StaticRoutesPolicy, load_effective_static_routes_defaults,
    prompt_static_routes_policy, prompt_static_routes_policy_with_answers,
};
use crate::qa::wizard;
use crate::setup_to_formspec;

/// Represents an environment variable placeholder found in answers.
#[derive(Debug, Clone)]
pub struct EnvVarPlaceholder {
    /// The placeholder string (e.g., "${PUBLIC_BASE_URL}")
    pub placeholder: String,
    /// The environment variable name (e.g., "PUBLIC_BASE_URL")
    pub var_name: String,
    /// The resolved value from environment, if available
    pub resolved_value: Option<String>,
    /// Which providers use this placeholder
    pub used_by: Vec<String>,
}

/// Collect all environment variable placeholders from loaded answers.
pub fn collect_env_var_placeholders(loaded: &LoadedAnswers) -> Vec<EnvVarPlaceholder> {
    let mut placeholders: BTreeMap<String, EnvVarPlaceholder> = BTreeMap::new();

    // Check platform_setup.static_routes.public_base_url
    if let Some(ref routes) = loaded.platform_setup.static_routes
        && let Some(ref value) = routes.public_base_url
        && let Some(var_name) = extract_env_var_name(value)
    {
        let entry = placeholders
            .entry(var_name.clone())
            .or_insert_with(|| EnvVarPlaceholder {
                placeholder: value.to_string(),
                var_name: var_name.clone(),
                resolved_value: std::env::var(&var_name).ok(),
                used_by: Vec::new(),
            });
        entry.used_by.push("platform_setup".to_string());
    }

    // Check each provider's answers
    for (provider_id, answers) in &loaded.setup_answers {
        if let Some(obj) = answers.as_object() {
            for (key, value) in obj {
                if let Some(s) = value.as_str()
                    && let Some(var_name) = extract_env_var_name(s)
                {
                    let entry =
                        placeholders
                            .entry(var_name.clone())
                            .or_insert_with(|| EnvVarPlaceholder {
                                placeholder: s.to_string(),
                                var_name: var_name.clone(),
                                resolved_value: std::env::var(&var_name).ok(),
                                used_by: Vec::new(),
                            });
                    let provider_key = format!("{provider_id}.{key}");
                    if !entry.used_by.contains(&provider_key) {
                        entry.used_by.push(provider_key);
                    }
                }
            }
        }
    }

    placeholders.into_values().collect()
}

/// Extract environment variable name from a placeholder like "${VAR_NAME}".
fn extract_env_var_name(value: &str) -> Option<String> {
    if value.starts_with("${") && value.ends_with('}') {
        Some(value[2..value.len() - 1].to_string())
    } else {
        None
    }
}

/// Display environment variable placeholders and prompt for missing values.
///
/// Returns a map of env var name -> resolved value (either from env or user input).
/// Returns `Err` if user cancels.
pub fn confirm_env_var_placeholders(
    placeholders: &[EnvVarPlaceholder],
) -> Result<std::collections::HashMap<String, String>> {
    use rpassword::prompt_password;

    let mut resolved: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    if placeholders.is_empty() {
        return Ok(resolved);
    }

    println!();
    println!("── Environment Variables ──");
    println!("The following environment variables will be used:\n");

    let mut missing: Vec<&EnvVarPlaceholder> = Vec::new();

    for placeholder in placeholders {
        match &placeholder.resolved_value {
            Some(value) => {
                // Mask sensitive values (tokens, passwords, secrets)
                let display_value = if is_sensitive_var(&placeholder.var_name) {
                    mask_value(value)
                } else {
                    value.clone()
                };
                println!(
                    "  ${:<30} \x1b[32m✓\x1b[0m {}",
                    placeholder.var_name, display_value
                );
                resolved.insert(placeholder.var_name.clone(), value.clone());
            }
            None => {
                println!("  ${:<30} \x1b[31m✗ NOT SET\x1b[0m", placeholder.var_name);
                missing.push(placeholder);
            }
        };
    }

    println!();

    // Prompt for missing values
    if !missing.is_empty() {
        println!("Enter values for missing environment variables:");
        println!("(Press Enter to skip and keep placeholder, or 'q' to cancel)\n");

        for placeholder in missing {
            let is_sensitive = is_sensitive_var(&placeholder.var_name);
            let prompt = format!("  ${}: ", placeholder.var_name);

            let input = if is_sensitive {
                // Use secure password input for sensitive values
                print!("{}", prompt);
                io::stdout().flush()?;
                prompt_password("").unwrap_or_default()
            } else {
                print!("{}", prompt);
                io::stdout().flush()?;
                let mut buf = String::new();
                io::stdin().read_line(&mut buf)?;
                buf.trim().to_string()
            };

            if input.eq_ignore_ascii_case("q") {
                bail!("Setup cancelled by user");
            }

            if !input.is_empty() {
                resolved.insert(placeholder.var_name.clone(), input);
            }
        }

        println!();
    }

    Ok(resolved)
}

/// Apply resolved environment variable values to loaded answers.
///
/// Replaces `${VAR_NAME}` placeholders with actual values from the resolved map.
fn apply_resolved_env_vars(
    loaded: &mut LoadedAnswers,
    resolved: &std::collections::HashMap<String, String>,
) {
    // Apply to platform_setup.static_routes.public_base_url
    if let Some(ref mut routes) = loaded.platform_setup.static_routes
        && let Some(ref mut value) = routes.public_base_url
        && let Some(var_name) = extract_env_var_name(value)
        && let Some(resolved_value) = resolved.get(&var_name)
    {
        *value = resolved_value.clone();
    }

    // Apply to each provider's answers
    for (_provider_id, answers) in loaded.setup_answers.iter_mut() {
        if let Some(obj) = answers.as_object_mut() {
            for (_key, value) in obj.iter_mut() {
                if let Some(s) = value.as_str()
                    && let Some(var_name) = extract_env_var_name(s)
                    && let Some(resolved_value) = resolved.get(&var_name)
                {
                    *value = serde_json::Value::String(resolved_value.clone());
                }
            }
        }
    }
}

/// Check if a variable name suggests sensitive data.
fn is_sensitive_var(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("token")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("key")
        || lower.contains("credential")
}

/// Mask a sensitive value, showing only first and last 4 characters.
fn mask_value(value: &str) -> String {
    if value.len() <= 12 {
        "*".repeat(value.len())
    } else {
        format!("{}...{}", &value[..4], &value[value.len() - 4..])
    }
}

/// Parameters collected from interactive prompts.
pub struct SetupParams {
    pub bundle: PathBuf,
    pub tenant: String,
    pub team: Option<String>,
    pub env: String,
    pub advanced: bool,
}

/// Resolve bundle source - supports both directories and .gtbundle files.
pub fn resolve_bundle_source(path: &std::path::Path, i18n: &CliI18n) -> Result<PathBuf> {
    use crate::gtbundle;

    let path_str = path.to_string_lossy();
    if path_str.starts_with("https://") || path_str.starts_with("http://") {
        println!("{}", i18n.t("cli.simple.extracting"));
        let temp_dir = download_and_extract_remote_bundle(&path_str)
            .context("failed to fetch and extract remote bundle archive")?;
        println!(
            "{}",
            i18n.tf(
                "cli.simple.extracted_to",
                &[&temp_dir.display().to_string()]
            )
        );
        return Ok(temp_dir);
    }

    if gtbundle::is_gtbundle_file(path) {
        println!("{}", i18n.t("cli.simple.extracting"));
        let temp_dir = gtbundle::extract_gtbundle_to_temp(path)
            .context("failed to extract .gtbundle archive")?;
        println!(
            "{}",
            i18n.tf(
                "cli.simple.extracted_to",
                &[&temp_dir.display().to_string()]
            )
        );
        return Ok(temp_dir);
    }

    if gtbundle::is_gtbundle_dir(path) {
        return Ok(path.to_path_buf());
    }
    if path_str.ends_with(".gtbundle") && !path.exists() {
        bail!(
            "{}",
            i18n.tf(
                "setup.error.bundle_not_found",
                &[&path.display().to_string()]
            )
        );
    }

    if path.is_dir() {
        Ok(path.to_path_buf())
    } else if path.exists() {
        bail!(
            "{}",
            i18n.tf(
                "cli.simple.expected_bundle_format",
                &[&path.display().to_string()]
            )
        );
    } else {
        bail!(
            "{}",
            i18n.tf(
                "setup.error.bundle_not_found",
                &[&path.display().to_string()]
            )
        );
    }
}

fn download_and_extract_remote_bundle(url: &str) -> Result<PathBuf> {
    use crate::gtbundle;

    let response = ureq::get(url)
        .call()
        .map_err(|err| anyhow::anyhow!("failed to fetch {url}: {err}"))?;
    let bytes = response
        .into_body()
        .read_to_vec()
        .map_err(|err| anyhow::anyhow!("failed to read {url}: {err}"))?;

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = std::env::temp_dir().join(format!("greentic-setup-remote-{nonce}"));
    fs::create_dir_all(&base)?;

    let file_name = url
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("bundle.gtbundle");
    let archive_path = base.join(file_name);
    fs::write(&archive_path, bytes)?;

    if !gtbundle::is_gtbundle_file(&archive_path) {
        bail!("remote bundle URL must point to a .gtbundle archive: {url}");
    }

    gtbundle::extract_gtbundle_to_temp(&archive_path)
}

/// Resolve bundle directory from optional path argument.
pub fn resolve_bundle_dir(bundle: Option<PathBuf>) -> Result<PathBuf> {
    match bundle {
        Some(path) => Ok(path),
        None => std::env::current_dir().context("failed to get current directory"),
    }
}

/// Recursively copy a directory tree.
pub fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf, _only_used: bool) -> Result<()> {
    if !src.is_dir() {
        bail!("source is not a directory: {}", src.display());
    }

    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path, _only_used)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

/// Detect provider domain from .gtpack filename prefix.
///
/// Known prefixes: messaging-, state-, telemetry-, events-, oauth-, secrets-.
/// Falls back to "messaging" for unrecognized prefixes.
pub fn detect_domain_from_filename(filename: &str) -> &'static str {
    let stem = filename.strip_suffix(".gtpack").unwrap_or(filename);
    if stem.starts_with("messaging-")
        || stem.starts_with("state-")
        || stem.starts_with("telemetry-")
    {
        "messaging"
    } else if stem.starts_with("events-") || stem.starts_with("event-") {
        "events"
    } else if stem.starts_with("oauth-") {
        "oauth"
    } else if stem.starts_with("secrets-") {
        "secrets"
    } else {
        "messaging"
    }
}

/// Resolve a pack source (local path or OCI reference) to a local file path.
pub fn resolve_pack_source(source: &str) -> Result<PathBuf> {
    use crate::bundle_source::BundleSource;

    let parsed = BundleSource::parse(source)?;

    if parsed.is_local() {
        let path = parsed.resolve()?;
        if path.extension().and_then(|e| e.to_str()) != Some("gtpack") {
            anyhow::bail!("Not a .gtpack file: {source}");
        }
        Ok(path)
    } else {
        println!("    Fetching from registry...");
        let path = parsed.resolve()?;
        println!("    Downloaded to cache: {}", path.display());
        Ok(path)
    }
}

/// Prompt the user for setup parameters when no arguments are given.
pub fn prompt_setup_params(cli: &Cli, i18n: &CliI18n) -> Result<SetupParams> {
    use std::io::{self, Write as _};

    println!();
    println!("Greentic Setup");
    println!("==============");
    println!();
    println!("Configure a bundle for deployment. A bundle is a directory or");
    println!(".gtbundle archive containing provider packs and configuration.");
    println!();

    // Bundle name
    println!("  Bundle name (required)");
    println!("  A short name for this bundle (used as the directory name).");
    println!("  Examples: my-demo  telecom-bot  customer-support");
    print!("  > ");
    io::stdout().flush()?;
    let mut name_input = String::new();
    io::stdin().read_line(&mut name_input)?;
    let bundle_name = name_input.trim().to_string();
    if bundle_name.is_empty() {
        anyhow::bail!("Bundle name is required.");
    }
    println!();

    // Bundle path
    let default_path = format!("./{bundle_name}");
    println!("  Bundle path");
    println!("  Path to a bundle directory or .gtbundle file.");
    println!("  Press Enter to use: {default_path}");
    print!("  > ");
    io::stdout().flush()?;
    let mut bundle_input = String::new();
    io::stdin().read_line(&mut bundle_input)?;
    let bundle_str = bundle_input.trim();
    let bundle = if bundle_str.is_empty() {
        PathBuf::from(&default_path)
    } else {
        PathBuf::from(bundle_str)
    };

    // Resolve bundle and discover existing packs
    let bundle_dir = resolve_bundle_source(&bundle, i18n)?;
    let discovered =
        discovery::discover(&bundle_dir).unwrap_or_else(|_| discovery::DiscoveryResult {
            domains: discovery::DetectedDomains {
                messaging: false,
                events: false,
                oauth: false,
                state: false,
                secrets: false,
            },
            providers: Vec::new(),
        });

    // Show existing packs
    println!();
    if discovered.providers.is_empty() {
        println!("  No packs found in bundle.");
    } else {
        println!("  Found {} pack(s) in bundle:", discovered.providers.len());
        for p in &discovered.providers {
            println!("    - {} ({})", p.provider_id, p.domain);
        }
    }

    // Add packs loop
    println!();
    println!("  Add packs to bundle");
    println!("  Enter path to a .gtpack file or OCI reference, or press Enter to skip.");
    println!("  Local:  ./messaging-telegram.gtpack  ../packs/state-redis.gtpack");
    println!("  OCI:    oci://ghcr.io/greentic-ai-org/packs/mcp-github.gtpack:latest");
    loop {
        print!("  add pack> ");
        io::stdout().flush()?;
        let mut pack_input = String::new();
        io::stdin().read_line(&mut pack_input)?;
        let pack_str = pack_input.trim();
        if pack_str.is_empty() {
            break;
        }

        match resolve_pack_source(pack_str) {
            Ok(pack_path) => {
                let filename = pack_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("pack.gtpack");
                let domain = detect_domain_from_filename(filename);

                let target_dir = bundle_dir.join("providers").join(domain);
                std::fs::create_dir_all(&target_dir)?;

                let target = target_dir.join(filename);
                std::fs::copy(&pack_path, &target)?;
                println!("    Added {filename} -> providers/{domain}/");
            }
            Err(e) => {
                println!("    Error: {e}");
                continue;
            }
        }
    }

    // Tenant
    let default_tenant = &cli.tenant;
    println!();
    println!("  Tenant (optional)");
    println!("  Tenant identifier for multi-tenant isolation.");
    print!("  > (default: {default_tenant}) ");
    io::stdout().flush()?;
    let mut tenant_input = String::new();
    io::stdin().read_line(&mut tenant_input)?;
    let tenant = if tenant_input.trim().is_empty() {
        default_tenant.clone()
    } else {
        tenant_input.trim().to_string()
    };

    // Team
    println!();
    println!("  Team (optional)");
    println!("  Team within the tenant. Leave blank for default.");
    print!("  > ");
    io::stdout().flush()?;
    let mut team_input = String::new();
    io::stdin().read_line(&mut team_input)?;
    let team = if team_input.trim().is_empty() {
        None
    } else {
        Some(team_input.trim().to_string())
    };

    // Env
    let default_env = &cli.env;
    println!();
    println!("  Environment (optional)");
    println!("  Deployment environment for secrets and configuration.");
    print!("  > (default: {default_env}) ");
    io::stdout().flush()?;
    let mut env_input = String::new();
    io::stdin().read_line(&mut env_input)?;
    let env = if env_input.trim().is_empty() {
        default_env.clone()
    } else {
        env_input.trim().to_string()
    };

    // Advanced mode
    println!();
    println!("  Advanced mode");
    println!("  Show all configuration options including optional ones.");
    print!("  > [y/N] ");
    io::stdout().flush()?;
    let mut adv_input = String::new();
    io::stdin().read_line(&mut adv_input)?;
    let advanced = matches!(
        adv_input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes" | "true" | "1"
    );

    println!();

    Ok(SetupParams {
        bundle,
        tenant,
        team,
        env,
        advanced,
    })
}

/// Run interactive wizard for all discovered packs in the bundle.
pub fn run_interactive_wizard(
    bundle_path: &std::path::Path,
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
            platform_setup: PlatformSetupAnswers {
                static_routes: Some(static_routes.to_answers()),
                deployment_targets,
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
        platform_setup: PlatformSetupAnswers {
            static_routes: Some(static_routes.to_answers()),
            deployment_targets,
        },
        setup_answers: all_answers,
    })
}

pub fn complete_loaded_answers_with_prompts(
    bundle_path: &std::path::Path,
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

pub fn ensure_deployment_targets_present(
    bundle_path: &std::path::Path,
    loaded: &LoadedAnswers,
) -> Result<()> {
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
