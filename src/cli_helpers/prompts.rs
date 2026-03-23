//! Interactive prompts for setup parameters.
//!
//! Functions for prompting users to enter setup configuration interactively.

use std::io::{self, Write as _};
use std::path::PathBuf;

use anyhow::Result;

use crate::cli_args::Cli;
use crate::cli_i18n::CliI18n;
use crate::discovery;

use super::bundle::resolve_bundle_source;
use super::bundle::{detect_domain_from_filename, resolve_pack_source};

/// Parameters collected from interactive prompts.
pub struct SetupParams {
    pub bundle: PathBuf,
    pub tenant: String,
    pub team: Option<String>,
    pub env: String,
    pub advanced: bool,
}

/// Prompt the user for setup parameters when no arguments are given.
pub fn prompt_setup_params(cli: &Cli, i18n: &CliI18n) -> Result<SetupParams> {
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
