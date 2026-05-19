//! Setup and update commands for bundle configuration.

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::bootstrap::{LocalEnvOutcome, ensure_local_environment};
use greentic_deployer::environment::LocalFsStore;

use crate::cli_args::*;
use crate::cli_helpers::{
    complete_loaded_answers_with_prompts, ensure_deployment_targets_present,
    ensure_required_setup_answers_present, resolve_bundle_dir, resolve_setup_scope,
    run_interactive_wizard,
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

    let loaded_answers = if answers.is_some() {
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
        ensure_required_setup_answers_present(&bundle_dir, &loaded_answers)
            .context("Missing required answers in --non-interactive mode")?;
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
    let (_env, outcome) = ensure_local_environment(&store)
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
