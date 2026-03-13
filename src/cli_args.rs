//! CLI argument definitions for greentic-setup.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

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
pub struct Cli {
    /// Bundle path (.gtbundle file or directory)
    #[arg(value_name = "BUNDLE")]
    pub bundle: Option<PathBuf>,

    /// Dry run - show wizard but don't execute
    #[arg(long = "dry-run", global = true)]
    pub dry_run: bool,

    /// Emit answers template to file (combine with --dry-run to only generate)
    #[arg(long = "emit-answers", value_name = "FILE", global = true)]
    pub emit_answers: Option<PathBuf>,

    /// Apply answers from file
    #[arg(long = "answers", short = 'a', value_name = "FILE", global = true)]
    pub answers: Option<PathBuf>,

    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo", global = true)]
    pub tenant: String,

    /// Team identifier
    #[arg(long = "team", global = true)]
    pub team: Option<String>,

    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev", global = true)]
    pub env: String,

    /// UI locale (BCP-47 tag, e.g., en, ja, id)
    #[arg(long = "locale", global = true)]
    pub locale: Option<String>,

    /// Advanced mode — show all questions including optional ones
    #[arg(long = "advanced", global = true)]
    pub advanced: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Bundle lifecycle management (advanced)
    #[command(subcommand)]
    Bundle(BundleCommand),
}

#[derive(Subcommand, Debug, Clone)]
pub enum BundleCommand {
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
pub struct BundleInitArgs {
    /// Bundle directory (default: current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,
    /// Bundle name
    #[arg(long = "name", short = 'n')]
    pub name: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct BundleAddArgs {
    /// Pack reference (local path or OCI reference)
    #[arg(value_name = "PACK_REF")]
    pub pack_ref: String,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    pub tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Dry run (don't actually add)
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone)]
pub struct BundleSetupArgs {
    /// Provider ID to setup/update (optional, setup all if not specified)
    #[arg(value_name = "PROVIDER_ID")]
    pub provider_id: Option<String>,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Answers file (JSON/YAML)
    #[arg(long = "answers", short = 'a')]
    pub answers: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    pub tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Filter by domain (messaging/events/secrets/oauth/all)
    #[arg(long = "domain", short = 'd', default_value = "all")]
    pub domain: String,
    /// Number of parallel setup operations
    #[arg(long = "parallel", default_value = "1")]
    pub parallel: usize,
    /// Backup existing config before setup
    #[arg(long = "backup")]
    pub backup: bool,
    /// Skip secrets initialization
    #[arg(long = "skip-secrets-init")]
    pub skip_secrets_init: bool,
    /// Continue on error (best effort)
    #[arg(long = "best-effort")]
    pub best_effort: bool,
    /// Non-interactive mode (require --answers)
    #[arg(long = "non-interactive")]
    pub non_interactive: bool,
    /// Dry run (plan only, don't execute)
    #[arg(long = "dry-run")]
    pub dry_run: bool,
    /// Emit answers template JSON (use with --dry-run)
    #[arg(long = "emit-answers")]
    pub emit_answers: Option<PathBuf>,
    /// Advanced mode — show all questions including optional ones
    #[arg(long = "advanced")]
    pub advanced: bool,
}

#[derive(Args, Debug, Clone)]
pub struct BundleRemoveArgs {
    /// Provider ID to remove
    #[arg(value_name = "PROVIDER_ID")]
    pub provider_id: String,
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    pub tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Force removal without confirmation
    #[arg(long = "force", short = 'f')]
    pub force: bool,
}

#[derive(Args, Debug, Clone)]
pub struct BundleBuildArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Output directory for portable bundle
    #[arg(long = "out", short = 'o')]
    pub out: PathBuf,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't')]
    pub tenant: Option<String>,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Only include used providers
    #[arg(long = "only-used-providers")]
    pub only_used_providers: bool,
    /// Run doctor validation after build
    #[arg(long = "doctor")]
    pub doctor: bool,
    /// Skip doctor validation
    #[arg(long = "skip-doctor")]
    pub skip_doctor: bool,
}

#[derive(Args, Debug, Clone)]
pub struct BundleListArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Filter by domain (messaging/events/secrets/oauth)
    #[arg(long = "domain", short = 'd', default_value = "messaging")]
    pub domain: String,
    /// Show flows for a specific pack
    #[arg(long = "pack", short = 'p')]
    pub pack: Option<String>,
    /// Output format (text/json)
    #[arg(long = "format", default_value = "text")]
    pub format: String,
}

#[derive(Args, Debug, Clone)]
pub struct BundleStatusArgs {
    /// Bundle directory (default: current directory)
    #[arg(long = "bundle", short = 'b')]
    pub bundle: Option<PathBuf>,
    /// Output format (text/json)
    #[arg(long = "format", default_value = "text")]
    pub format: String,
}
