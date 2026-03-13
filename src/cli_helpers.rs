//! CLI helper functions for greentic-setup.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::cli_args::Cli;
use crate::cli_i18n::CliI18n;
use crate::discovery;
use crate::engine::LoadedAnswers;
use crate::platform_setup::{
    PlatformSetupAnswers, load_static_routes_artifact, prompt_static_routes_policy,
};
use crate::qa::wizard;
use crate::setup_to_formspec;

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

    let path_str = path.to_string_lossy();
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
    env: &str,
    advanced: bool,
) -> Result<LoadedAnswers> {
    use serde_json::Value;

    let mut all_answers = serde_json::Map::new();
    let existing_static_routes = load_static_routes_artifact(bundle_path)?;
    let static_routes = prompt_static_routes_policy(env, existing_static_routes.as_ref())?;

    let discovered = discovery::discover(bundle_path)?;

    if discovered.providers.is_empty() {
        println!("No providers found in bundle. Nothing to configure.");
        return Ok(LoadedAnswers {
            platform_setup: PlatformSetupAnswers {
                static_routes: Some(static_routes.to_answers()),
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

    for provider in &discovered.providers {
        let provider_id = &provider.provider_id;
        let form_spec = setup_to_formspec::pack_to_form_spec(&provider.pack_path, provider_id);

        if let Some(spec) = form_spec {
            if spec.questions.is_empty() {
                println!("Provider {}: No configuration required.", provider_id);
                all_answers.insert(provider_id.clone(), Value::Object(serde_json::Map::new()));
                continue;
            }

            let answers = wizard::prompt_form_spec_answers(&spec, provider_id, advanced)?;
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
        },
        setup_answers: all_answers,
    })
}
