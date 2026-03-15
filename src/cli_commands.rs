//! CLI command implementations for greentic-setup.

use anyhow::{Context, Result, bail};

use crate::cli_args::*;
use crate::cli_helpers::{copy_dir_recursive, resolve_bundle_dir, run_interactive_wizard};
use crate::cli_i18n::CliI18n;
use crate::engine::{LoadedAnswers, SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::platform_setup::StaticRoutesPolicy;
use crate::{SetupEngine, SetupMode, bundle};

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

/// Shared implementation for setup and update commands.
fn setup_or_update(args: BundleSetupArgs, mode: SetupMode, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let provider_display = args
        .provider_id
        .clone()
        .unwrap_or_else(|| "all".to_string());

    let header_key = match mode {
        SetupMode::Update => "cli.bundle.update.updating",
        _ => "cli.bundle.setup.setting_up",
    };
    println!("{}", i18n.t(header_key));
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

    let loaded_answers = if let Some(answers_path) = &args.answers {
        engine
            .load_answers(answers_path)
            .context(i18n.t("cli.error.failed_read_answers"))?
    } else if args.emit_answers.is_some() {
        LoadedAnswers::default()
    } else if args.non_interactive {
        bail!("{}", i18n.t("cli.error.answers_required"));
    } else {
        println!("\n{}", i18n.t("cli.simple.interactive_mode"));
        println!();
        run_interactive_wizard(&bundle_dir, &args.env, args.advanced)?
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
        static_routes: StaticRoutesPolicy::normalize(
            loaded_answers.platform_setup.static_routes.as_ref(),
            &args.env,
        )
        .context(i18n.t("cli.error.failed_read_answers"))?,
        setup_answers: loaded_answers.setup_answers,
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
        .plan(mode, &request, args.dry_run || args.emit_answers.is_some())
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    if let Some(emit_path) = &args.emit_answers {
        let emit_path_str = emit_path.display().to_string();
        engine
            .emit_answers(&plan, emit_path)
            .context(i18n.t("cli.error.failed_emit_answers"))?;
        println!(
            "\n{}",
            i18n.tf("cli.bundle.setup.emit_written", &[&emit_path_str])
        );
        let usage_key = match mode {
            SetupMode::Update => "cli.bundle.update.emit_usage",
            _ => "cli.bundle.setup.emit_usage",
        };
        println!("{}", i18n.tf(usage_key, &[&emit_path_str]));
        return Ok(());
    }

    if args.dry_run {
        let dry_key = match mode {
            SetupMode::Update => "cli.bundle.update.dry_run",
            _ => "cli.bundle.setup.dry_run",
        };
        println!("\n{}", i18n.tf(dry_key, &[&provider_display]));
        return Ok(());
    }

    engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;

    let done_key = match mode {
        SetupMode::Update => "cli.bundle.update.complete",
        _ => "cli.bundle.setup.complete",
    };
    println!("\n{}", i18n.tf(done_key, &[&provider_display]));

    Ok(())
}

pub fn setup(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    setup_or_update(args, SetupMode::Create, i18n)
}

pub fn update(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    setup_or_update(args, SetupMode::Update, i18n)
}

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

pub fn build(args: BundleBuildArgs, i18n: &CliI18n) -> Result<()> {
    use crate::gtbundle;

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
    }

    if is_archive {
        gtbundle::create_gtbundle(&bundle_dir, &args.out)
            .context("failed to create .gtbundle archive")?;
    } else {
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

pub fn list(args: BundleListArgs, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let mut packs = Vec::new();
    let providers_dir = bundle_dir.join("providers");
    let packs_dir = bundle_dir.join("packs");

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

    let is_valid = bundle::is_bundle_root(&bundle_dir);

    let providers_dir = bundle_dir.join("providers");
    let packs_dir = bundle_dir.join("packs");
    let mut pack_count = 0;
    let mut packs = Vec::new();

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
