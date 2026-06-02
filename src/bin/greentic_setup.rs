//! greentic-setup CLI binary.
//!
//! ## Simple Usage (recommended)
//!
//! ```bash
//! greentic-setup ./my-bundle                              # Interactive wizard
//! greentic-setup --dry-run ./my-bundle                    # Preview wizard
//! greentic-setup --dry-run --emit-answers a.json ./my-bundle  # Generate template
//! greentic-setup --answers a.json ./my-bundle.gtbundle    # Apply answers
//! ```
//!
//! ## Advanced Usage (bundle subcommands)
//!
//! - `bundle init` - Initialize a new bundle directory
//! - `bundle add` - Add a pack to a bundle
//! - `bundle setup` - Run setup flow for provider(s)
//! - `bundle update` - Update provider configuration
//! - `bundle remove` - Remove a provider from a bundle
//! - `bundle build` - Build a portable bundle
//! - `bundle list` - List packs/flows in a bundle
//! - `bundle status` - Show bundle status

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::fs;
use std::io::{self, Write};
use std::thread;
use std::time::Duration;

use greentic_setup::cli_args::{BundleCommand, Cli, Command};
use greentic_setup::cli_commands;
use greentic_setup::cli_helpers::{
    SetupOutputTarget, complete_loaded_answers_with_prompts, copy_dir_recursive,
    ensure_deployment_targets_present, ensure_required_setup_answers_present,
    maybe_start_cli_setup_tunnel, prompt_setup_params, resolve_bundle_source,
    resolve_setup_scope_with_bundle, run_interactive_wizard, setup_output_target,
};
use greentic_setup::cli_i18n::CliI18n;
use greentic_setup::engine::{LoadedAnswers, SetupConfig, SetupRequest};
use greentic_setup::plan::TenantSelection;
use greentic_setup::platform_setup::StaticRoutesPolicy;
use greentic_setup::{SetupEngine, SetupMode, bundle, gtbundle};

/// Global i18n instance (initialized once at startup).
fn get_i18n() -> &'static CliI18n {
    get_i18n_with_locale(None)
}

/// Get i18n instance with optional locale override.
fn get_i18n_with_locale(locale: Option<&str>) -> &'static CliI18n {
    use std::sync::OnceLock;
    static I18N: OnceLock<CliI18n> = OnceLock::new();
    I18N.get_or_init(|| CliI18n::from_request(locale).expect("failed to initialize i18n"))
}

/// Initialize i18n with the specified locale (call early in main).
fn init_i18n(locale: Option<&str>) {
    let _ = get_i18n_with_locale(locale);
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    init_i18n(cli.locale.as_deref());
    let i18n = get_i18n();

    // Launch web UI by default unless --no-ui is set.
    #[cfg(feature = "ui")]
    if cli.ui && !cli.no_ui && !cli.non_interactive && cli.command.is_none() {
        return run_ui_mode(&cli, i18n);
    }

    match cli.command {
        Some(Command::Doctor(args)) => cli_commands::doctor(args, i18n),
        Some(Command::Bundle(cmd)) => match *cmd {
            BundleCommand::Init(args) => cli_commands::init(args, i18n),
            BundleCommand::Add(args) => cli_commands::add(args, i18n),
            BundleCommand::Setup(mut args) => {
                args.non_interactive = cli.non_interactive;
                cli_commands::setup(args, i18n)
            }
            BundleCommand::Update(mut args) => {
                args.non_interactive = cli.non_interactive;
                cli_commands::update(args, i18n)
            }
            BundleCommand::Remove(args) => cli_commands::remove(args, i18n),
            BundleCommand::Build(args) => cli_commands::build(args, i18n),
            BundleCommand::List(args) => cli_commands::list(args, i18n),
            BundleCommand::Status(args) => cli_commands::status(args, i18n),
        },
        None => run_simple_setup(&cli, i18n),
    }
}

/// Run simple setup mode: greentic-setup [OPTIONS] <BUNDLE>
fn run_simple_setup(cli: &Cli, i18n: &CliI18n) -> Result<()> {
    // If no bundle path given and no flags, run fully interactive mode
    let (bundle_path, mut tenant, mut team, mut env, advanced) = if cli.bundle.is_none()
        && cli.answers.is_none()
        && cli.emit_answers.is_none()
        && !cli.dry_run
    {
        let params = prompt_setup_params(cli, i18n)?;
        (
            params.bundle,
            params.tenant,
            params.team,
            params.env,
            params.advanced,
        )
    } else {
        let path = cli.bundle.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "{}\n\n{}",
                i18n.t("cli.simple.bundle_required"),
                i18n.t("cli.help.for_help")
            )
        })?;
        (
            path,
            cli.tenant.clone(),
            cli.team.clone(),
            cli.env.clone(),
            cli.advanced,
        )
    };

    let bundle_dir = resolve_bundle_source(&bundle_path, i18n)?;

    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;
    let loader_engine = SetupEngine::new(SetupConfig {
        tenant: tenant.clone(),
        team: team.clone(),
        env: env.clone(),
        offline: false,
        verbose: true,
    });

    let loaded_answers = if let Some(answers_path) = &cli.answers {
        println!(
            "{}",
            i18n.tf(
                "setup.answers.loaded",
                &[&answers_path.display().to_string()]
            )
        );
        loader_engine
            .load_answers(answers_path, cli.key.as_deref(), !cli.non_interactive)
            .context(i18n.t("cli.error.failed_read_answers"))?
    } else if cli.non_interactive {
        anyhow::bail!("{}", i18n.t("cli.error.answers_required"));
    } else if cli.emit_answers.is_some() || cli.dry_run {
        LoadedAnswers::default()
    } else {
        println!("{}", i18n.t("cli.simple.interactive_mode"));
        println!();
        run_interactive_wizard(&bundle_dir, &tenant, team.as_deref(), &env, advanced)?
    };

    (tenant, team, env) =
        resolve_setup_scope_with_bundle(tenant, team, env, &loaded_answers, &bundle_dir);

    println!("{}", i18n.t("cli.simple.header"));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.bundle",
            &[&bundle_path.display().to_string()]
        )
    );
    println!("{}", i18n.tf("cli.bundle.add.tenant", &[&tenant]));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.team",
            &[team.as_deref().unwrap_or("default")]
        )
    );
    println!("{}", i18n.tf("cli.bundle.add.env", &[&env]));
    println!();

    let mut loaded_answers = if cli.answers.is_some() {
        complete_loaded_answers_with_prompts(
            &bundle_dir,
            &tenant,
            team.as_deref(),
            &env,
            advanced,
            cli.non_interactive,
            loaded_answers,
        )?
    } else {
        loaded_answers
    };
    if cli.answers.is_some() {
        ensure_deployment_targets_present(&bundle_dir, &loaded_answers)?;
    }

    let is_dry_run = cli.dry_run || cli.emit_answers.is_some();
    let mut no_ui_oauth_server = if !is_dry_run {
        Some(
            greentic_setup::no_ui_oauth::start_callback_server(&bundle_dir, &env)
                .context("failed to start no-UI OAuth callback server")?,
        )
    } else {
        None
    };
    let _setup_tunnel = if !is_dry_run {
        let local_base_url = no_ui_oauth_server
            .as_ref()
            .map(|server| server.local_base_url.as_str())
            .unwrap_or("http://127.0.0.1:1");
        let tunnel = maybe_start_cli_setup_tunnel(&mut loaded_answers, local_base_url)
            .context("failed to start setup tunnel")?;
        if let Some(tunnel) = tunnel.as_ref() {
            println!("Setup tunnel public_base_url: {}", tunnel.public_base_url);
        } else {
            no_ui_oauth_server = None;
        }
        tunnel
    } else {
        None
    };
    if cli.non_interactive {
        ensure_required_setup_answers_present(&bundle_dir, &loaded_answers)
            .context("Missing required answers in --non-interactive mode")?;
    }

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        bundle_name: greentic_setup::bundle::read_bundle_name(&bundle_dir)
            .ok()
            .flatten(),
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
        .plan(SetupMode::Create, &request, is_dry_run)
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    if let Some(emit_path) = &cli.emit_answers {
        engine
            .emit_answers(&plan, emit_path, cli.key.as_deref(), !cli.non_interactive)
            .context(i18n.t("cli.error.failed_emit_answers"))?;
        println!(
            "\n{}",
            i18n.tf(
                "cli.bundle.setup.emit_written",
                &[&emit_path.display().to_string()]
            )
        );
        println!(
            "{}",
            i18n.tf(
                "cli.simple.emit_usage",
                &[
                    &emit_path.display().to_string(),
                    &bundle_path.display().to_string()
                ]
            )
        );
        return Ok(());
    }

    if cli.dry_run {
        println!(
            "\n{}",
            i18n.tf("cli.simple.dry_run", &[&bundle_path.display().to_string()])
        );
        return Ok(());
    }

    let report = engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;
    print_pending_setup_actions(&report.pending_setup_actions);
    wait_for_pending_oauth_callbacks(no_ui_oauth_server, &report.pending_setup_actions)?;
    if !cli.non_interactive {
        execute_pending_oauth_device_actions(&bundle_dir, &env, &report.pending_setup_actions)?;
    }

    if let Some(output_target) = setup_output_target(&bundle_path)? {
        match output_target {
            SetupOutputTarget::Directory(output_bundle) => {
                if output_bundle.exists() {
                    if output_bundle.is_dir() {
                        fs::remove_dir_all(&output_bundle).with_context(|| {
                            format!(
                                "failed to replace existing bundle directory {}",
                                output_bundle.display()
                            )
                        })?;
                    } else {
                        fs::remove_file(&output_bundle).with_context(|| {
                            format!(
                                "failed to replace existing bundle file {}",
                                output_bundle.display()
                            )
                        })?;
                    }
                }
                copy_dir_recursive(&bundle_dir, &output_bundle, false)
                    .context("failed to write configured local bundle directory")?;
                println!("Configured bundle written to: {}", output_bundle.display());
            }
            SetupOutputTarget::Archive(output_bundle) => {
                gtbundle::create_gtbundle(&bundle_dir, &output_bundle)
                    .context("failed to write configured .gtbundle archive")?;
                println!("Configured bundle written to: {}", output_bundle.display());
            }
        }
    }

    println!(
        "\n{}",
        i18n.tf(
            "setup.execute.success",
            &[&bundle_path.display().to_string()]
        )
    );

    Ok(())
}

fn print_pending_setup_actions(actions: &[greentic_setup::setup_actions::SetupAction]) {
    let visible_actions: Vec<_> = actions
        .iter()
        .filter(|action| {
            matches!(
                action.kind,
                greentic_setup::setup_actions::SetupActionKind::OauthInstallButton
            ) && action.status == greentic_setup::setup_actions::SetupActionStatus::Pending
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

fn wait_for_pending_oauth_callbacks(
    server: Option<greentic_setup::no_ui_oauth::NoUiOAuthCallbackServer>,
    actions: &[greentic_setup::setup_actions::SetupAction],
) -> Result<()> {
    let pending = greentic_setup::no_ui_oauth::pending_oauth_install_actions(actions);
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

fn execute_pending_oauth_device_actions(
    bundle_dir: &std::path::Path,
    env: &str,
    actions: &[greentic_setup::setup_actions::SetupAction],
) -> Result<()> {
    for action in actions {
        if action.kind != greentic_setup::setup_actions::SetupActionKind::OauthDeviceCode
            || action.status != greentic_setup::setup_actions::SetupActionStatus::Pending
        {
            continue;
        }

        println!("Starting {}...", action.label);
        let start = greentic_setup::oauth_device::start_oauth_device_code(
            bundle_dir,
            &greentic_setup::oauth_device::OAuthDeviceStartInput {
                provider_id: action.provider_id.clone(),
                tenant: action.tenant.clone(),
                team: action.team.clone(),
                action_id: action.id.clone(),
            },
            greentic_setup::oauth_device::DEFAULT_EXTENSION_KEY,
        )?;

        println!();
        println!("{}:", action.label);
        let instructions = device_action_instructions(action);
        if instructions.is_empty() {
            println!("Copy this code: {}", start.user_code);
            println!(
                "{}: {}",
                device_action_authorize_label(action),
                start.verification_uri
            );
            println!("Return here and verify when authorization is complete.");
        } else {
            for instruction in instructions {
                println!(
                    "{}",
                    render_device_instruction(action, &start, &instruction)
                );
            }
        }
        println!();
        println!("Code: {}", start.user_code);
        println!(
            "{}: {}",
            device_action_authorize_label(action),
            start.verification_uri
        );
        println!();
        print!("Press Enter to {}...", device_action_finalize_label(action));
        io::stdout().flush().ok();
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);

        let runtime =
            tokio::runtime::Runtime::new().context("failed to create OAuth polling runtime")?;
        let mut interval = start.interval.max(1);
        loop {
            let report = runtime.block_on(greentic_setup::oauth_device::poll_oauth_device_code(
                bundle_dir,
                env,
                &greentic_setup::oauth_device::OAuthDevicePollInput {
                    session_id: start.session_id.clone(),
                },
                greentic_setup::oauth_device::DEFAULT_EXTENSION_KEY,
            ))?;
            match report.status {
                greentic_setup::oauth_device::OAuthDevicePollStatus::Complete => {
                    if let Some(message) = device_action_success_message(action) {
                        println!("{message}");
                    } else {
                        println!("OAuth device-code setup complete for {}.", action.label);
                    }
                    if !report.persisted_keys.is_empty() {
                        println!("Persisted keys: {}", report.persisted_keys.join(", "));
                    }
                    break;
                }
                greentic_setup::oauth_device::OAuthDevicePollStatus::Pending
                | greentic_setup::oauth_device::OAuthDevicePollStatus::SlowDown => {
                    interval = report.interval.unwrap_or(interval).max(1);
                    if greentic_setup::setup_actions::current_epoch_secs() >= start.expires_at {
                        bail!("OAuth device code expired before authorization completed");
                    }
                    if let Some(message) = report.message {
                        println!("{message}");
                    } else {
                        println!("Authorization is not complete yet; checking again shortly.");
                    }
                    thread::sleep(Duration::from_secs(interval.min(30)));
                }
                greentic_setup::oauth_device::OAuthDevicePollStatus::Failed => {
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

fn device_action_instructions(action: &greentic_setup::setup_actions::SetupAction) -> Vec<String> {
    for key in [
        "instructions_after_start",
        "approval_steps",
        "device_code_steps",
        "steps",
    ] {
        if let Some(value) = action.extra.get(key)
            && let Some(items) = string_list_value(value)
            && !items.is_empty()
        {
            return items;
        }
    }
    Vec::new()
}

fn string_list_value(value: &serde_json::Value) -> Option<Vec<String>> {
    if let Some(items) = value.as_array() {
        return Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        );
    }
    value.as_str().map(|item| vec![item.to_string()])
}

fn action_extra_string(
    action: &greentic_setup::setup_actions::SetupAction,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .filter_map(|key| action.extra.get(*key))
        .find_map(|value| value.as_str().map(str::to_string))
}

fn device_action_authorize_label(action: &greentic_setup::setup_actions::SetupAction) -> String {
    action_extra_string(
        action,
        &[
            "authorize_label",
            "verification_label",
            "open_label",
            "start_label",
        ],
    )
    .unwrap_or_else(|| "Open verification page".to_string())
}

fn device_action_finalize_label(action: &greentic_setup::setup_actions::SetupAction) -> String {
    action_extra_string(
        action,
        &[
            "finalize_label",
            "verify_label",
            "complete_label",
            "poll_label",
        ],
    )
    .unwrap_or_else(|| "verify setup".to_string())
}

fn device_action_success_message(
    action: &greentic_setup::setup_actions::SetupAction,
) -> Option<String> {
    action_extra_string(
        action,
        &["success_message", "complete_message", "verified_message"],
    )
}

fn render_device_instruction(
    action: &greentic_setup::setup_actions::SetupAction,
    start: &greentic_setup::oauth_device::OAuthDeviceStartReport,
    template: &str,
) -> String {
    template
        .replace("{user_code}", &start.user_code)
        .replace("{code}", &start.user_code)
        .replace("{verification_uri}", &start.verification_uri)
        .replace("{verification_url}", &start.verification_uri)
        .replace("{label}", &action.label)
        .replace("{action_label}", &action.label)
        .replace("{authorize_label}", &device_action_authorize_label(action))
        .replace(
            "{verification_label}",
            &device_action_authorize_label(action),
        )
        .replace("{finalize_label}", &device_action_finalize_label(action))
}

/// Launch the web-based setup UI.
#[cfg(feature = "ui")]
fn run_ui_mode(cli: &Cli, i18n: &CliI18n) -> Result<()> {
    let bundle_path = cli.bundle.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "{}\n\n{}",
            i18n.t("cli.simple.bundle_required"),
            i18n.t("cli.help.for_help")
        )
    })?;

    let bundle_dir = resolve_bundle_source(&bundle_path, i18n)?;
    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

    // Compute the write-back target up front so that after the UI session
    // completes the extracted bundle dir gets re-packed (or copied) back
    // to the user's original input — matches the behaviour of
    // run_simple_setup which calls gtbundle::create_gtbundle directly.
    let output_target = setup_output_target(&bundle_path)?;

    // Load answers from --answers file for UI pre-fill (values + scope).
    let (prefill_answers, answers_tenant, answers_team, answers_env) = if let Some(answers_path) =
        &cli.answers
    {
        println!(
            "{}",
            i18n.tf(
                "setup.answers.loaded",
                &[&answers_path.display().to_string()],
            ),
        );
        let loader_engine = SetupEngine::new(SetupConfig {
            tenant: cli.tenant.clone(),
            team: cli.team.clone(),
            env: cli.env.clone(),
            offline: false,
            verbose: false,
        });
        let loaded = loader_engine
            .load_answers(answers_path, cli.key.as_deref(), true)
            .context(i18n.t("cli.error.failed_read_answers"))?;
        // Persist tunnel config from answers so greentic-start can read it.
        if let Some(tunnel) = loaded.platform_setup.tunnel.as_ref() {
            let _ = greentic_setup::platform_setup::persist_tunnel_artifact(&bundle_dir, tunnel);
        }
        let mut prefill = loaded.setup_answers;
        prefill.insert(
            "platform_setup".to_string(),
            serde_json::to_value(&loaded.platform_setup)
                .context("failed to serialize platform setup answers for UI prefill")?,
        );
        (Some(prefill), loaded.tenant, loaded.team, loaded.env)
    } else {
        (None, None, None, None)
    };

    // Use scope from answers file when available, fall back to CLI args.
    // Track whether scope came from answers so the UI skips bundle detection.
    let scope_from_answers =
        answers_tenant.is_some() || answers_team.is_some() || answers_env.is_some();
    let tenant = answers_tenant.unwrap_or_else(|| cli.tenant.clone());
    let team = answers_team.or_else(|| cli.team.clone());
    let env = answers_env.unwrap_or_else(|| cli.env.clone());

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(greentic_setup::ui::launch(
        &bundle_dir,
        &tenant,
        team.as_deref(),
        &env,
        cli.advanced,
        cli.locale.as_deref(),
        prefill_answers,
        scope_from_answers,
        output_target,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn bundle_build_parses_as_subcommand_even_with_default_ui_enabled() {
        let cli = Cli::parse_from([
            "greentic-setup",
            "bundle",
            "build",
            "--bundle",
            "./demo-bundle",
            "--out",
            "/tmp/demo.gtbundle",
        ]);

        match cli.command {
            Some(Command::Bundle(cmd)) => match *cmd {
                BundleCommand::Build(args) => {
                    assert_eq!(
                        args.bundle.as_deref(),
                        Some(std::path::Path::new("./demo-bundle"))
                    );
                    assert_eq!(args.out, std::path::PathBuf::from("/tmp/demo.gtbundle"));
                }
                other => panic!("expected bundle build subcommand, got {other:?}"),
            },
            other => panic!("expected bundle build subcommand, got {other:?}"),
        }
    }

    #[test]
    fn non_interactive_parses_as_global_simple_flag() {
        let cli = Cli::parse_from([
            "greentic-setup",
            "--non-interactive",
            "--answers",
            "answers.json",
            "./demo-bundle",
        ]);

        assert!(cli.non_interactive);
        assert_eq!(
            cli.answers.as_deref(),
            Some(std::path::Path::new("answers.json"))
        );
    }

    #[test]
    fn non_interactive_parses_after_bundle_subcommand() {
        let cli = Cli::parse_from([
            "greentic-setup",
            "bundle",
            "setup",
            "--non-interactive",
            "--answers",
            "answers.json",
        ]);

        assert!(cli.non_interactive);
        match cli.command {
            Some(Command::Bundle(cmd)) => match *cmd {
                BundleCommand::Setup(args) => {
                    assert_eq!(
                        args.answers.as_deref(),
                        Some(std::path::Path::new("answers.json"))
                    );
                }
                other => panic!("expected bundle setup subcommand, got {other:?}"),
            },
            other => panic!("expected bundle setup subcommand, got {other:?}"),
        }
    }

    #[test]
    fn doctor_parses_as_top_level_subcommand() {
        let cli = Cli::parse_from([
            "greentic-setup",
            "doctor",
            "./demo",
            "--json",
            "--stage",
            "locks",
        ]);

        match cli.command {
            Some(Command::Doctor(args)) => {
                assert_eq!(args.bundle, std::path::PathBuf::from("./demo"));
                assert!(args.json);
                assert!(!args.show_info);
                assert_eq!(
                    args.stage,
                    Some(greentic_setup::cli_args::DoctorStageArg::Locks)
                );
            }
            other => panic!("expected doctor subcommand, got {other:?}"),
        }
    }
}
