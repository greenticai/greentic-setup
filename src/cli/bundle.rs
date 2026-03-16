//! Bundle CLI commands.
//!
//! Handles bundle lifecycle management commands:
//! - init: Initialize a new bundle directory
//! - add: Add a pack to a bundle
//! - setup: Run setup flow for provider(s)
//! - update: Update provider configuration
//! - remove: Remove a provider from a bundle
//! - build: Build a portable bundle
//! - list: List packs/flows in a bundle
//! - status: Show bundle status

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;

use crate::bundle;
use crate::cli_i18n::CliI18n;
use crate::discovery;
use crate::engine::{SetupConfig, SetupRequest};
use crate::gtbundle;
use crate::plan::TenantSelection;
use crate::qa::wizard;
use crate::setup_to_formspec;
use crate::{SetupEngine, SetupMode};

/// Bundle init command arguments.
#[derive(Args, Debug, Clone)]
pub struct BundleInitArgs {
    /// Bundle directory (default: current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Bundle name
    #[arg(long = "name", short = 'n')]
    pub name: Option<String>,
}

/// Bundle add command arguments.
#[derive(Args, Debug, Clone)]
pub struct BundleAddArgs {
    /// Pack reference (local path or OCI reference)
    #[arg(value_name = "PACK_REF")]
    pub pack_ref: String,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    pub tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Dry run (don't actually add)
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// Bundle setup/update command arguments.
#[derive(Args, Debug, Clone)]
pub struct BundleSetupArgs {
    /// Provider ID to setup/update (optional, setup all if not specified)
    #[arg(value_name = "PROVIDER_ID")]
    pub provider_id: Option<String>,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Answers file (JSON/YAML)
    #[arg(long = "answers", short = 'a')]
    pub answers: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    pub tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Filter by domain (messaging/events/secrets/oauth/all)
    #[arg(long = "domain", short = 'd', default_value = "all")]
    pub domain: String,
    /// Number of parallel setup operations
    #[arg(long = "parallel", default_value = "1")]
    pub parallel: usize,
    /// Backup existing config before setup
    #[arg(long = "backup")]
    pub backup: bool,
    /// Skip secrets initialization
    #[arg(long = "skip-secrets-init")]
    pub skip_secrets_init: bool,
    /// Continue on error (best effort)
    #[arg(long = "best-effort")]
    pub best_effort: bool,
    /// Non-interactive mode (require --answers)
    #[arg(long = "non-interactive")]
    pub non_interactive: bool,
    /// Dry run (plan only, don't execute)
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Emit answers template JSON (use with --dry-run)
    #[arg(long = "emit-answers")]
    pub emit_answers: Option<PathBuf>,
}

/// Bundle remove command arguments.
#[derive(Args, Debug, Clone)]
pub struct BundleRemoveArgs {
    /// Provider ID to remove
    #[arg(value_name = "PROVIDER_ID")]
    pub provider_id: String,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    pub tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Force removal without confirmation
    #[arg(long = "force", short = 'f')]
    pub force: bool,
}

/// Bundle build command arguments.
#[derive(Args, Debug, Clone)]
pub struct BundleBuildArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Output directory for portable bundle
    #[arg(long = "out", short = 'o')]
    pub out: PathBuf,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't')]
    pub tenant: Option<String>,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Only include used providers
    #[arg(long = "only-used-providers")]
    pub only_used_providers: bool,
    /// Run doctor validation after build
    #[arg(long = "doctor")]
    pub doctor: bool,
    /// Skip doctor validation
    #[arg(long = "skip-doctor")]
    pub skip_doctor: bool,
}

/// Bundle list command arguments.
#[derive(Args, Debug, Clone)]
pub struct BundleListArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Filter by domain (messaging/events/secrets/oauth)
    #[arg(long = "domain", short = 'd', default_value = "messaging")]
    pub domain: String,
    /// Show flows for a specific pack
    #[arg(long = "pack", short = 'p')]
    pub pack: Option<String>,
    /// Output format (text/json)
    #[arg(long = "format", default_value = "text")]
    pub format: String,
}

/// Bundle status command arguments.
#[derive(Args, Debug, Clone)]
pub struct BundleStatusArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Output format (text/json)
    #[arg(long = "format", default_value = "text")]
    pub format: String,
}

/// Resolve bundle directory from optional path.
pub fn resolve_bundle_dir(bundle: Option<PathBuf>) -> Result<PathBuf> {
    match bundle {
        Some(path) => Ok(path),
        None => std::env::current_dir().context("failed to get current directory"),
    }
}

/// Initialize a new bundle directory.
pub fn init(args: BundleInitArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = args.path.unwrap_or_else(|| PathBuf::from("."));
    let bundle_path = bundle_dir.display().to_string();

    if bundle_dir.join("greentic.demo.yaml").exists() {
        println!("{}", i18n.tf("cli.bundle.init.exists", &[&bundle_path]));
        return Ok(());
    }

    println!("{}", i18n.tf("cli.bundle.init.creating", &[&bundle_path]));

    bundle::create_demo_bundle_structure(&bundle_dir, args.name.as_deref())
        .context(i18n.t("cli.error.failed_create_bundle"))?;

    println!("{}", i18n.tf("cli.bundle.init.created", &[&bundle_path]));
    println!("\n{}", i18n.t("cli.bundle.init.next_steps"));
    println!("{}", i18n.tf("cli.bundle.init.step_add", &[&bundle_path]));
    println!("{}", i18n.tf("cli.bundle.init.step_setup", &[&bundle_path]));

    Ok(())
}

/// Add a pack to a bundle.
pub fn add(args: BundleAddArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;
    let bundle_path = bundle_dir.display().to_string();

    println!("{}", i18n.t("cli.bundle.add.adding"));
    println!("{}", i18n.tf("cli.bundle.add.pack_ref", &[&args.pack_ref]));
    println!("{}", i18n.tf("cli.bundle.add.bundle", &[&bundle_path]));
    println!("{}", i18n.tf("cli.bundle.add.tenant", &[&args.tenant]));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.team",
            &[args.team.as_deref().unwrap_or("default")]
        )
    );
    println!("{}", i18n.tf("cli.bundle.add.env", &[&args.env]));

    // Create bundle structure if it doesn't exist
    if !bundle_dir.join("greentic.demo.yaml").exists() {
        bundle::create_demo_bundle_structure(&bundle_dir, None)
            .context(i18n.t("cli.error.failed_create_bundle"))?;
        println!(
            "{}",
            i18n.tf("cli.bundle.add.created_structure", &[&bundle_path])
        );
    }

    // Build setup request
    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        pack_refs: vec![args.pack_ref.clone()],
        tenants: vec![TenantSelection {
            tenant: args.tenant.clone(),
            team: args.team.clone(),
            allow_paths: Vec::new(),
        }],
        ..Default::default()
    };

    let config = SetupConfig {
        tenant: args.tenant,
        team: args.team,
        env: args.env,
        offline: false,
        verbose: true,
    };
    let engine = SetupEngine::new(config);
    let plan = engine
        .plan(SetupMode::Create, &request, args.dry_run)
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    if args.dry_run {
        println!("\n{}", i18n.t("cli.bundle.add.dry_run"));
        return Ok(());
    }

    let report = engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;

    println!("\n{}", i18n.t("cli.bundle.add.success"));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.resolved",
            &[&report.resolved_packs.len().to_string()]
        )
    );

    Ok(())
}

/// Run setup flow for provider(s) in a bundle.
pub fn setup(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let provider_display = args
        .provider_id
        .clone()
        .unwrap_or_else(|| "all".to_string());

    println!("{}", i18n.t("cli.bundle.setup.setting_up"));
    println!(
        "{}",
        i18n.tf("cli.bundle.setup.provider", &[&provider_display])
    );
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.bundle",
            &[&bundle_dir.display().to_string()]
        )
    );
    println!("{}", i18n.tf("cli.bundle.add.tenant", &[&args.tenant]));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.team",
            &[args.team.as_deref().unwrap_or("default")]
        )
    );
    println!("{}", i18n.tf("cli.bundle.add.env", &[&args.env]));
    println!("{}", i18n.tf("cli.bundle.setup.domain", &[&args.domain]));

    let config = SetupConfig {
        tenant: args.tenant.clone(),
        team: args.team.clone(),
        env: args.env.clone(),
        offline: false,
        verbose: true,
    };
    let engine = SetupEngine::new(config);

    // Load answers (not required if --emit-answers is provided)
    let setup_answers = if let Some(answers_path) = &args.answers {
        engine
            .load_answers(answers_path)
            .context(i18n.t("cli.error.failed_read_answers"))?
    } else if args.emit_answers.is_some() {
        // Empty answers for emit mode - will generate template
        serde_json::Map::new()
    } else if args.non_interactive {
        bail!("{}", i18n.t("cli.error.answers_required"));
    } else {
        // Interactive mode - run wizard for discovered packs
        println!("\n{}", i18n.t("cli.simple.interactive_mode"));
        println!();
        run_interactive_wizard(&bundle_dir)?
    };

    let providers = args
        .provider_id
        .clone()
        .map_or_else(Vec::new, |id| vec![id]);

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        providers,
        tenants: vec![TenantSelection {
            tenant: args.tenant,
            team: args.team,
            allow_paths: Vec::new(),
        }],
        setup_answers,
        domain_filter: if args.domain == "all" {
            None
        } else {
            Some(args.domain.clone())
        },
        parallel: args.parallel,
        backup: args.backup,
        skip_secrets_init: args.skip_secrets_init,
        best_effort: args.best_effort,
        ..Default::default()
    };

    let plan = engine
        .plan(
            SetupMode::Create,
            &request,
            args.dry_run || args.emit_answers.is_some(),
        )
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    // Emit answers template if requested
    if let Some(emit_path) = &args.emit_answers {
        let emit_path_str = emit_path.display().to_string();
        engine
            .emit_answers(&plan, emit_path)
            .context(i18n.t("cli.error.failed_emit_answers"))?;
        println!(
            "\n{}",
            i18n.tf("cli.bundle.setup.emit_written", &[&emit_path_str])
        );
        println!(
            "{}",
            i18n.tf("cli.bundle.setup.emit_usage", &[&emit_path_str])
        );
        return Ok(());
    }

    if args.dry_run {
        println!(
            "\n{}",
            i18n.tf("cli.bundle.setup.dry_run", &[&provider_display])
        );
        return Ok(());
    }

    engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;

    println!(
        "\n{}",
        i18n.tf("cli.bundle.setup.complete", &[&provider_display])
    );

    Ok(())
}

/// Update a provider's configuration in a bundle.
pub fn update(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let provider_display = args
        .provider_id
        .clone()
        .unwrap_or_else(|| "all".to_string());

    println!("{}", i18n.t("cli.bundle.update.updating"));
    println!(
        "{}",
        i18n.tf("cli.bundle.setup.provider", &[&provider_display])
    );
    println!("{}", i18n.tf("cli.bundle.setup.domain", &[&args.domain]));

    let config = SetupConfig {
        tenant: args.tenant.clone(),
        team: args.team.clone(),
        env: args.env.clone(),
        offline: false,
        verbose: true,
    };
    let engine = SetupEngine::new(config);

    let setup_answers = if let Some(answers_path) = &args.answers {
        engine
            .load_answers(answers_path)
            .context(i18n.t("cli.error.failed_read_answers"))?
    } else if args.emit_answers.is_some() {
        // Empty answers for emit mode - will generate template
        serde_json::Map::new()
    } else if args.non_interactive {
        bail!("{}", i18n.t("cli.error.answers_required"));
    } else {
        // Interactive mode - run wizard for discovered packs
        println!("\n{}", i18n.t("cli.simple.interactive_mode"));
        println!();
        run_interactive_wizard(&bundle_dir)?
    };

    let providers = args
        .provider_id
        .clone()
        .map_or_else(Vec::new, |id| vec![id]);

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        providers,
        tenants: vec![TenantSelection {
            tenant: args.tenant,
            team: args.team,
            allow_paths: Vec::new(),
        }],
        setup_answers,
        domain_filter: if args.domain == "all" {
            None
        } else {
            Some(args.domain.clone())
        },
        parallel: args.parallel,
        backup: args.backup,
        skip_secrets_init: args.skip_secrets_init,
        best_effort: args.best_effort,
        ..Default::default()
    };

    let plan = engine
        .plan(
            SetupMode::Update,
            &request,
            args.dry_run || args.emit_answers.is_some(),
        )
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    // Emit answers template if requested
    if let Some(emit_path) = &args.emit_answers {
        let emit_path_str = emit_path.display().to_string();
        engine
            .emit_answers(&plan, emit_path)
            .context(i18n.t("cli.error.failed_emit_answers"))?;
        println!(
            "\n{}",
            i18n.tf("cli.bundle.setup.emit_written", &[&emit_path_str])
        );
        println!(
            "{}",
            i18n.tf("cli.bundle.update.emit_usage", &[&emit_path_str])
        );
        return Ok(());
    }

    if args.dry_run {
        println!(
            "\n{}",
            i18n.tf("cli.bundle.update.dry_run", &[&provider_display])
        );
        return Ok(());
    }

    engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;

    println!(
        "\n{}",
        i18n.tf("cli.bundle.update.complete", &[&provider_display])
    );

    Ok(())
}

/// Remove a provider from a bundle.
pub fn remove(args: BundleRemoveArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    println!("{}", i18n.t("cli.bundle.remove.removing"));
    println!(
        "{}",
        i18n.tf("cli.bundle.setup.provider", &[&args.provider_id])
    );
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.bundle",
            &[&bundle_dir.display().to_string()]
        )
    );

    if !args.force {
        println!("\n{}", i18n.t("cli.bundle.remove.confirm"));
        println!("{}", i18n.t("cli.bundle.remove.use_force"));
        bail!("{}", i18n.t("cli.bundle.remove.cancelled"));
    }

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        providers_remove: vec![args.provider_id.clone()],
        tenants: vec![TenantSelection {
            tenant: args.tenant.clone(),
            team: args.team.clone(),
            allow_paths: Vec::new(),
        }],
        ..Default::default()
    };

    let config = SetupConfig {
        tenant: args.tenant,
        team: args.team,
        env: "dev".to_string(),
        offline: false,
        verbose: true,
    };
    let engine = SetupEngine::new(config);
    let plan = engine
        .plan(SetupMode::Remove, &request, false)
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);
    engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;

    println!(
        "\n{}",
        i18n.tf("cli.bundle.remove.complete", &[&args.provider_id])
    );

    Ok(())
}

/// Build a portable bundle.
pub fn build(args: BundleBuildArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let out_str = args.out.to_string_lossy();
    let is_archive = out_str.ends_with(".gtbundle");

    println!("{}", i18n.t("cli.bundle.build.building"));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.bundle",
            &[&bundle_dir.display().to_string()]
        )
    );
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.build.output",
            &[&args.out.display().to_string()]
        )
    );
    println!(
        "  Format: {}",
        if is_archive {
            "archive (.gtbundle)"
        } else {
            "directory"
        }
    );

    if let Some(ref tenant) = args.tenant {
        println!("{}", i18n.tf("cli.bundle.add.tenant", &[tenant]));
    }

    if args.doctor && !args.skip_doctor {
        println!("\n{}", i18n.t("cli.bundle.build.running_doctor"));
        // TODO: Integrate with mcp doctor
    }

    if is_archive {
        // Create .gtbundle archive
        gtbundle::create_gtbundle(&bundle_dir, &args.out)
            .context("failed to create .gtbundle archive")?;
    } else {
        // Copy to directory
        std::fs::create_dir_all(&args.out).context("failed to create output directory")?;
        copy_dir_recursive(&bundle_dir, &args.out, args.only_used_providers)
            .context("failed to copy bundle")?;
    }

    println!(
        "\n{}",
        i18n.tf(
            "cli.bundle.build.success",
            &[&args.out.display().to_string()]
        )
    );

    Ok(())
}

/// List packs or flows in a bundle.
pub fn list(args: BundleListArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let mut packs = Vec::new();
    let providers_dir = bundle_dir.join("providers");
    let packs_dir = bundle_dir.join("packs");

    // Check providers/<domain>/ directory
    let domain_dir = providers_dir.join(&args.domain);
    if domain_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&domain_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "gtpack")
                && let Some(name) = path.file_stem().and_then(|n| n.to_str())
            {
                packs.push((name.to_string(), args.domain.clone()));
            }
        }
    }

    // Check packs/ directory
    if packs_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&packs_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "gtpack")
                && let Some(name) = path.file_stem().and_then(|n| n.to_str())
            {
                packs.push((name.to_string(), "pack".to_string()));
            }
        }
    }

    // Filter by specific pack if requested
    if let Some(ref pack_filter) = args.pack {
        packs.retain(|(name, _)| name.contains(pack_filter));
    }

    if args.format == "json" {
        let output = serde_json::json!({
            "bundle": bundle_dir.display().to_string(),
            "domain": args.domain,
            "pack_count": packs.len(),
            "packs": packs.iter().map(|(name, domain)| {
                serde_json::json!({
                    "name": name,
                    "domain": domain,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "{}",
            i18n.tf(
                "cli.bundle.list.bundle",
                &[&bundle_dir.display().to_string()]
            )
        );
        println!("{}", i18n.tf("cli.bundle.list.domain", &[&args.domain]));
        println!(
            "{}",
            i18n.tf("cli.bundle.list.packs_found", &[&packs.len().to_string()])
        );

        for (name, domain) in &packs {
            println!("  - {} ({})", name, domain);
        }
    }

    Ok(())
}

/// Show bundle status.
pub fn status(args: BundleStatusArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    if !bundle_dir.exists() {
        if args.format == "json" {
            println!(r#"{{"exists": false, "path": "{}"}}"#, bundle_dir.display());
        } else {
            println!(
                "{}",
                i18n.tf(
                    "cli.bundle.status.not_found",
                    &[&bundle_dir.display().to_string()]
                )
            );
        }
        return Ok(());
    }

    let is_valid = bundle_dir.join("greentic.demo.yaml").exists();

    let providers_dir = bundle_dir.join("providers");
    let packs_dir = bundle_dir.join("packs");
    let mut pack_count = 0;
    let mut packs = Vec::new();

    // Check providers/<domain>/ directories
    if providers_dir.exists() {
        for domain in &[
            "messaging",
            "events",
            "oauth",
            "secrets",
            "mcp",
            "state",
            "other",
        ] {
            let domain_dir = providers_dir.join(domain);
            if domain_dir.exists()
                && let Ok(entries) = std::fs::read_dir(&domain_dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "gtpack") {
                        pack_count += 1;
                        if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                            packs.push(format!("providers/{}/{}", domain, name));
                        }
                    }
                }
            }
        }
    }

    // Check packs/ directory
    if packs_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&packs_dir)
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "gtpack") {
                pack_count += 1;
                if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                    packs.push(format!("packs/{}", name));
                }
            }
        }
    }

    // Count tenants
    let tenants_dir = bundle_dir.join("tenants");
    let mut tenant_count = 0;
    let mut tenants = Vec::new();

    if tenants_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&tenants_dir)
    {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                tenant_count += 1;
                if let Some(name) = entry.file_name().to_str() {
                    tenants.push(name.to_string());
                }
            }
        }
    }

    if args.format == "json" {
        let status = serde_json::json!({
            "exists": true,
            "valid": is_valid,
            "path": bundle_dir.display().to_string(),
            "pack_count": pack_count,
            "packs": packs,
            "tenant_count": tenant_count,
            "tenants": tenants,
        });
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!(
            "{}",
            i18n.tf(
                "cli.bundle.status.bundle_label",
                &[&bundle_dir.display().to_string()]
            )
        );
        let valid_status = if is_valid {
            i18n.t("cli.bundle.status.valid_yes")
        } else {
            i18n.t("cli.bundle.status.valid_no")
        };
        println!("Valid: {}", valid_status);
        println!(
            "{}",
            i18n.tf("cli.bundle.status.packs", &[&pack_count.to_string()])
        );
        for pack in &packs {
            println!("  - {}", pack);
        }
        println!(
            "{}",
            i18n.tf("cli.bundle.status.tenants", &[&tenant_count.to_string()])
        );
        for tenant in &tenants {
            println!("  - {}", tenant);
        }
    }

    Ok(())
}

/// Copy directory recursively.
fn copy_dir_recursive(src: &Path, dst: &Path, _only_used: bool) -> Result<()> {
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

/// Run interactive wizard for all discovered packs in the bundle.
pub fn run_interactive_wizard(
    bundle_path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    use serde_json::Value;

    let mut all_answers = serde_json::Map::new();

    // Discover packs in the bundle
    let discovered = discovery::discover(bundle_path)?;

    if discovered.providers.is_empty() {
        println!("No providers found in bundle. Nothing to configure.");
        return Ok(all_answers);
    }

    println!(
        "Found {} provider(s) to configure:",
        discovered.providers.len()
    );
    for provider in &discovered.providers {
        println!("  - {} ({})", provider.provider_id, provider.domain);
    }
    println!();

    // Run wizard for each provider
    for provider in &discovered.providers {
        let provider_id = &provider.provider_id;

        // Try to build FormSpec from setup.yaml or pack manifest
        let form_spec = setup_to_formspec::pack_to_form_spec(&provider.pack_path, provider_id);

        if let Some(spec) = form_spec {
            if spec.questions.is_empty() {
                println!("Provider {}: No configuration required.", provider_id);
                all_answers.insert(provider_id.clone(), Value::Object(serde_json::Map::new()));
                continue;
            }

            // Run interactive prompts for this provider
            let answers = wizard::prompt_form_spec_answers(&spec, provider_id)?;
            all_answers.insert(provider_id.clone(), answers);
        } else {
            // No FormSpec available - provider uses flow-based setup or has no questions
            println!(
                "Provider {}: No setup questions found (may use flow-based setup).",
                provider_id
            );
            all_answers.insert(provider_id.clone(), Value::Object(serde_json::Map::new()));
        }

        println!();
    }

    Ok(all_answers)
}
