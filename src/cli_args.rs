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

  Deploy a bundle into an environment:
    greentic-setup env-deploy ./my-bundle.gtbundle
    greentic-setup env-deploy ./my-bundle.gtbundle --env staging
    greentic-setup env-deploy --dry-run ./my-bundle.gtbundle

  Add a messaging provider to an environment:
    greentic-setup provider add telegram
    greentic-setup provider add slack --env staging
    greentic-setup provider add telegram --answers answers.json --non-interactive
    greentic-setup provider list
    greentic-setup provider remove <endpoint-id>

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

    /// Encryption/decryption key for answer documents that include secrets
    #[arg(long = "key", value_name = "KEY", global = true)]
    pub key: Option<String>,

    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo", global = true)]
    pub tenant: String,

    /// Team identifier
    #[arg(long = "team", global = true)]
    pub team: Option<String>,

    /// Environment (defaults to `local`; legacy `dev` remapped via the A4b
    /// compat alias with a once-per-process warning until removal).
    #[arg(long = "env", short = 'e', default_value = "local", global = true)]
    pub env: String,

    /// UI locale (BCP-47 tag, e.g., en, ja, id)
    #[arg(long = "locale", global = true)]
    pub locale: Option<String>,

    /// Advanced mode — show all questions including optional ones
    #[arg(long = "advanced", global = true)]
    pub advanced: bool,

    /// Launch web-based setup UI in browser (enabled by default).
    /// Use --no-ui to disable the UI; stdin prompts may still be used.
    #[arg(long = "ui", global = true, default_value_t = true)]
    pub ui: bool,

    /// Disable web UI; stdin prompts may still be used.
    #[arg(long = "no-ui", global = true)]
    pub no_ui: bool,

    /// Strict non-interactive mode: no prompts, fail if answers incomplete
    #[arg(long = "non-interactive", global = true)]
    pub non_interactive: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Diagnose bundle setup inputs and generated setup outputs
    Doctor(DoctorArgs),
    /// Deploy a bundle into an environment via the env-apply engine
    EnvDeploy(EnvDeployArgs),
    /// Manage messaging providers in an environment
    #[command(subcommand)]
    Provider(ProviderCommand),
    /// Bundle lifecycle management (advanced)
    #[command(subcommand)]
    Bundle(Box<BundleCommand>),
}

#[derive(Args, Debug, Clone)]
pub struct EnvDeployArgs {
    /// Bundle path (.gtbundle file or bundle directory)
    #[arg(value_name = "BUNDLE")]
    pub bundle: PathBuf,
}

#[derive(Args, Debug, Clone)]
pub struct DoctorArgs {
    /// Bundle path (.gtbundle file or directory)
    #[arg(value_name = "BUNDLE")]
    pub bundle: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<DoctorCommand>,
    /// Emit stable machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
    /// Treat warnings as command failures
    #[arg(long = "strict")]
    pub strict: bool,
    /// Include fix hints in human-readable output
    #[arg(long = "fix-hints")]
    pub fix_hints: bool,
    /// Show informational diagnostics in human-readable output
    #[arg(long = "show-info")]
    pub show_info: bool,
    /// Limit checks to one stage
    #[arg(long = "stage", value_enum)]
    pub stage: Option<DoctorStageArg>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum DoctorCommand {
    /// Validate a provider pack's setup contract
    Provider(DoctorProviderArgs),
}

#[derive(Args, Debug, Clone)]
pub struct DoctorProviderArgs {
    /// Provider pack path (.gtpack)
    #[arg(value_name = "PACK")]
    pub pack: PathBuf,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorStageArg {
    Setup,
    Cache,
    Locks,
    Answers,
    Runtime,
    Routes,
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
    /// Show persisted generic provider setup status
    SetupStatus(BundleSetupStatusArgs),
    /// Inspect and record the next generic provider setup step
    SetupNext(BundleSetupNextArgs),
    /// Clear retry-blocking state for a generic provider setup step
    SetupRetry(BundleSetupRetryArgs),
    /// Reset persisted generic provider setup state
    SetupReset(BundleSetupResetArgs),
    /// Migrate legacy provider setup state into the generic setup state layout
    SetupMigrate(BundleSetupMigrateArgs),
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
    /// Environment (defaults to `local`; legacy `dev` remapped via the A4b
    /// compat alias with a once-per-process warning until removal).
    #[arg(long = "env", short = 'e', default_value = "local")]
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
    /// Encryption/decryption key for answer documents that include secrets
    #[arg(long = "key", value_name = "KEY")]
    pub key: Option<String>,
    /// Tenant identifier
    #[arg(long = "tenant", short = 't', default_value = "demo")]
    pub tenant: String,
    /// Team identifier
    #[arg(long = "team")]
    pub team: Option<String>,
    /// Environment (defaults to `local`; legacy `dev` remapped via the A4b
    /// compat alias with a once-per-process warning until removal).
    #[arg(long = "env", short = 'e', default_value = "local")]
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
    /// Populated from the global --non-interactive flag before dispatch.
    #[arg(skip)]
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
pub struct BundleSetupStatusArgs {
    /// Provider ID to inspect
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
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Output format: text or json
    #[arg(long = "format", default_value = "text")]
    pub format: String,
}

#[derive(Args, Debug, Clone)]
pub struct BundleSetupNextArgs {
    /// Provider ID to advance
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
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Output format: text or json
    #[arg(long = "format", default_value = "text")]
    pub format: String,
    /// Only report the next action; do not write state or events
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

#[derive(Args, Debug, Clone)]
pub struct BundleSetupRetryArgs {
    /// Provider ID to retry
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
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Optional step to retry; defaults to the last recorded setup step
    #[arg(long = "step")]
    pub step: Option<String>,
    /// Emit stable machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct BundleSetupResetArgs {
    /// Provider ID to reset
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
    /// Confirm destructive reset of setup progress
    #[arg(long = "yes")]
    pub yes: bool,
    /// Emit stable machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct BundleSetupMigrateArgs {
    /// Provider ID to migrate
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
    /// Environment (dev/staging/prod)
    #[arg(long = "env", short = 'e', default_value = "dev")]
    pub env: String,
    /// Emit stable machine-readable JSON
    #[arg(long = "json")]
    pub json: bool,
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

// --- Provider subcommands ---------------------------------------------------

#[derive(Subcommand, Debug, Clone)]
pub enum ProviderCommand {
    /// Add a messaging provider to an environment
    Add(ProviderAddArgs),
    /// List messaging providers in an environment
    List(ProviderListArgs),
    /// Remove a messaging provider from an environment
    Remove(ProviderRemoveArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ProviderAddArgs {
    /// Provider kind (telegram, slack, webex, teams)
    #[arg(value_name = "KIND")]
    pub kind: String,
    /// Bundle id to link (auto-detected when the env has exactly one bundle)
    #[arg(long = "bundle-id")]
    pub bundle_id: Option<String>,
    /// Local .gtpack file override (skips OCI fetch)
    #[arg(long = "pack")]
    pub pack: Option<PathBuf>,
    /// OCI tag override (e.g. a specific version like "0.5.6"). Only affects
    /// the OCI reference; ignored when --pack is given.
    #[arg(long = "pack-version")]
    pub pack_version: Option<String>,
    /// Provider instance id (defaults to the kind name)
    #[arg(long = "provider-id")]
    pub provider_id: Option<String>,
    /// Human-readable display name for the endpoint
    #[arg(long = "display-name")]
    pub display_name: Option<String>,
}

#[derive(Args, Debug, Clone)]
pub struct ProviderListArgs {}

#[derive(Args, Debug, Clone)]
pub struct ProviderRemoveArgs {
    /// Endpoint id to remove (from `provider list`)
    #[arg(value_name = "ENDPOINT_ID")]
    pub endpoint_id: String,
}
