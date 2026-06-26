//! Setup and update commands for bundle configuration.

use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::bootstrap::{LocalEnvOutcome, ensure_local_environment};
use greentic_deployer::environment::LocalFsStore;

use crate::cli_args::*;
use crate::cli_helpers::{
    complete_loaded_answers_with_prompts, ensure_deployment_targets_present,
    ensure_required_setup_answers_present, maybe_start_cli_setup_tunnel, resolve_bundle_dir,
    resolve_setup_scope, run_interactive_wizard,
};
use crate::cli_i18n::CliI18n;
use crate::engine::{LoadedAnswers, SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::platform_setup::StaticRoutesPolicy;
use crate::{SetupEngine, SetupMode, bundle, resolve_env};

/// Run the setup command.
pub fn setup(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    setup_or_update(args, SetupMode::Create, i18n)
}

/// Run the update command.
pub fn update(args: BundleSetupArgs, i18n: &CliI18n) -> Result<()> {
    setup_or_update(args, SetupMode::Update, i18n)
}

/// Show persisted setup status for a provider backend contract.
pub fn setup_status(args: BundleSetupStatusArgs, _i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;
    bundle::validate_bundle_exists(&bundle_dir)?;
    let discovered = crate::discovery::discover(&bundle_dir)?;
    let provider = discovered
        .find_setup_target(&args.provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider not found in bundle: {}", args.provider_id))?;
    if let Some(machine) = crate::setup_machine::load_setup_machine_from_pack(&provider.pack_path)?
    {
        let team = args.team.as_deref().unwrap_or("default");
        let state = crate::setup_machine::load_or_init_setup_machine_state(
            &bundle_dir,
            &provider.provider_id,
            &args.tenant,
            team,
            &machine,
        )?;
        let status = crate::setup_machine::render_setup_machine_status(&machine, &state);
        if args.format == "json" {
            println!("{}", serde_json::to_string_pretty(&status)?);
        } else {
            println!("provider: {}", provider.provider_id);
            println!("tenant: {}", args.tenant);
            println!("team: {}", team);
            println!(
                "status: {}",
                status
                    .get("status")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            );
            println!(
                "next: {}",
                status
                    .get("current_step")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("complete")
            );
        }
        return Ok(());
    }
    let contract = crate::setup_machine::load_setup_backend_contract_from_pack(
        &provider.pack_path,
        Some(&provider.provider_id),
    )?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "provider {} does not declare greentic.setup.backend-contract.v1",
            provider.provider_id
        )
    })?;
    let team = args.team.as_deref().unwrap_or("default");
    let stored = crate::setup_backend_contract::load_backend_state(
        &bundle_dir,
        &args.env,
        &args.tenant,
        team,
        &provider.provider_id,
    )?;
    let status = crate::setup_backend_contract::render_status(&contract, &stored);

    if args.format == "json" {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("provider: {}", provider.provider_id);
        println!("tenant: {}", args.tenant);
        println!("team: {}", team);
        println!(
            "status: {}",
            if status.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
                "complete"
            } else {
                "incomplete"
            }
        );
        println!(
            "next: {}",
            status
                .get("next")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
        );
        if let Some(items) = status.get("items").and_then(serde_json::Value::as_array) {
            for item in items {
                println!(
                    "  - {} [{}]",
                    item.get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown"),
                    item.get("state")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                );
            }
        }
        if let Some(blocked) = status
            .get("blocked")
            .and_then(serde_json::Value::as_object)
            .filter(|blocked| !blocked.is_empty())
        {
            println!(
                "blocked: {}",
                blocked
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Setup step blocked")
            );
            if blocked
                .get("retryable")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                println!("retryable: true");
            }
        }
    }

    Ok(())
}

/// Inspect and record the next provider backend-contract setup step.
pub fn setup_next(args: BundleSetupNextArgs, _i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;
    bundle::validate_bundle_exists(&bundle_dir)?;
    let discovered = crate::discovery::discover(&bundle_dir)?;
    let provider = discovered
        .find_setup_target(&args.provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider not found in bundle: {}", args.provider_id))?;
    if let Some(machine) = crate::setup_machine::load_setup_machine_from_pack(&provider.pack_path)?
    {
        let team = args.team.as_deref().unwrap_or("default");
        let output = crate::setup_machine::advance_setup_machine_with_pack(
            &bundle_dir,
            Some(&provider.pack_path),
            &provider.provider_id,
            &args.tenant,
            team,
            &machine,
            args.dry_run,
        )?;
        print_setup_next_output(&output, &args.format)?;
        return Ok(());
    }
    let contract = crate::setup_machine::load_setup_backend_contract_from_pack(
        &provider.pack_path,
        Some(&provider.provider_id),
    )?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "provider {} does not declare greentic.setup.backend-contract.v1",
            provider.provider_id
        )
    })?;
    let team = args.team.as_deref().unwrap_or("default");
    let mut stored = crate::setup_backend_contract::load_backend_state(
        &bundle_dir,
        &args.env,
        &args.tenant,
        team,
        &provider.provider_id,
    )?;
    let status = crate::setup_backend_contract::render_status(&contract, &stored);
    let Some(next_step) = crate::setup_backend_contract::next_action_id(&contract, &stored) else {
        let output = serde_json::json!({
            "provider_id": provider.provider_id,
            "tenant": args.tenant,
            "team": team,
            "ok": true,
            "step": "complete",
            "next": "Setup complete.",
            "status": status,
        });
        print_setup_next_output(&output, &args.format)?;
        return Ok(());
    };
    let action = crate::setup_backend_contract::action_by_id(&contract, &next_step)
        .ok_or_else(|| anyhow::anyhow!("setup step not found in contract: {next_step}"))?;
    let executor_kind =
        crate::setup_backend_contract::executor_kind(action).unwrap_or("unsupported");
    let result = match executor_kind {
        "oauth_device_code"
            if crate::setup_backend_contract::oauth_device_login_started(&stored, action) =>
        {
            crate::setup_backend_contract::execute_oauth_device_code_complete(&mut stored, action)?
        }
        "oauth_device_code" => {
            crate::setup_backend_contract::execute_oauth_device_code_start(&mut stored, action)?
        }
        "provider_http"
            if action
                .get("executor")
                .and_then(|executor| {
                    executor
                        .get("path_template")
                        .or_else(|| executor.get("target_path_template"))
                })
                .is_some() =>
        {
            crate::setup_backend_contract::execute_provider_http_local_route(
                &bundle_dir,
                &mut stored,
                &provider.provider_id,
                &args.tenant,
                team,
                &args.env,
                action,
            )?
        }
        "provider_http" => crate::setup_backend_contract::execute_provider_http_external(
            &mut stored,
            &provider.provider_id,
            &args.tenant,
            team,
            &args.env,
            action,
        )?,
        "runtime_observation" => {
            crate::setup_backend_contract::execute_runtime_observation(&mut stored, action)?
        }
        "microsoft_graph_application" => {
            crate::setup_backend_contract::execute_microsoft_graph_application(
                &mut stored,
                &provider.provider_id,
                action,
            )?
        }
        "microsoft_graph_teams_app_catalog_publish" => {
            crate::setup_backend_contract::execute_microsoft_graph_teams_app_catalog_publish(
                &provider.pack_path,
                &mut stored,
                &args.tenant,
                team,
                &args.env,
                action,
            )?
        }
        "microsoft_graph_teams_app_user_install" => {
            crate::setup_backend_contract::execute_microsoft_graph_teams_app_user_install(
                &mut stored,
                &args.tenant,
                team,
                &args.env,
                action,
            )?
        }
        _ => crate::setup_backend_contract::step_result(
            action,
            false,
            "Use a greentic-setup build or provider-specific setup runner that supports this executor.",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "executor_not_available_in_cli",
                "executor_kind": executor_kind,
                "detail": "generic CLI setup-next records resumable state and diagnostics; this executor is not supported by this CLI build",
            }),
        ),
    };

    let event_path = if args.dry_run {
        None
    } else {
        Some(crate::setup_backend_contract::record_action_result(
            &bundle_dir,
            &args.tenant,
            team,
            &provider.provider_id,
            &mut stored,
            result.clone(),
        )?)
    };
    let output = serde_json::json!({
        "provider_id": provider.provider_id,
        "tenant": args.tenant,
        "team": team,
        "dry_run": args.dry_run,
        "step": next_step,
        "executor_kind": executor_kind,
        "result": result,
        "event_path": event_path,
        "status": status,
    });
    print_setup_next_output(&output, &args.format)?;
    Ok(())
}

/// Clear retry-blocking state for a provider backend-contract setup step.
pub fn setup_retry(args: BundleSetupRetryArgs, _i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;
    bundle::validate_bundle_exists(&bundle_dir)?;
    let discovered = crate::discovery::discover(&bundle_dir)?;
    let provider = discovered
        .find_setup_target(&args.provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider not found in bundle: {}", args.provider_id))?;
    if let Some(machine) = crate::setup_machine::load_setup_machine_from_pack(&provider.pack_path)?
    {
        let team = args.team.as_deref().unwrap_or("default");
        let output = crate::setup_machine::retry_setup_machine_step(
            &bundle_dir,
            &provider.provider_id,
            &args.tenant,
            team,
            &machine,
            args.step.as_deref(),
        )?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("provider: {}", output["provider_id"].as_str().unwrap_or(""));
            println!("tenant: {}", output["tenant"].as_str().unwrap_or(""));
            println!("team: {}", output["team"].as_str().unwrap_or(""));
            println!("retry: {}", output["step"].as_str().unwrap_or(""));
            println!("changed: {}", output["changed"].as_bool().unwrap_or(false));
            if let Some(path) = output.get("event_path").and_then(serde_json::Value::as_str) {
                println!("event: {path}");
            }
        }
        return Ok(());
    }
    let contract = crate::setup_machine::load_setup_backend_contract_from_pack(
        &provider.pack_path,
        Some(&provider.provider_id),
    )?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "provider {} does not declare greentic.setup.backend-contract.v1",
            provider.provider_id
        )
    })?;
    let team = args.team.as_deref().unwrap_or("default");
    let mut stored = crate::setup_backend_contract::load_backend_state(
        &bundle_dir,
        &args.env,
        &args.tenant,
        team,
        &provider.provider_id,
    )?;
    let selected_step = args
        .step
        .as_deref()
        .or_else(|| {
            stored
                .get("last_setup_result")
                .and_then(|result| result.get("step"))
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("no setup step to retry; pass --step <step-id>"))?;
    let action = crate::setup_backend_contract::action_by_id(&contract, &selected_step)
        .ok_or_else(|| anyhow::anyhow!("setup step not found in contract: {selected_step}"))?;

    let mut changed = false;
    if stored
        .get("last_setup_result")
        .and_then(|result| result.get("step"))
        .and_then(serde_json::Value::as_str)
        == Some(selected_step.as_str())
    {
        stored.remove("last_setup_result");
        changed = true;
    }
    if stored
        .get("oauth_resume")
        .and_then(|resume| resume.get("resume_step"))
        .and_then(serde_json::Value::as_str)
        == Some(selected_step.as_str())
    {
        stored.remove("oauth_resume");
        changed = true;
    }
    crate::setup_backend_contract::save_backend_state(
        &bundle_dir,
        &args.tenant,
        team,
        &provider.provider_id,
        &stored,
    )?;
    let event_path = crate::setup_backend_contract::append_backend_event(
        &bundle_dir,
        &args.tenant,
        team,
        &provider.provider_id,
        serde_json::json!({
            "type": "step_retry_requested",
            "step": selected_step,
            "executor_kind": crate::setup_backend_contract::executor_kind(action),
            "changed": changed,
        }),
    )?;
    let status = crate::setup_backend_contract::render_status(&contract, &stored);
    let output = serde_json::json!({
        "provider_id": provider.provider_id,
        "tenant": args.tenant,
        "team": team,
        "step": selected_step,
        "changed": changed,
        "event_path": event_path,
        "status": status,
    });
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("provider: {}", output["provider_id"].as_str().unwrap_or(""));
        println!("tenant: {}", output["tenant"].as_str().unwrap_or(""));
        println!("team: {}", output["team"].as_str().unwrap_or(""));
        println!("retry: {}", output["step"].as_str().unwrap_or(""));
        println!("changed: {}", output["changed"].as_bool().unwrap_or(false));
        println!("event: {}", event_path.display());
    }
    Ok(())
}

/// Reset persisted setup state for a provider backend contract.
pub fn setup_reset(args: BundleSetupResetArgs, _i18n: &CliI18n) -> Result<()> {
    if !args.yes {
        bail!("refusing to reset setup state without --yes");
    }
    let bundle_dir = resolve_bundle_dir(args.bundle)?;
    bundle::validate_bundle_exists(&bundle_dir)?;
    let team = args.team.as_deref().unwrap_or("default");
    if let Ok(discovered) = crate::discovery::discover(&bundle_dir)
        && let Some(provider) = discovered.find_setup_target(&args.provider_id)
        && let Some(_machine) =
            crate::setup_machine::load_setup_machine_from_pack(&provider.pack_path)?
    {
        let archive_path = crate::setup_machine::reset_setup_machine_state(
            &bundle_dir,
            &args.tenant,
            team,
            &provider.provider_id,
            "manual-reset",
        )?;
        let output = serde_json::json!({
            "provider_id": provider.provider_id,
            "tenant": args.tenant,
            "team": team,
            "reset": true,
            "archive_path": archive_path,
            "setup_model": "setup_machine",
        });
        if args.json {
            println!("{}", serde_json::to_string_pretty(&output)?);
        } else {
            println!("provider: {}", output["provider_id"].as_str().unwrap_or(""));
            println!("tenant: {}", output["tenant"].as_str().unwrap_or(""));
            println!("team: {}", output["team"].as_str().unwrap_or(""));
            match output
                .get("archive_path")
                .and_then(serde_json::Value::as_str)
            {
                Some(path) => println!("archive: {path}"),
                None => println!("archive: none"),
            }
        }
        return Ok(());
    }
    let archive_path = crate::setup_backend_contract::reset_backend_state(
        &bundle_dir,
        &args.tenant,
        team,
        &args.provider_id,
        "manual-reset",
    )?;
    let output = serde_json::json!({
        "provider_id": args.provider_id,
        "tenant": args.tenant,
        "team": team,
        "reset": true,
        "archive_path": archive_path,
    });
    if args.json {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("provider: {}", output["provider_id"].as_str().unwrap_or(""));
        println!("tenant: {}", output["tenant"].as_str().unwrap_or(""));
        println!("team: {}", output["team"].as_str().unwrap_or(""));
        match output
            .get("archive_path")
            .and_then(serde_json::Value::as_str)
        {
            Some(path) => println!("archive: {path}"),
            None => println!("archive: none"),
        }
    }
    Ok(())
}

fn print_setup_next_output(output: &serde_json::Value, format: &str) -> Result<()> {
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(output)?);
        return Ok(());
    }
    println!(
        "provider: {}",
        output
            .get("provider_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    );
    println!(
        "tenant: {}",
        output
            .get("tenant")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    );
    println!(
        "team: {}",
        output
            .get("team")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    );
    println!(
        "next: {}",
        output
            .get("step")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
    );
    if let Some(kind) = output
        .get("executor_kind")
        .and_then(serde_json::Value::as_str)
    {
        println!("executor: {kind}");
    }
    if let Some(next) = output
        .get("result")
        .and_then(|result| result.get("next"))
        .and_then(serde_json::Value::as_str)
    {
        println!("action: {next}");
    }
    if let Some(path) = output.get("event_path").and_then(serde_json::Value::as_str) {
        println!("event: {path}");
    }
    Ok(())
}

/// Migrate legacy setup backend state to the generic setup state location.
pub fn setup_migrate(args: BundleSetupMigrateArgs, _i18n: &CliI18n) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;
    bundle::validate_bundle_exists(&bundle_dir)?;
    let discovered = crate::discovery::discover(&bundle_dir)?;
    let provider = discovered
        .find_setup_target(&args.provider_id)
        .ok_or_else(|| anyhow::anyhow!("provider not found in bundle: {}", args.provider_id))?;
    let team = args.team.as_deref().unwrap_or("default");
    if let Some(machine) = crate::setup_machine::load_setup_machine_from_pack(&provider.pack_path)?
    {
        let migration = crate::setup_machine::migrate_setup_machine_state(
            &bundle_dir,
            &args.env,
            &args.tenant,
            team,
            &provider.provider_id,
            &machine,
        )?;
        let result = serde_json::json!({
            "provider_id": provider.provider_id,
            "tenant": args.tenant,
            "team": team,
            "machine_id": machine.id,
            "legacy_backend_path": migration.legacy_backend_path.display().to_string(),
            "legacy_setup_actions_path": migration.legacy_setup_actions_path.display().to_string(),
            "state_path": migration.state_path.display().to_string(),
            "backend_archive_path": migration.backend_archive_path.as_ref().map(|path| path.display().to_string()),
            "setup_actions_archive_path": migration.setup_actions_archive_path.as_ref().map(|path| path.display().to_string()),
            "event_path": migration.event_path.display().to_string(),
            "source": migration.source.clone(),
            "legacy_backend_removed": migration.legacy_backend_removed,
            "setup_actions_removed": migration.setup_actions_removed,
            "initialized": migration.initialized,
            "migrated": true,
            "empty": migration.empty,
        });

        if args.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            println!("provider: {}", result["provider_id"].as_str().unwrap_or(""));
            println!("tenant: {}", result["tenant"].as_str().unwrap_or(""));
            println!("team: {}", result["team"].as_str().unwrap_or(""));
            println!("machine: {}", result["machine_id"].as_str().unwrap_or(""));
            println!("state: {}", migration.state_path.display());
            println!("event: {}", migration.event_path.display());
            println!("source: {}", migration.source);
            if let Some(path) = result
                .get("backend_archive_path")
                .and_then(serde_json::Value::as_str)
            {
                println!("legacy backend archive: {path}");
            }
            if migration.legacy_backend_removed {
                println!("legacy backend: removed");
            }
            if let Some(path) = result
                .get("setup_actions_archive_path")
                .and_then(serde_json::Value::as_str)
            {
                println!("setup-actions archive: {path}");
            }
            if migration.setup_actions_removed {
                println!("setup-actions: removed");
            }
            if migration.empty {
                println!("note: no existing setup state was found; initialized machine state");
            }
        }
        return Ok(());
    }
    let migration = crate::setup_backend_contract::migrate_backend_state(
        &bundle_dir,
        &args.env,
        &args.tenant,
        team,
        &args.provider_id,
    )?;

    let result = serde_json::json!({
        "provider_id": args.provider_id,
        "tenant": args.tenant,
        "team": team,
        "legacy_path": migration.legacy_path.display().to_string(),
        "legacy_setup_actions_path": migration.legacy_setup_actions_path.display().to_string(),
        "state_path": migration.state_path.display().to_string(),
        "archive_path": migration.archive_path.as_ref().map(|path| path.display().to_string()),
        "setup_actions_archive_path": migration.setup_actions_archive_path.as_ref().map(|path| path.display().to_string()),
        "event_path": migration.event_path.display().to_string(),
        "source": migration.source.clone(),
        "legacy_removed": migration.legacy_removed,
        "setup_actions_removed": migration.setup_actions_removed,
        "migrated": true,
        "empty": migration.empty,
    });

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("provider: {}", result["provider_id"].as_str().unwrap_or(""));
        println!("tenant: {}", result["tenant"].as_str().unwrap_or(""));
        println!("team: {}", result["team"].as_str().unwrap_or(""));
        println!("state: {}", migration.state_path.display());
        println!("event: {}", migration.event_path.display());
        println!("source: {}", migration.source);
        if let Some(path) = result
            .get("archive_path")
            .and_then(serde_json::Value::as_str)
        {
            println!("archive: {path}");
        }
        if migration.legacy_removed {
            println!("legacy: removed");
        }
        if let Some(path) = result
            .get("setup_actions_archive_path")
            .and_then(serde_json::Value::as_str)
        {
            println!("setup-actions archive: {path}");
        }
        if migration.setup_actions_removed {
            println!("setup-actions: removed");
        }
        if migration.empty {
            println!("note: no existing setup state was found; wrote an empty state file");
        }
    }

    Ok(())
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

    // A10: thread the env_id through the wizard surface as the canonical
    // env id. resolve_env applies the A4b `dev` → `local` compat alias so
    // a user passing `--env dev` doesn't slip past as a raw legacy string.
    let cli_env = resolve_env(Some(&cli_env));

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    bootstrap_local_environment(i18n)?;

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

    let mut loaded_answers = if answers.is_some() {
        complete_loaded_answers_with_prompts(
            &bundle_dir,
            &tenant,
            team.as_deref(),
            &env,
            advanced,
            non_interactive,
            loaded_answers,
        )?
    } else {
        loaded_answers
    };
    if non_interactive {
        ensure_deployment_targets_present(&bundle_dir, &loaded_answers)?;
    }

    let is_dry_run = dry_run || emit_answers.is_some();
    let _setup_tunnel = if !is_dry_run {
        let local_base_url = "http://127.0.0.1:1";
        let tunnel = maybe_start_cli_setup_tunnel(&mut loaded_answers, local_base_url)
            .context("failed to start setup tunnel")?;
        if let Some(tunnel) = tunnel.as_ref() {
            println!("Setup tunnel public_base_url: {}", tunnel.public_base_url);
        }
        tunnel
    } else {
        None
    };
    if non_interactive {
        ensure_required_setup_answers_present(&bundle_dir, &loaded_answers)
            .context("Missing required answers in --non-interactive mode")?;
    }

    let providers = provider_id.clone().map_or_else(Vec::new, |id| vec![id]);

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        bundle_name: crate::bundle::read_bundle_name(&bundle_dir).ok().flatten(),
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
        tunnel: loaded_answers.platform_setup.tunnel,
        telemetry: loaded_answers.platform_setup.telemetry,
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
        tenant: tenant.clone(),
        team: team.clone(),
        env: env.clone(),
        offline: false,
        verbose: true,
    });

    let plan = engine
        .plan(mode, &request, is_dry_run)
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

    let report = engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;
    let _ = (report, non_interactive, env);

    let done_key = match mode {
        SetupMode::Update => "cli.bundle.update.complete",
        _ => "cli.bundle.setup.complete",
    };
    println!("\n{}", i18n.tf(done_key, &[&provider_display]));

    Ok(())
}

pub fn print_pending_setup_actions(actions: &[crate::setup_actions::SetupAction]) {
    let visible_actions: Vec<_> = actions
        .iter()
        .filter(|action| {
            matches!(
                action.kind,
                crate::setup_actions::SetupActionKind::OauthInstallButton
            ) && action.status == crate::setup_actions::SetupActionStatus::Pending
        })
        .collect();
    if visible_actions.is_empty() {
        return;
    }

    println!();
    for action in visible_actions {
        if let Some(url) = action.authorize_url.as_deref() {
            println!("{url}");
        }
        if action.callback_path.is_some() {
            println!(
                "After completing the OAuth flow, re-run setup if the callback was not handled automatically."
            );
        }
        println!();
    }
}

/// Idempotently auto-create the `local` Environment on first `gtc setup`.
///
/// Per A4 of `plans/next-gen-deployment.md`: every `gtc setup` (and update)
/// invocation guarantees a `local` Environment exists with the five default
/// capability-slot bindings (deployer/secrets/telemetry/sessions/state).
/// Subsequent calls find the env on disk and stay silent.
pub(crate) fn bootstrap_local_environment(i18n: &CliI18n) -> Result<()> {
    let root = LocalFsStore::default_root()
        .context("Cannot determine default environment store root (no home directory).")?;
    let store = LocalFsStore::new(root.clone());
    // greentic-setup never seeds a `public_base_url` at bootstrap time; the
    // operator sets it later via `gtc op env init --public-url <URL>` or
    // `gtc op env set-public-url`. Passing `None` preserves prior behavior on
    // both first-run and idempotent re-runs.
    let (_env, outcome) = ensure_local_environment(&store, None)
        .with_context(|| format!("Bootstrapping `local` environment at {}", root.display()))?;
    if outcome == LocalEnvOutcome::Created {
        println!(
            "{}",
            i18n.tf(
                "cli.bundle.setup.env_bootstrap_created",
                &[&root.display().to_string()]
            )
        );
    }
    Ok(())
}

pub fn wait_for_pending_oauth_callbacks(
    server: Option<crate::no_ui_oauth::NoUiOAuthCallbackServer>,
    actions: &[crate::setup_actions::SetupAction],
) -> Result<()> {
    let pending = crate::no_ui_oauth::pending_oauth_install_actions(actions);
    if pending.is_empty() {
        return Ok(());
    }
    let Some(server) = server else {
        return Ok(());
    };
    println!("Waiting for OAuth callback...");
    let message = server.wait_for_callback()?;
    println!("{message}");
    Ok(())
}

pub fn execute_pending_oauth_device_actions(
    bundle_dir: &std::path::Path,
    env: &str,
    actions: &[crate::setup_actions::SetupAction],
) -> Result<()> {
    for action in actions {
        if action.kind != crate::setup_actions::SetupActionKind::OauthDeviceCode
            || action.status != crate::setup_actions::SetupActionStatus::Pending
        {
            continue;
        }
        println!("Starting {}...", action.label);
        let start = crate::oauth_device::start_oauth_device_code(
            bundle_dir,
            &crate::oauth_device::OAuthDeviceStartInput {
                provider_id: action.provider_id.clone(),
                tenant: action.tenant.clone(),
                team: action.team.clone(),
                action_id: action.id.clone(),
            },
            crate::oauth_device::DEFAULT_EXTENSION_KEY,
        )?;
        println!("Open {}", start.verification_uri);
        println!("Enter code: {}", start.user_code);
        print!("Press Enter after approving, or wait while setup polls...");
        io::stdout().flush().ok();
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);

        let runtime =
            tokio::runtime::Runtime::new().context("failed to create OAuth polling runtime")?;
        let mut interval = start.interval.max(1);
        loop {
            let report = runtime.block_on(crate::oauth_device::poll_oauth_device_code(
                bundle_dir,
                env,
                &crate::oauth_device::OAuthDevicePollInput {
                    session_id: start.session_id.clone(),
                },
                crate::oauth_device::DEFAULT_EXTENSION_KEY,
            ))?;
            match report.status {
                crate::oauth_device::OAuthDevicePollStatus::Complete => {
                    println!("OAuth device-code setup complete for {}.", action.label);
                    break;
                }
                crate::oauth_device::OAuthDevicePollStatus::Pending
                | crate::oauth_device::OAuthDevicePollStatus::SlowDown => {
                    interval = report.interval.unwrap_or(interval).max(1);
                    if crate::setup_actions::current_epoch_secs() >= start.expires_at {
                        bail!("OAuth device code expired before authorization completed");
                    }
                    thread::sleep(Duration::from_secs(interval.min(30)));
                }
                crate::oauth_device::OAuthDevicePollStatus::Failed => {
                    if !report.checklist.is_empty() {
                        println!("Checklist:");
                        for item in &report.checklist {
                            println!("- {item}");
                        }
                    }
                    bail!(
                        "{}",
                        report
                            .message
                            .unwrap_or_else(|| "OAuth device-code setup failed".to_string())
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `HOME` is process-global; serialize tests that mutate it.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    fn with_home<R>(tmp: &std::path::Path, body: impl FnOnce() -> R) -> R {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        // SAFETY: serialized by HOME_LOCK; tests are single-threaded inside the
        // critical section. unsafe is required because set_var/remove_var are
        // marked unsafe in Rust 2024 edition.
        unsafe {
            std::env::set_var("HOME", tmp);
        }
        let out = body();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
        out
    }

    #[test]
    fn bootstrap_creates_local_env_under_default_root() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let i18n = CliI18n::from_request(Some("en")).expect("i18n");
        with_home(tmp.path(), || {
            bootstrap_local_environment(&i18n).expect("first bootstrap");
        });
        let env_file = tmp
            .path()
            .join(".greentic")
            .join("environments")
            .join("local")
            .join("environment.json");
        assert!(env_file.exists(), "expected env file at {env_file:?}");
    }

    #[test]
    fn bootstrap_is_idempotent_across_calls() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let i18n = CliI18n::from_request(Some("en")).expect("i18n");
        with_home(tmp.path(), || {
            bootstrap_local_environment(&i18n).expect("first bootstrap");
            bootstrap_local_environment(&i18n).expect("second bootstrap");
        });
        let env_file = tmp
            .path()
            .join(".greentic")
            .join("environments")
            .join("local")
            .join("environment.json");
        assert!(env_file.exists());
    }
}
