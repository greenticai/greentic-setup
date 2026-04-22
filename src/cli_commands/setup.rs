//! Setup and update commands for bundle configuration.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use greentic_secrets_cli::passphrase::{PassphraseSource, resolve as resolve_passphrase};
use greentic_secrets_passphrase::{PromptMode, derive_master_key, peek_header};
use secrets_provider_dev::PassphraseKeyProvider;

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
        passphrase_stdin,
        passphrase_file,
        reconfigure,
        allow_downgrade,
    } = args;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    // If --reconfigure: wipe existing dev store + marker so first-run prompt fires.
    if reconfigure {
        let store_path = crate::secrets::default_path(&bundle_dir);
        let _ = std::fs::remove_file(&store_path);
        let marker = {
            let mut p = store_path.clone();
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            p.set_file_name(format!("{name}.encrypted-marker"));
            p
        };
        let _ = std::fs::remove_file(&marker);
        eprintln!("{}", i18n.t("cli.setup.passphrase.reconfigured"));
    }

    // Initialize the global passphrase-derived KeyProvider before any
    // dev store open. Subsequent calls to crate::secrets::open_dev_store
    // (and SecretsSetup::new) will use AES-256-GCM with this provider.
    init_global_passphrase_provider(
        &bundle_dir,
        passphrase_stdin,
        passphrase_file.as_deref(),
        allow_downgrade,
        i18n,
    )?;

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

    // Diff-only interactive prompting: ask the user only for required
    // pack secrets that are missing from BOTH the dev store AND
    // seeds.yaml. Existing values are never re-prompted. Empty
    // optional values are skipped. --non-interactive skips this step.
    if !non_interactive {
        prompt_missing_pack_secrets_blocking(&bundle_dir, engine.config(), provider_id.as_deref())
            .context(i18n.t("cli.setup.passphrase.failed"))?;
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

/// Discover packs in the bundle, enumerate their required secrets,
/// and interactively prompt the user for any value that is missing
/// from both the dev store and `seeds.yaml`. Returns immediately if
/// no missing keys are found — existing values are never re-prompted.
fn prompt_missing_pack_secrets_blocking(
    bundle_dir: &Path,
    config: &crate::engine::SetupConfig,
    provider_filter: Option<&str>,
) -> Result<()> {
    use crate::secrets::SecretsSetup;
    use crate::secrets_prompt::prompt_missing_keys;

    // The discovery + missing-key enumeration + persistence are all
    // async at the underlying SecretsBackend level. Wrap a single
    // tokio runtime around them; this mirrors the pattern used by
    // engine/executors.rs::persist_all_config_as_secrets.
    let rt = tokio::runtime::Runtime::new().context("creating tokio runtime for secret prompts")?;

    rt.block_on(async {
        let setup = SecretsSetup::new(
            bundle_dir,
            &config.env,
            &config.tenant,
            config.team.as_deref(),
        )?;

        let discovered = match crate::discovery::discover(bundle_dir) {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };

        let mut all_missing = Vec::new();
        for target in discovered.setup_targets() {
            if let Some(filter) = provider_filter
                && target.provider_id != filter
            {
                continue;
            }
            let missing = setup
                .missing_pack_secrets(&target.pack_path, &target.provider_id)
                .await?;
            all_missing.extend(missing);
        }

        if all_missing.is_empty() {
            return Ok(());
        }

        eprintln!(
            "\n{} required secret value(s) missing — please enter them:",
            all_missing.len()
        );
        let entered = prompt_missing_keys(&all_missing)?;
        for (uri, value) in entered {
            setup.set_secret_text(&uri, &value).await?;
        }
        Ok::<_, anyhow::Error>(())
    })
}

/// Resolve a passphrase from the CLI flags + bundle state and install
/// the resulting `PassphraseKeyProvider` as the process-global key
/// provider. Idempotent — only the first call wins.
fn init_global_passphrase_provider(
    bundle_dir: &Path,
    passphrase_stdin: bool,
    passphrase_file: Option<&Path>,
    allow_downgrade: bool,
    i18n: &CliI18n,
) -> Result<()> {
    if crate::secrets::has_global_key_provider() {
        return Ok(());
    }

    let store_path = crate::secrets::default_path(bundle_dir);
    let existing_header = peek_header(&store_path).ok().flatten();
    let mode = if existing_header.is_some() {
        PromptMode::Unlock
    } else {
        PromptMode::Initial
    };

    let source = if let Some(p) = passphrase_file {
        PassphraseSource::File(p)
    } else if passphrase_stdin || std::env::var("GREENTIC_PASSPHRASE_STDIN").as_deref() == Ok("1") {
        PassphraseSource::Stdin
    } else {
        PassphraseSource::Tty(mode)
    };

    let passphrase = resolve_passphrase(source).context(i18n.t("cli.setup.passphrase.failed"))?;

    let salt = match &existing_header {
        Some(h) => h.salt,
        None => greentic_secrets_passphrase::random_salt(),
    };
    let master_key =
        derive_master_key(&passphrase, &salt).context(i18n.t("cli.setup.passphrase.kdf_failed"))?;
    drop(passphrase);

    let provider = Arc::new(PassphraseKeyProvider::new(master_key, salt));
    crate::secrets::set_global_key_provider(provider, allow_downgrade);

    if existing_header.is_none() {
        eprintln!("{}", i18n.t("cli.setup.passphrase.first_setup_complete"));
    }
    Ok(())
}
