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

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use greentic_setup::cli_i18n::CliI18n;
use greentic_setup::engine::{LoadedAnswers, SetupConfig, SetupRequest};
use greentic_setup::plan::TenantSelection;
use greentic_setup::platform_setup::{
    PlatformSetupAnswers, StaticRoutesPolicy, load_static_routes_artifact,
    prompt_static_routes_policy,
};
use greentic_setup::qa::wizard;
use greentic_setup::setup_to_formspec;
use greentic_setup::{SetupEngine, SetupMode, bundle, discovery};

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

    // Initialize i18n with CLI locale
    init_i18n(cli.locale.as_deref());

    match cli.command {
        Some(Command::Bundle(cmd)) => match cmd {
            BundleCommand::Init(args) => init(args),
            BundleCommand::Add(args) => add(args),
            BundleCommand::Setup(args) => setup(args),
            BundleCommand::Update(args) => update(args),
            BundleCommand::Remove(args) => remove(args),
            BundleCommand::Build(args) => build(args),
            BundleCommand::List(args) => list(args),
            BundleCommand::Status(args) => status(args),
        },
        None => {
            // Simple mode: greentic-setup [OPTIONS] <BUNDLE>
            run_simple_setup(&cli)
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "greentic-setup")]
#[command(version)]
#[command(about = "Greentic bundle setup CLI")]
#[command(after_help = r#"EXAMPLES:
  Interactive wizard:
    greentic-setup ./my-bundle

  Preview without executing:
    greentic-setup --dry-run ./my-bundle

  Generate answers template:
    greentic-setup --dry-run --emit-answers answers.json ./my-bundle

  Apply answers file:
    greentic-setup --answers answers.json ./my-bundle.gtbundle

  Advanced (bundle subcommands):
    greentic-setup bundle init ./my-bundle
    greentic-setup bundle add pack.gtpack --bundle ./my-bundle
    greentic-setup bundle status --bundle ./my-bundle
"#)]
struct Cli {
    /// Bundle path (.gtbundle file or directory)
    #[arg(value_name = "BUNDLE")]
    bundle: Option<PathBuf>,

    /// Dry run - show wizard but don't execute
    #[arg(long = "dry-run", global = true)]
    dry_run: bool,

    /// Emit answers template to file (combine with --dry-run to only generate)
    #[arg(long = "emit-answers", value_name = "FILE", global = true)]
    emit_answers: Option<PathBuf>,

    /// Apply answers from file
    #[arg(long = "answers", short = 'a', value_name = "FILE", global = true)]
    answers: Option<PathBuf>,

    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo", global = true)]
    tenant: String,

    /// Team identifier
    #[arg(long = "team", global = true)]
    team: Option<String>,

    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev", global = true)]
    env: String,

    /// UI locale (BCP-47 tag, e.g., en, ja, id)
    #[arg(long = "locale", global = true)]
    locale: Option<String>,

    /// Advanced mode — show all questions including optional ones
    #[arg(long = "advanced", global = true)]
    advanced: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Bundle lifecycle management (advanced)
    #[command(subcommand)]
    Bundle(BundleCommand),
}

/// Run simple setup mode: greentic-setup [OPTIONS] <BUNDLE>
fn run_simple_setup(cli: &Cli) -> Result<()> {
    let i18n = get_i18n();

    // If no bundle path given and no flags, run fully interactive mode
    let (bundle_path, tenant, team, env, advanced) = if cli.bundle.is_none()
        && cli.answers.is_none()
        && cli.emit_answers.is_none()
        && !cli.dry_run
    {
        let params = prompt_setup_params(cli)?;
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

    // Resolve bundle source (directory or .gtbundle file)
    let bundle_dir = resolve_bundle_source(&bundle_path)?;

    // Validate bundle exists
    bundle::validate_bundle_exists(&bundle_dir).context(i18n.t("cli.error.invalid_bundle"))?;

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

    let config = SetupConfig {
        tenant: tenant.clone(),
        team: team.clone(),
        env: env.clone(),
        offline: false,
        verbose: true,
    };
    let engine = SetupEngine::new(config);

    // Load answers if provided
    let loaded_answers = if let Some(answers_path) = &cli.answers {
        println!(
            "{}",
            i18n.tf(
                "setup.answers.loaded",
                &[&answers_path.display().to_string()]
            )
        );
        engine
            .load_answers(answers_path)
            .context(i18n.t("cli.error.failed_read_answers"))?
    } else if cli.emit_answers.is_some() || cli.dry_run {
        LoadedAnswers::default()
    } else {
        println!("{}", i18n.t("cli.simple.interactive_mode"));
        println!();
        run_interactive_wizard(&bundle_dir, &cli.env, advanced)?
    };

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        tenants: vec![TenantSelection {
            tenant: tenant.clone(),
            team: team.clone(),
            allow_paths: Vec::new(),
        }],
        static_routes: StaticRoutesPolicy::normalize(
            loaded_answers.platform_setup.static_routes.as_ref(),
            &cli.env,
        )
        .context(i18n.t("cli.error.failed_read_answers"))?,
        setup_answers: loaded_answers.setup_answers,
        ..Default::default()
    };

    let is_dry_run = cli.dry_run || cli.emit_answers.is_some();
    let plan = engine
        .plan(SetupMode::Create, &request, is_dry_run)
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    // Emit answers template if requested
    if let Some(emit_path) = &cli.emit_answers {
        engine
            .emit_answers(&plan, emit_path)
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

    engine
        .execute(&plan)
        .context(i18n.t("cli.error.failed_execute_plan"))?;

    println!(
        "\n{}",
        i18n.tf(
            "setup.execute.success",
            &[&bundle_path.display().to_string()]
        )
    );

    Ok(())
}

/// Resolve bundle source - supports both directories and .gtbundle files
fn resolve_bundle_source(path: &std::path::Path) -> Result<PathBuf> {
    use greentic_setup::gtbundle;

    let i18n = get_i18n();

    // Check if it's a .gtbundle file (archive)
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

    // Check if it's a directory named *.gtbundle
    if gtbundle::is_gtbundle_dir(path) {
        return Ok(path.to_path_buf());
    }

    // Check if path ends with .gtbundle but doesn't exist
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

    // It's a directory path
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

#[derive(Subcommand, Debug, Clone)]
enum BundleCommand {
    /// Initialize a new bundle directory
    Init(BundleInitArgs),
    /// Add a pack to a bundle
    Add(BundleAddArgs),
    /// Run setup flow for provider(s) in a bundle
    Setup(BundleSetupArgs),
    /// Update a provider's configuration in a bundle
    Update(BundleSetupArgs),
    /// Remove a provider from a bundle
    Remove(BundleRemoveArgs),
    /// Build a portable bundle (copy + resolve)
    Build(BundleBuildArgs),
    /// List packs or flows in a bundle
    List(BundleListArgs),
    /// Show bundle status
    Status(BundleStatusArgs),
}

#[derive(Args, Debug, Clone)]
struct BundleInitArgs {
    /// Bundle directory (default: current directory)
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Bundle name
    #[arg(long = "name", short = 'n')]
    name: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct BundleAddArgs {
    /// Pack reference (local path or OCI reference)
    #[arg(value_name = "PACK_REF")]
    pack_ref: String,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    bundle: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    team: Option<String>,
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    env: String,
    /// Dry run (don't actually add)
    #[arg(long = "dry-run")]
    dry_run: bool,
}

#[derive(Args, Debug, Clone)]
struct BundleSetupArgs {
    /// Provider ID to setup/update (optional, setup all if not specified)
    #[arg(value_name = "PROVIDER_ID")]
    provider_id: Option<String>,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    bundle: Option<PathBuf>,
    /// Answers file (JSON/YAML)
    #[arg(long = "answers", short = 'a')]
    answers: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    team: Option<String>,
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    env: String,
    /// Filter by domain (messaging/events/secrets/oauth/all)
    #[arg(long = "domain", short = 'd', default_value = "all")]
    domain: String,
    /// Number of parallel setup operations
    #[arg(long = "parallel", default_value = "1")]
    parallel: usize,
    /// Backup existing config before setup
    #[arg(long = "backup")]
    backup: bool,
    /// Skip secrets initialization
    #[arg(long = "skip-secrets-init")]
    skip_secrets_init: bool,
    /// Continue on error (best effort)
    #[arg(long = "best-effort")]
    best_effort: bool,
    /// Non-interactive mode (require --answers)
    #[arg(long = "non-interactive")]
    non_interactive: bool,
    /// Dry run (plan only, don't execute)
    #[arg(long = "dry-run")]
    dry_run: bool,
    /// Emit answers template JSON (use with --dry-run)
    #[arg(long = "emit-answers")]
    emit_answers: Option<PathBuf>,
    /// Advanced mode — show all questions including optional ones
    #[arg(long = "advanced")]
    advanced: bool,
}

#[derive(Args, Debug, Clone)]
struct BundleRemoveArgs {
    /// Provider ID to remove
    #[arg(value_name = "PROVIDER_ID")]
    provider_id: String,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    bundle: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    team: Option<String>,
    /// Force removal without confirmation
    #[arg(long = "force", short = 'f')]
    force: bool,
}

#[derive(Args, Debug, Clone)]
struct BundleBuildArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    bundle: Option<PathBuf>,
    /// Output directory for portable bundle
    #[arg(long = "out", short = 'o')]
    out: PathBuf,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't')]
    tenant: Option<String>,
    /// Team identifier
    #[arg(long = "team")]
    team: Option<String>,
    /// Only include used providers
    #[arg(long = "only-used-providers")]
    only_used_providers: bool,
    /// Run doctor validation after build
    #[arg(long = "doctor")]
    doctor: bool,
    /// Skip doctor validation
    #[arg(long = "skip-doctor")]
    skip_doctor: bool,
}

#[derive(Args, Debug, Clone)]
struct BundleListArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    bundle: Option<PathBuf>,
    /// Filter by domain (messaging/events/secrets/oauth)
    #[arg(long = "domain", short = 'd', default_value = "messaging")]
    domain: String,
    /// Show flows for a specific pack
    #[arg(long = "pack", short = 'p')]
    pack: Option<String>,
    /// Output format (text/json)
    #[arg(long = "format", default_value = "text")]
    format: String,
}

#[derive(Args, Debug, Clone)]
struct BundleStatusArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    bundle: Option<PathBuf>,
    /// Output format (text/json)
    #[arg(long = "format", default_value = "text")]
    format: String,
}

// ── Command Implementations ─────────────────────────────────────────────────

fn init(args: BundleInitArgs) -> Result<()> {
    let i18n = get_i18n();
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

fn add(args: BundleAddArgs) -> Result<()> {
    let i18n = get_i18n();
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

fn setup(args: BundleSetupArgs) -> Result<()> {
    let i18n = get_i18n();
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

fn update(args: BundleSetupArgs) -> Result<()> {
    let i18n = get_i18n();
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

fn remove(args: BundleRemoveArgs) -> Result<()> {
    let i18n = get_i18n();
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

fn build(args: BundleBuildArgs) -> Result<()> {
    use greentic_setup::gtbundle;

    let i18n = get_i18n();
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

fn list(args: BundleListArgs) -> Result<()> {
    let i18n = get_i18n();
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

fn status(args: BundleStatusArgs) -> Result<()> {
    let i18n = get_i18n();
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

// ── Helpers ─────────────────────────────────────────────────────────────────

fn resolve_bundle_dir(bundle: Option<PathBuf>) -> Result<PathBuf> {
    match bundle {
        Some(path) => Ok(path),
        None => std::env::current_dir().context("failed to get current directory"),
    }
}

fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf, _only_used: bool) -> Result<()> {
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

/// Parameters collected from interactive prompts.
struct SetupParams {
    bundle: PathBuf,
    tenant: String,
    team: Option<String>,
    env: String,
    advanced: bool,
}

/// Prompt the user for setup parameters when no arguments are given.
fn prompt_setup_params(cli: &Cli) -> Result<SetupParams> {
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
    let bundle_dir = resolve_bundle_source(&bundle)?;
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

/// Detect provider domain from .gtpack filename prefix.
///
/// Known prefixes: messaging-, state-, telemetry-, events-, oauth-, secrets-.
/// Falls back to "messaging" for unrecognized prefixes.
fn detect_domain_from_filename(filename: &str) -> &'static str {
    let stem = filename.strip_suffix(".gtpack").unwrap_or(filename);
    // messaging-, state-, telemetry- all live in providers/messaging/
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
        "messaging" // default fallback
    }
}

/// Resolve a pack source (local path or OCI reference) to a local file path.
///
/// Supports:
/// - Local paths: `./messaging-telegram.gtpack`, `../packs/state-redis.gtpack`
/// - OCI references: `oci://ghcr.io/org/packs/mcp-github.gtpack:latest`
fn resolve_pack_source(source: &str) -> Result<PathBuf> {
    use greentic_setup::bundle_source::BundleSource;

    let parsed = BundleSource::parse(source)?;

    if parsed.is_local() {
        let path = parsed.resolve()?;
        if path.extension().and_then(|e| e.to_str()) != Some("gtpack") {
            anyhow::bail!("Not a .gtpack file: {source}");
        }
        Ok(path)
    } else {
        // OCI / repo / store — fetch via distributor-client
        println!("    Fetching from registry...");
        let path = parsed.resolve()?;
        println!("    Downloaded to cache: {}", path.display());
        Ok(path)
    }
}

/// Run interactive wizard for all discovered packs in the bundle.
///
/// Discovers packs, builds FormSpec for each, and prompts the user
/// for configuration answers interactively.
fn run_interactive_wizard(bundle_path: &std::path::Path, env: &str, advanced: bool) -> Result<LoadedAnswers> {
    use serde_json::Value;

    let mut all_answers = serde_json::Map::new();
    let existing_static_routes = load_static_routes_artifact(bundle_path)?;
    let static_routes = prompt_static_routes_policy(env, existing_static_routes.as_ref())?;

    // Discover packs in the bundle
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
            let answers = wizard::prompt_form_spec_answers(&spec, provider_id, advanced)?;
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

    Ok(LoadedAnswers {
        platform_setup: PlatformSetupAnswers {
            static_routes: Some(static_routes.to_answers()),
        },
        setup_answers: all_answers,
    })
}
