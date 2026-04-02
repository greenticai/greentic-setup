//! Setup and update commands for bundle configuration.

use anyhow::{Context, Result, bail};

use crate::cli_args::*;
use crate::cli_helpers::{
    complete_loaded_answers_with_prompts, ensure_deployment_targets_present, resolve_bundle_dir,
    resolve_setup_scope, run_interactive_wizard,
};
use crate::cli_i18n::CliI18n;
use crate::engine::{LoadedAnswers, SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::platform_setup::StaticRoutesPolicy;
use crate::{SetupEngine, SetupMode, bundle};

/// Run the setup command.
pub fn setup(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    setup_or_update(args, SetupMode::Create, i18n)
}

/// Run the update command.
pub fn update(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    setup_or_update(args, SetupMode::Update, i18n)
}

/// Shared implementation for setup and update commands.
fn setup_or_update(args: BundleSetupArgs, mode: SetupMode, i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;
    let BundleSetupArgs {
        provider_id,
        bundle: _,
        tenant: cli_tenant,
        team: cli_team,
        env: cli_env,
        domain,
        dry_run,
        emit_answers,
        answers,
        key,
        non_interactive,
        advanced,
        parallel,
        backup,
        skip_secrets_init,
        best_effort,
    } = args;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    let provider_display = provider_id.clone().unwrap_or_else(|| "all".to_string());

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
    let loader_engine = SetupEngine::new(SetupConfig {
        tenant: cli_tenant.clone(),
        team: cli_team.clone(),
        env: cli_env.clone(),
        offline: false,
        verbose: true,
    });

    let loaded_answers = if let Some(answers_path) = &answers {
        loader_engine
            .load_answers(answers_path, key.as_deref(), !non_interactive)
            .context(i18n.t("cli.error.failed_read_answers"))?
    } else if emit_answers.is_some() {
        LoadedAnswers::default()
    } else if non_interactive {
        bail!("{}", i18n.t("cli.error.answers_required"));
    } else {
        println!("\n{}", i18n.t("cli.simple.interactive_mode"));
        println!();
        run_interactive_wizard(
            &bundle_dir,
            &cli_tenant,
            cli_team.as_deref(),
            &cli_env,
            advanced,
        )?
    };
    let (tenant, team, env) = if answers.is_some() {
        resolve_setup_scope(cli_tenant, cli_team, cli_env, &loaded_answers)
    } else {
        (cli_tenant, cli_team, cli_env)
    };

    println!("{}", i18n.tf("cli.bundle.add.tenant", &[&tenant]));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.team",
            &[team.as_deref().unwrap_or("default")]
        )
    );
    println!("{}", i18n.tf("cli.bundle.add.env", &[&env]));
    println!("{}", i18n.tf("cli.bundle.setup.domain", &[&domain]));

    let loaded_answers = if answers.is_some() && !non_interactive {
        complete_loaded_answers_with_prompts(
            &bundle_dir,
            &tenant,
            team.as_deref(),
            &env,
            advanced,
            loaded_answers,
        )?
    } else {
        loaded_answers
    };
    if non_interactive {
        ensure_deployment_targets_present(&bundle_dir, &loaded_answers)?;
    }

    let providers = provider_id.clone().map_or_else(Vec::new, |id| vec![id]);

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        providers,
        tenants: vec![TenantSelection {
            tenant: tenant.clone(),
            team: team.clone(),
            allow_paths: Vec::new(),
        }],
        static_routes: StaticRoutesPolicy::normalize(
            loaded_answers.platform_setup.static_routes.as_ref(),
            &env,
        )
        .context(i18n.t("cli.error.failed_read_answers"))?,
        deployment_targets: loaded_answers.platform_setup.deployment_targets,
        setup_answers: loaded_answers.setup_answers,
        domain_filter: if domain == "all" {
            None
        } else {
            Some(domain.clone())
        },
        parallel,
        backup,
        skip_secrets_init,
        best_effort,
        ..Default::default()
    };

    let engine = SetupEngine::new(SetupConfig {
        tenant,
        team,
        env,
        offline: false,
        verbose: true,
    });

    let plan = engine
        .plan(mode, &request, dry_run || emit_answers.is_some())
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    if let Some(emit_path) = &emit_answers {
        let emit_path_str = emit_path.display().to_string();
        engine
            .emit_answers(&plan, emit_path, key.as_deref(), !non_interactive)
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

    if dry_run {
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
