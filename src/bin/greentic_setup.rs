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

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use greentic_setup::engine::{SetupConfig, SetupRequest};
use greentic_setup::plan::TenantSelection;
use greentic_setup::{bundle, SetupEngine, SetupMode};

fn main() -> Result<()> {
    let cli = Cli::parse();

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
    let bundle_path = cli.bundle.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Bundle path required. Usage: greentic-setup [OPTIONS] <BUNDLE>\n\nFor help: greentic-setup --help")
    })?;

    // Resolve bundle source (directory or .gtbundle file)
    let bundle_dir = resolve_bundle_source(bundle_path)?;

    // Validate bundle exists
    bundle::validate_bundle_exists(&bundle_dir).context("invalid bundle directory")?;

    println!("Greentic Setup");
    println!("  Bundle: {}", bundle_path.display());
    println!("  Tenant: {}", cli.tenant);
    println!("  Team: {}", cli.team.as_deref().unwrap_or("default"));
    println!("  Env: {}", cli.env);
    println!();

    let config = SetupConfig {
        tenant: cli.tenant.clone(),
        team: cli.team.clone(),
        env: cli.env.clone(),
        offline: false,
        verbose: true,
    };
    let engine = SetupEngine::new(config);

    // Load answers if provided
    let setup_answers = if let Some(answers_path) = &cli.answers {
        println!("Loading answers from: {}", answers_path.display());
        engine
            .load_answers(answers_path)
            .context("failed to read answers file")?
    } else if cli.emit_answers.is_some() {
        // Empty answers for emit mode
        serde_json::Map::new()
    } else if cli.dry_run {
        // Dry run without answers - will show wizard preview
        serde_json::Map::new()
    } else {
        // Interactive mode
        println!("Interactive wizard mode");
        println!("Use --answers <file> to provide answers, or");
        println!("Use --dry-run --emit-answers <file> to generate template.");
        println!();
        // TODO: Implement interactive wizard
        // For now, require answers file
        bail!("interactive wizard not yet implemented - use --answers <file>");
    };

    let request = SetupRequest {
        bundle: bundle_dir.clone(),
        tenants: vec![TenantSelection {
            tenant: cli.tenant.clone(),
            team: cli.team.clone(),
            allow_paths: Vec::new(),
        }],
        setup_answers,
        ..Default::default()
    };

    let is_dry_run = cli.dry_run || cli.emit_answers.is_some();
    let plan = engine
        .plan(SetupMode::Create, &request, is_dry_run)
        .context("failed to build plan")?;

    engine.print_plan(&plan);

    // Emit answers template if requested
    if let Some(emit_path) = &cli.emit_answers {
        engine
            .emit_answers(&plan, emit_path)
            .context("failed to emit answers template")?;
        println!("\nAnswers template written to: {}", emit_path.display());
        println!("Edit and use with: greentic-setup --answers {} {}", emit_path.display(), bundle_path.display());
        return Ok(());
    }

    if cli.dry_run {
        println!("\n[dry-run] Would setup bundle: {}", bundle_path.display());
        return Ok(());
    }

    engine.execute(&plan).context("failed to execute plan")?;

    println!("\nSetup complete: {}", bundle_path.display());

    Ok(())
}

/// Resolve bundle source - supports both directories and .gtbundle files
fn resolve_bundle_source(path: &PathBuf) -> Result<PathBuf> {
    use greentic_setup::gtbundle;

    // Check if it's a .gtbundle file (archive)
    if gtbundle::is_gtbundle_file(path) {
        println!("Extracting .gtbundle archive...");
        let temp_dir = gtbundle::extract_gtbundle_to_temp(path)
            .context("failed to extract .gtbundle archive")?;
        println!("  Extracted to: {}", temp_dir.display());
        return Ok(temp_dir);
    }

    // Check if it's a directory named *.gtbundle
    if gtbundle::is_gtbundle_dir(path) {
        return Ok(path.clone());
    }

    // Check if path ends with .gtbundle but doesn't exist
    let path_str = path.to_string_lossy();
    if path_str.ends_with(".gtbundle") && !path.exists() {
        bail!("bundle not found: {}", path.display());
    }

    // It's a directory path
    if path.is_dir() {
        Ok(path.clone())
    } else if path.exists() {
        bail!("expected directory or .gtbundle file: {}", path.display());
    } else {
        bail!("bundle not found: {}", path.display());
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
    let bundle_dir = args.path.unwrap_or_else(|| PathBuf::from("."));

    if bundle_dir.join("greentic.demo.yaml").exists() {
        println!("Bundle already exists at {}", bundle_dir.display());
        return Ok(());
    }

    println!("Creating bundle at {}...", bundle_dir.display());

    bundle::create_demo_bundle_structure(&bundle_dir, args.name.as_deref())
        .context("failed to create bundle structure")?;

    println!("Bundle created at {}", bundle_dir.display());
    println!("\nNext steps:");
    println!(
        "  1. greentic-setup bundle add <pack.gtpack> --bundle {}",
        bundle_dir.display()
    );
    println!("  2. greentic-setup bundle setup --bundle {} --answers answers.yaml", bundle_dir.display());

    Ok(())
}

fn add(args: BundleAddArgs) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    println!("Adding pack to bundle...");
    println!("  Pack ref: {}", args.pack_ref);
    println!("  Bundle: {}", bundle_dir.display());
    println!("  Tenant: {}", args.tenant);
    println!(
        "  Team: {}",
        args.team.as_deref().unwrap_or("default")
    );
    println!("  Env: {}", args.env);

    // Create bundle structure if it doesn't exist
    if !bundle_dir.join("greentic.demo.yaml").exists() {
        bundle::create_demo_bundle_structure(&bundle_dir, None)
            .context("failed to create bundle structure")?;
        println!("Created bundle structure at {}", bundle_dir.display());
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
        .context("failed to build plan")?;

    engine.print_plan(&plan);

    if args.dry_run {
        println!("\n[dry-run] Would add pack to bundle");
        return Ok(());
    }

    let report = engine.execute(&plan).context("failed to execute plan")?;

    println!("\nPack added to bundle successfully.");
    println!("  Resolved packs: {}", report.resolved_packs.len());

    Ok(())
}

fn setup(args: BundleSetupArgs) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context("invalid bundle directory")?;

    let provider_display = args
        .provider_id
        .clone()
        .unwrap_or_else(|| "all".to_string());

    println!("Setting up provider...");
    println!("  Provider: {}", provider_display);
    println!("  Bundle: {}", bundle_dir.display());
    println!("  Tenant: {}", args.tenant);
    println!(
        "  Team: {}",
        args.team.as_deref().unwrap_or("default")
    );
    println!("  Env: {}", args.env);
    println!("  Domain: {}", args.domain);

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
            .context("failed to read answers file")?
    } else if args.emit_answers.is_some() {
        // Empty answers for emit mode - will generate template
        serde_json::Map::new()
    } else if args.non_interactive {
        bail!("--answers required in non-interactive mode");
    } else {
        println!("\nInteractive setup not yet implemented.");
        println!("Use --answers <file> to provide setup answers.");
        println!("Or use --dry-run --emit-answers <file> to generate answers template.");
        bail!("interactive setup requires --answers file");
    };

    let providers = args.provider_id.clone().map_or_else(Vec::new, |id| vec![id]);

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
        .plan(SetupMode::Create, &request, args.dry_run || args.emit_answers.is_some())
        .context("failed to build plan")?;

    engine.print_plan(&plan);

    // Emit answers template if requested
    if let Some(emit_path) = &args.emit_answers {
        engine
            .emit_answers(&plan, emit_path)
            .context("failed to emit answers template")?;
        println!("\nAnswers template written to: {}", emit_path.display());
        println!("Edit and use with: greentic-setup bundle setup --answers {}", emit_path.display());
        return Ok(());
    }

    if args.dry_run {
        println!("\n[dry-run] Would setup provider: {}", provider_display);
        return Ok(());
    }

    engine.execute(&plan).context("failed to execute plan")?;

    println!("\nProvider setup complete: {}", provider_display);

    Ok(())
}

fn update(args: BundleSetupArgs) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context("invalid bundle directory")?;

    let provider_display = args
        .provider_id
        .clone()
        .unwrap_or_else(|| "all".to_string());

    println!("Updating provider configuration...");
    println!("  Provider: {}", provider_display);
    println!("  Domain: {}", args.domain);

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
            .context("failed to read answers file")?
    } else if args.emit_answers.is_some() {
        // Empty answers for emit mode - will generate template
        serde_json::Map::new()
    } else if args.non_interactive {
        bail!("--answers required in non-interactive mode");
    } else {
        println!("Use --answers <file> to provide setup answers.");
        println!("Or use --emit-answers <file> to generate answers template.");
        bail!("interactive update requires --answers file");
    };

    let providers = args.provider_id.clone().map_or_else(Vec::new, |id| vec![id]);

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
        .plan(SetupMode::Update, &request, args.dry_run || args.emit_answers.is_some())
        .context("failed to build plan")?;

    engine.print_plan(&plan);

    // Emit answers template if requested
    if let Some(emit_path) = &args.emit_answers {
        engine
            .emit_answers(&plan, emit_path)
            .context("failed to emit answers template")?;
        println!("\nAnswers template written to: {}", emit_path.display());
        println!("Edit and use with: greentic-setup bundle update --answers {}", emit_path.display());
        return Ok(());
    }

    if args.dry_run {
        println!("\n[dry-run] Would update provider: {}", provider_display);
        return Ok(());
    }

    engine.execute(&plan).context("failed to execute plan")?;

    println!("\nProvider update complete: {}", provider_display);

    Ok(())
}

fn remove(args: BundleRemoveArgs) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context("invalid bundle directory")?;

    println!("Removing provider...");
    println!("  Provider: {}", args.provider_id);
    println!("  Bundle: {}", bundle_dir.display());

    if !args.force {
        println!("\nThis will remove the provider configuration.");
        println!("Use --force to confirm.");
        bail!("removal cancelled - use --force to confirm");
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
        .context("failed to build plan")?;

    engine.print_plan(&plan);
    engine.execute(&plan).context("failed to execute plan")?;

    println!("\nProvider removed: {}", args.provider_id);

    Ok(())
}

fn build(args: BundleBuildArgs) -> Result<()> {
    use greentic_setup::gtbundle;

    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context("invalid bundle directory")?;

    let out_str = args.out.to_string_lossy();
    let is_archive = out_str.ends_with(".gtbundle");

    println!("Building portable bundle...");
    println!("  Bundle: {}", bundle_dir.display());
    println!("  Output: {}", args.out.display());
    println!("  Format: {}", if is_archive { "archive (.gtbundle)" } else { "directory" });

    if let Some(ref tenant) = args.tenant {
        println!("  Tenant: {}", tenant);
    }

    if args.doctor && !args.skip_doctor {
        println!("\nRunning doctor validation...");
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

    println!("\nBundle built successfully at {}", args.out.display());

    Ok(())
}

fn list(args: BundleListArgs) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    bundle::validate_bundle_exists(&bundle_dir).context("invalid bundle directory")?;

    let mut packs = Vec::new();
    let providers_dir = bundle_dir.join("providers");
    let packs_dir = bundle_dir.join("packs");

    // Check providers/<domain>/ directory
    let domain_dir = providers_dir.join(&args.domain);
    if domain_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&domain_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "gtpack") {
                    if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                        packs.push((name.to_string(), args.domain.clone()));
                    }
                }
            }
        }
    }

    // Check packs/ directory
    if packs_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&packs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "gtpack") {
                    if let Some(name) = path.file_stem().and_then(|n| n.to_str()) {
                        packs.push((name.to_string(), "pack".to_string()));
                    }
                }
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
        println!("Bundle: {}", bundle_dir.display());
        println!("Domain: {}", args.domain);
        println!("Packs found: {}", packs.len());

        for (name, domain) in &packs {
            println!("  - {} ({})", name, domain);
        }
    }

    Ok(())
}

fn status(args: BundleStatusArgs) -> Result<()> {
    let bundle_dir = resolve_bundle_dir(args.bundle)?;

    if !bundle_dir.exists() {
        if args.format == "json" {
            println!(
                r#"{{"exists": false, "path": "{}"}}"#,
                bundle_dir.display()
            );
        } else {
            println!("Bundle not found: {}", bundle_dir.display());
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
            "messaging", "events", "oauth", "secrets", "mcp", "state", "other",
        ] {
            let domain_dir = providers_dir.join(domain);
            if domain_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&domain_dir) {
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
    }

    // Check packs/ directory
    if packs_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&packs_dir) {
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
    }

    // Count tenants
    let tenants_dir = bundle_dir.join("tenants");
    let mut tenant_count = 0;
    let mut tenants = Vec::new();

    if tenants_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&tenants_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    tenant_count += 1;
                    if let Some(name) = entry.file_name().to_str() {
                        tenants.push(name.to_string());
                    }
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
        println!("Bundle: {}", bundle_dir.display());
        let valid_status = if is_valid {
            "yes".to_string()
        } else {
            "no (missing greentic.demo.yaml)".to_string()
        };
        println!("Valid: {}", valid_status);
        println!("Packs: {} installed", pack_count);
        for pack in &packs {
            println!("  - {}", pack);
        }
        println!("Tenants: {}", tenant_count);
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
