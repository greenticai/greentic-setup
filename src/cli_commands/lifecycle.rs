//! Bundle lifecycle commands: init, add, remove.

use anyhow::{Context, Result, bail};

use crate::cli_args::*;
use crate::cli_helpers::resolve_bundle_dir;
use crate::cli_i18n::CliI18n;
use crate::engine::{SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::{SetupEngine, SetupMode, bundle};

/// Initialize a new bundle directory.
pub fn init(args: BundleInitArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = args.path.unwrap_or_else(|| std::path::PathBuf::from("."));
    let bundle_path = bundle_dir.display().to_string();

    if bundle::is_bundle_root(&bundle_dir) {
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

    if !bundle::is_bundle_root(&bundle_dir) {
        bundle::create_demo_bundle_structure(&bundle_dir, None)
            .context(i18n.t("cli.error.failed_create_bundle"))?;
        println!(
            "{}",
            i18n.tf("cli.bundle.add.created_structure", &[&bundle_path])
        );
    }

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
