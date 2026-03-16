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
use clap::{Parser, Subcommand};

use greentic_setup::bundle;
use greentic_setup::cli;
use greentic_setup::cli_i18n::CliI18n;
use greentic_setup::engine::{SetupConfig, SetupRequest};
use greentic_setup::gtbundle;
use greentic_setup::plan::TenantSelection;
use greentic_setup::{SetupEngine, SetupMode};

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
    let cli_args = Cli::parse();

    // Initialize i18n with CLI locale
    init_i18n(cli_args.locale.as_deref());

    match cli_args.command {
        Some(Command::Bundle(cmd)) => {
            let i18n = get_i18n();
            match cmd {
                BundleCommand::Init(args) => cli::bundle::init(args, i18n),
                BundleCommand::Add(args) => cli::bundle::add(args, i18n),
                BundleCommand::Setup(args) => cli::bundle::setup(args, i18n),
                BundleCommand::Update(args) => cli::bundle::update(args, i18n),
                BundleCommand::Remove(args) => cli::bundle::remove(args, i18n),
                BundleCommand::Build(args) => cli::bundle::build(args, i18n),
                BundleCommand::List(args) => cli::bundle::list(args, i18n),
                BundleCommand::Status(args) => cli::bundle::status(args, i18n),
            }
        }
        Some(Command::Wizard(cmd)) => {
            let i18n = get_i18n();
            match cmd {
                WizardCommand::Apply(args) => cli::wizard::apply(args, i18n),
            }
        }
        None => {
            // Simple mode: greentic-setup [OPTIONS] <BUNDLE>
            run_simple_setup(&cli_args)
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

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Bundle lifecycle management (advanced)
    #[command(subcommand)]
    Bundle(BundleCommand),

    /// Bundle wizard (create bundle with packs)
    #[command(subcommand)]
    Wizard(WizardCommand),
}

#[derive(Subcommand, Debug, Clone)]
enum BundleCommand {
    /// Initialize a new bundle directory
    Init(cli::BundleInitArgs),
    /// Add a pack to a bundle
    Add(cli::BundleAddArgs),
    /// Run setup flow for provider(s) in a bundle
    Setup(cli::BundleSetupArgs),
    /// Update a provider's configuration in a bundle
    Update(cli::BundleSetupArgs),
    /// Remove a provider from a bundle
    Remove(cli::BundleRemoveArgs),
    /// Build a portable bundle (copy + resolve)
    Build(cli::BundleBuildArgs),
    /// List packs or flows in a bundle
    List(cli::BundleListArgs),
    /// Show bundle status
    Status(cli::BundleStatusArgs),
}

#[derive(Subcommand, Debug, Clone)]
enum WizardCommand {
    /// Apply bundle wizard from answer document
    Apply(cli::WizardApplyArgs),
}

/// Run simple setup mode: greentic-setup [OPTIONS] <BUNDLE>
fn run_simple_setup(cli_args: &Cli) -> Result<()> {
    let i18n = get_i18n();
    let bundle_path = cli_args.bundle.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "{}\n\n{}",
            i18n.t("cli.simple.bundle_required"),
            i18n.t("cli.help.for_help")
        )
    })?;

    // Resolve bundle source (directory or .gtbundle file)
    let bundle_dir = resolve_bundle_source(bundle_path)?;

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
    println!("{}", i18n.tf("cli.bundle.add.tenant", &[&cli_args.tenant]));
    println!(
        "{}",
        i18n.tf(
            "cli.bundle.add.team",
            &[cli_args.team.as_deref().unwrap_or("default")]
        )
    );
    println!("{}", i18n.tf("cli.bundle.add.env", &[&cli_args.env]));
    println!();

    let config = SetupConfig {
        tenant: cli_args.tenant.clone(),
        team: cli_args.team.clone(),
        env: cli_args.env.clone(),
        offline: false,
        verbose: true,
    };
    let engine = SetupEngine::new(config);

    // Load answers if provided
    let setup_answers = if let Some(answers_path) = &cli_args.answers {
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
    } else if cli_args.emit_answers.is_some() {
        // Empty answers for emit mode
        serde_json::Map::new()
    } else if cli_args.dry_run {
        // Dry run without answers - will show wizard preview
        serde_json::Map::new()
    } else {
        // Interactive mode - run wizard for each discovered pack
        println!("{}", i18n.t("cli.simple.interactive_mode"));
        println!();
        cli::bundle::run_interactive_wizard(&bundle_dir)?
    };

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        tenants: vec![TenantSelection {
            tenant: cli_args.tenant.clone(),
            team: cli_args.team.clone(),
            allow_paths: Vec::new(),
        }],
        setup_answers,
        ..Default::default()
    };

    let is_dry_run = cli_args.dry_run || cli_args.emit_answers.is_some();
    let plan = engine
        .plan(SetupMode::Create, &request, is_dry_run)
        .context(i18n.t("cli.error.failed_build_plan"))?;

    engine.print_plan(&plan);

    // Emit answers template if requested
    if let Some(emit_path) = &cli_args.emit_answers {
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

    if cli_args.dry_run {
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

/// Resolve bundle source - supports both directories and .gtbundle files.
fn resolve_bundle_source(path: &std::path::Path) -> Result<PathBuf> {
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
