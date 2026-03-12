# greentic-setup

End-to-end bundle setup engine for the Greentic platform.

Provides a library and CLI for discovering, configuring, and deploying Greentic bundles — including pack resolution, QA-driven setup (via greentic-qa), secrets persistence, webhook registration, admin API types, hot reload, and adaptive card setup flows.

**Dual-mode:**
- **Library** — core APIs for programmatic use by greentic-operator, runner, and other tools
- **CLI binary** — `greentic-setup bundle ...` commands for bundle lifecycle management (invoked via `gtc setup` passthrough)

## Features

- **Setup engine** — plan and execute create/update/remove workflows for bundles
- **Pack discovery** — scan bundle directories for `.gtpack` files, read CBOR/JSON manifests
- **QA-driven configuration** — interactive CLI prompts and adaptive card rendering (via greentic-qa FormSpec)
- **Conditional questions** — `visible_if` expressions for dynamic form flows (powered by qa-spec `Expr` evaluation)
- **Secrets management** — persist secrets and config to dev store for all pack types
- **Admin API types** — mTLS-secured request/response types for runtime bundle lifecycle management
- **Hot reload** — diff-based bundle change detection and reload planning
- **Adaptive card setup** — session management and link generation for card-based onboarding
- **Full i18n** — all user-facing strings via greentic-i18n

## Architecture

```
greentic-setup
├── bin/
│   └── greentic_setup  CLI binary (simple mode + bundle subcommands)
├── cli_i18n        CLI localization helper (CliI18n wrapper)
├── engine          SetupEngine: plan → execute orchestration
├── plan            SetupPlan, SetupStep, SetupMode, metadata types
├── bundle          Bundle directory creation, gmap paths, provider registry
├── bundle_source   Pack source resolution (file://, oci://, path)
├── discovery       Pack discovery from .gtpack files (CBOR + JSON manifests)
├── gtbundle        Portable .gtbundle archive format (zip/squashfs)
├── qa/
│   ├── bridge      Provider QA JSON → FormSpec conversion (+ visible_if)
│   ├── wizard      Interactive wizard, validation, card rendering
│   └── persist     Secrets + config persistence to dev store
├── secrets         Dev store path resolution, SecretsSetup
├── setup_input     Setup answers loading from JSON/YAML files
├── setup_to_formspec  Legacy setup.yaml → FormSpec conversion
├── secret_name     Canonical secret name/URI normalization
├── admin/
│   ├── tls         AdminTlsConfig for mTLS endpoint
│   └── routes      Admin API request/response types
├── reload          BundleDiff, ReloadPlan, ReloadAction
├── card_setup      CardSetupSession, SetupLinkConfig
└── webhook         Webhook URL validation helpers
```

Previously embedded in greentic-operator (~5,000 lines). Extracted as a standalone library so it can be reused by the operator, runner, CLI tools, and admin APIs.

## Usage

### Simple Mode (recommended)

The simplest way to use greentic-setup — just point it at a bundle:

```bash
# Interactive wizard - prompts for all configuration
greentic-setup ./my-bundle

# Preview what will happen (dry-run)
greentic-setup --dry-run ./my-bundle

# Generate answers template file
greentic-setup --dry-run --emit-answers answers.yaml ./my-bundle

# Apply answers from file (non-interactive)
greentic-setup --answers answers.yaml ./my-bundle

# Works with .gtbundle archives too
greentic-setup ./my-bundle.gtbundle
```

**Options:**
| Flag | Description |
|------|-------------|
| `--dry-run` | Preview setup plan without executing |
| `--emit-answers <FILE>` | Generate answers template to file |
| `-a, --answers <FILE>` | Apply answers from file |
| `-t, --tenant <TENANT>` | Tenant identifier (default: demo) |
| `--team <TEAM>` | Team identifier |
| `-e, --env <ENV>` | Environment: dev/staging/prod (default: dev) |

### Via gtc passthrough

```bash
gtc setup ./my-bundle
gtc setup --answers answers.yaml ./my-bundle
gtc setup bundle status --bundle ./my-bundle
```

### Advanced: Bundle Subcommands

For fine-grained control over bundle lifecycle:

```bash
# Initialize a new bundle
greentic-setup bundle init ./my-bundle --name "My Bundle"

# Add a pack to the bundle
greentic-setup bundle add telegram-pack.gtpack --bundle ./my-bundle

# Interactive setup (wizard mode)
greentic-setup bundle setup --bundle ./my-bundle

# Setup with answers file
greentic-setup bundle setup --bundle ./my-bundle --answers answers.yaml

# Generate answers template
greentic-setup bundle setup --bundle ./my-bundle --emit-answers answers.yaml

# Setup specific provider
greentic-setup bundle setup messaging-telegram --bundle ./my-bundle --answers telegram.yaml

# Update provider configuration
greentic-setup bundle update messaging-telegram --bundle ./my-bundle --answers telegram.yaml

# Remove a provider
greentic-setup bundle remove messaging-telegram --bundle ./my-bundle --force

# Build portable bundle (.gtbundle archive)
greentic-setup bundle build --bundle ./my-bundle --out ./dist

# List packs in bundle
greentic-setup bundle list --bundle ./my-bundle --domain messaging

# Show bundle status
greentic-setup bundle status --bundle ./my-bundle --format json
```

### As a library

```rust
use greentic_setup::{SetupEngine, SetupMode};
use greentic_setup::engine::{SetupRequest, SetupConfig};
use greentic_setup::plan::TenantSelection;
use std::path::PathBuf;

let config = SetupConfig {
    tenant: "demo".to_string(),
    team: Some("default".to_string()),
    env: "dev".to_string(),
    offline: false,
    verbose: true,
};

let engine = SetupEngine::new(config);

let request = SetupRequest {
    bundle: PathBuf::from("./demo-bundle"),
    bundle_name: Some("My Bundle".to_string()),
    pack_refs: vec!["oci://ghcr.io/greentic-ai/telegram:latest".to_string()],
    tenants: vec![TenantSelection {
        tenant: "demo".to_string(),
        team: Some("default".to_string()),
        allow_paths: vec!["packs/messaging-telegram".to_string()],
    }],
    // ... other fields default
    ..Default::default()
};

let plan = engine.plan(SetupMode::Create, &request, false).unwrap();
engine.print_plan(&plan);
```

### QA-driven setup

```rust
use greentic_setup::qa::wizard::run_qa_setup;
use std::path::Path;

let (answers, form_spec) = run_qa_setup(
    Path::new("providers/messaging/messaging-telegram.gtpack"),
    "messaging-telegram",
    None,       // no pre-loaded answers
    true,       // interactive
    None,       // no pre-built FormSpec
).unwrap();
```

### Bundle-level static hosting policy

Answers files can now carry bundle/platform static hosting policy separately from provider answers:

```yaml
bundle_source: ./my-bundle
env: dev
tenant: demo
platform_setup:
  static_routes:
    public_web_enabled: false
    public_surface_policy: disabled
    default_route_prefix_policy: pack_declared
    tenant_path_policy: pack_declared
setup_answers:
  messaging-telegram:
    bot_token: "your-bot-token"
```

When setup executes, it persists the normalized bundle-level artifact to
`state/config/platform/static-routes.json`.

### Pack discovery

```rust
use greentic_setup::discovery;
use std::path::Path;

let result = discovery::discover(Path::new("./demo-bundle")).unwrap();
println!("Found {} providers", result.providers.len());
for p in &result.providers {
    println!("  {} ({}) @ {}", p.provider_id, p.domain, p.pack_path.display());
}
```

### Hot reload diffing

```rust
use greentic_setup::reload::{diff_discoveries, plan_reload};
use std::path::Path;

let prev = discovery::discover(Path::new("./bundle-v1")).unwrap();
let curr = discovery::discover(Path::new("./bundle-v2")).unwrap();
let diff = diff_discoveries(&prev, &curr);

if !diff.is_empty() {
    let plan = plan_reload(Path::new("./bundle-v2"), &diff);
    println!("{} reload actions needed", plan.actions.len());
}
```

### Admin API types

```rust
use greentic_setup::admin::{AdminTlsConfig, BundleDeployRequest, AdminResponse};

let tls = AdminTlsConfig {
    server_cert: "certs/server.crt".into(),
    server_key: "certs/server.key".into(),
    client_ca: "certs/ca.crt".into(),
    allowed_clients: vec!["CN=greentic-admin".into()],
    port: 8443,
};
tls.validate().unwrap();
```

### From CLI (via greentic-operator)

```bash
# Interactive setup
gtc op demo wizard --execute

# Automated setup from answers file
gtc op demo start --setup-input answers.json

# QA setup wizard
gtc op demo setup-wizard
```

## Modules

| Module | Description |
|--------|-------------|
| `engine` | `SetupEngine` — orchestrates plan building for create/update/remove |
| `plan` | Plan types: `SetupPlan`, `SetupStep`, `SetupMode`, metadata |
| `bundle` | Bundle directory structure creation and management |
| `bundle_source` | Pack source resolution (`file://`, `oci://`, path) |
| `discovery` | Pack discovery from `.gtpack` files (CBOR + JSON manifests) |
| `gtbundle` | Portable `.gtbundle` archive format (zip/squashfs) |
| `cli_i18n` | CLI localization helper for user-facing messages |
| `qa::bridge` | Provider QA JSON → FormSpec conversion with `visible_if` support |
| `qa::wizard` | Interactive wizard, validation, card rendering, visibility evaluation |
| `qa::persist` | Secrets + config persistence (visibility-aware) |
| `secrets` | Dev store path resolution, `SecretsSetup` |
| `setup_input` | Setup answers loading from JSON/YAML |
| `setup_to_formspec` | Legacy `setup.yaml` → FormSpec conversion |
| `secret_name` | Canonical secret name normalization |
| `admin::tls` | `AdminTlsConfig` for mTLS endpoints |
| `admin::routes` | Admin API request/response types |
| `reload` | `BundleDiff`, `ReloadPlan`, `ReloadAction` for hot reload |
| `card_setup` | `CardSetupSession`, `SetupLinkConfig` for adaptive card flows |
| `webhook` | Webhook URL validation helpers |

## Secret URI Format

```
secrets://{env}/{tenant}/{team}/{provider_id}/{key}
          dev    demo      _    messaging-telegram  bot_token
```

- Team: `None` / `"default"` / empty → `"_"` (wildcard)
- Key: normalized via `canonical_secret_name()` (lowercase, underscores, collapsed)

## Conditional Questions (`visible_if`)

Provider QA specs can include `visible_if` expressions:

```json
{
  "id": "redis_password",
  "label": "Redis Password",
  "required": true,
  "secret": true,
  "visible_if": {"field": "redis_auth_enabled", "eq": "true"}
}
```

Supported formats:
- `{"field": "q1", "eq": "value"}` — equality check
- `{"field": "q1"}` — truthy check
- Full qa-spec `Expr` JSON (And/Or/Not/Eq/Ne/Lt/Gt/IsSet)

Invisible questions are:
- Skipped during interactive prompts
- Skipped during validation (required check)
- Not persisted as secrets

## CI and Releases

### Local checks

```bash
bash ci/local_check.sh
```

Runs: fmt → clippy → test → build → doc → package dry-run.

### Cutting a release

1. Bump version in `Cargo.toml`
2. Commit and tag: `git tag v0.4.x`
3. Push tag: `git push origin v0.4.x`
4. `publish.yml` workflow triggers → publishes to crates.io

**Required GitHub secrets:** `CARGO_REGISTRY_TOKEN`

### i18n

```bash
tools/i18n.sh all        # translate + validate + status
tools/i18n.sh translate  # generate translations (200 per batch)
```

Requires `greentic-i18n` repo at `../greentic-i18n/`.

## Documentation

| Document | Description |
|----------|-------------|
| **[Demo Guide](./docs/demo-guide.md)** | Complete guide for creating and running bundles |
| **[Admin API Reference](./docs/admin-api.md)** | Full admin endpoint documentation with examples |
| **[Adaptive Cards Guide](./docs/adaptive-cards.md)** | Card-based setup flow with security details |
| **[mTLS Setup Guide](./docs/mtls-setup.md)** | Certificate generation and configuration |
| **[Demo Features](./DEMO_SETUP_FEATURES.md)** | Advanced features demo (conditional QA, secrets, hot reload) |
| **[Manual Testing](./docs/manual-testing.md)** | Step-by-step testing guide for all features |

### API Reference (rustdoc)

```bash
cargo doc --open
```

## License

MIT
