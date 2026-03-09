# Plan: `gtc setup ./bundle`

> End-to-end bundle setup via greentic-setup CLI, replacing operator's embedded setup logic.

## Command Design

```
gtc setup <BUNDLE_SOURCE> [OPTIONS]

BUNDLE_SOURCE:
  ./path/to/bundle.gtbundle     Local directory (absolute or relative)
  file:///absolute/path.gtbundle Local via file URI
  oci://ghcr.io/org/bundle:tag  OCI registry (via greentic-distributor-client)
  repo://org/bundle-name        Pack repository (via greentic-distributor-client) [placeholder]
  store://bundle-id             Component store (via greentic-distributor-client) [placeholder]

OPTIONS:
  --answers <PATH>         Load answers JSON (from --emit-answers or manual)
  --dry-run                Plan only, no execution
  --emit-answers <PATH>    Generate answers JSON template (combine with --dry-run)
  --execute                Execute setup plan immediately
  --tenant <ID>            Target tenant (default: "demo")
  --team <ID>              Target team (default: none)
  --env <ENV>              Environment (default: GREENTIC_ENV or "dev")
  --locale <LANG>          UI locale (default: system locale)
  --verbose                Show step details
  --offline                Skip remote registry lookups
```

### Example Flows

```bash
# Interactive setup
gtc setup ./my.gtbundle

# Generate answers template (dry-run)
gtc setup ./my.gtbundle --dry-run --emit-answers answers.json

# Fill answers, then automated setup
gtc setup ./my.gtbundle --answers answers.json --execute

# From OCI registry
gtc setup oci://ghcr.io/greentic-ai/demo-bundle:latest --answers answers.json --execute
```

---

## Current State vs Target

### What operator does today (to be replaced)

```
cli.rs (DemoWizardArgs)         ← parse CLI args
  │
  ├─ wizard_plan_builder.rs     ← build plan from request
  ├─ wizard.rs                  ← execute plan (create/update/remove)
  │   ├─ resolve_pack_refs()    ← fetch packs via distributor-client
  │   ├─ create_bundle()        ← create directory structure
  │   ├─ seed_setup_answers()   ← persist secrets
  │   └─ run_webhook_setup()    ← auto-register webhooks
  ├─ qa_setup_wizard.rs         ← interactive QA card wizard
  ├─ qa_persist.rs              ← persist secrets + config
  ├─ setup_input.rs             ← load --answers file
  ├─ secrets_setup.rs           ← seed dev secrets store
  └─ discovery.rs               ← discover packs in bundle
```

**Problem:** ~5,000 lines of setup logic embedded in greentic-operator, not reusable.

### Target: greentic-setup as standalone

```
greentic-setup (library + binary)
  │
  ├─ SetupEngine                ← orchestrator (replaces wizard.rs + cli.rs setup parts)
  │   ├─ discover()             ← pack discovery (app, extension, provider, capability)
  │   ├─ plan()                 ← build setup plan
  │   ├─ execute()              ← execute plan
  │   └─ validate()             ← validate bundle + answers
  │
  ├─ BundleSource               ← resolve bundle from any source
  │   ├─ from_path()            ← local directory
  │   ├─ from_file_uri()        ← file:// URI
  │   ├─ from_oci()             ← oci:// via distributor-client
  │   ├─ from_repo()            ← repo:// [placeholder]
  │   └─ from_store()           ← store:// [placeholder]
  │
  ├─ QaEngine                   ← answer collection (replaces qa_setup_wizard.rs)
  │   ├─ interactive()          ← CLI prompts + adaptive cards
  │   ├─ from_answers()         ← load from --answers JSON
  │   └─ emit_answers()         ← generate answers template
  │
  ├─ SecretsPersistence         ← replaces qa_persist.rs + secrets_setup.rs
  │   ├─ persist_secrets()
  │   ├─ persist_config()
  │   └─ seed_requirements()
  │
  └─ I18n                       ← all user-facing strings via greentic-i18n
```

**greentic-operator becomes:**
```rust
// cli.rs - thin wrapper
fn run_setup(args: SetupArgs) -> Result<()> {
    let engine = greentic_setup::SetupEngine::new(args.into())?;
    let plan = engine.plan()?;
    if args.dry_run {
        engine.print_plan(&plan);
        if let Some(path) = args.emit_answers {
            engine.emit_answers(&plan, &path)?;
        }
        return Ok(());
    }
    engine.execute(&plan)?;
    Ok(())
}
```

---

## Implementation Phases

### Phase 1: Crate scaffold + BundleSource

**Goal:** Create greentic-setup crate, implement bundle source resolution.

#### 1.1 Crate structure

```
greentic-setup/
├── Cargo.toml
├── src/
│   ├── lib.rs              ← pub mod exports
│   ├── bundle_source.rs    ← BundleSource enum + resolution
│   ├── engine.rs           ← SetupEngine (stub)
│   ├── plan.rs             ← SetupPlan, SetupStep, SetupMode
│   ├── discovery.rs        ← Pack discovery (from operator/discovery.rs)
│   ├── qa/
│   │   ├── mod.rs
│   │   ├── engine.rs       ← QaEngine
│   │   ├── bridge.rs       ← provider QA → FormSpec
│   │   ├── persist.rs      ← secrets + config persistence
│   │   ├── input.rs        ← answers file loader
│   │   └── wizard.rs       ← interactive wizard
│   ├── secrets.rs          ← SecretsSetup
│   ├── webhook.rs          ← webhook auto-setup
│   └── i18n.rs             ← i18n wrapper (all user strings)
├── tests/
│   ├── bundle_source_test.rs
│   ├── plan_test.rs
│   └── integration_test.rs
└── i18n/
    └── en.json             ← English strings
```

#### 1.2 Cargo.toml

```toml
[package]
name = "greentic-setup"
version = "0.4.0"
edition = "2024"

[features]
default = ["oci"]
oci = ["greentic-distributor-client/dist-client"]

[dependencies]
greentic-distributor-client = { version = "0.4", optional = true }
greentic-i18n = "0.4"
greentic-qa-lib = "0.4"
greentic-secrets-provider-dev = "0.4"
greentic-types = "0.4"
greentic-pack-lib = "0.4"
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
tracing = "0.1"
zip = { version = "8", default-features = false, features = ["deflate-flate2"] }
```

#### 1.3 BundleSource implementation

```rust
// src/bundle_source.rs

pub enum BundleSource {
    /// Local directory path
    LocalDir(PathBuf),
    /// file:// URI → local path
    FileUri(PathBuf),
    /// oci://registry/repo:tag
    Oci { reference: String },
    /// repo://org/name (placeholder)
    Repo { reference: String },
    /// store://id (placeholder)
    Store { reference: String },
}

impl BundleSource {
    /// Parse a bundle source string into the appropriate variant.
    pub fn parse(source: &str) -> Result<Self> {
        if source.starts_with("oci://") {
            Ok(Self::Oci { reference: source.to_string() })
        } else if source.starts_with("repo://") {
            Ok(Self::Repo { reference: source.to_string() })
        } else if source.starts_with("store://") {
            Ok(Self::Store { reference: source.to_string() })
        } else if source.starts_with("file://") {
            let path = url_to_path(source)?;
            Ok(Self::FileUri(path))
        } else {
            // Treat as local path
            let path = PathBuf::from(source);
            Ok(Self::LocalDir(path))
        }
    }

    /// Resolve the source to a local directory path.
    /// For remote sources, fetches and extracts to a local cache.
    pub fn resolve(&self) -> Result<PathBuf> {
        match self {
            Self::LocalDir(path) | Self::FileUri(path) => {
                if !path.exists() {
                    anyhow::bail!("bundle path does not exist: {}", path.display());
                }
                Ok(path.clone())
            }
            Self::Oci { reference } => {
                // Use greentic-distributor-client to fetch
                resolve_oci_bundle(reference)
            }
            Self::Repo { reference } => {
                anyhow::bail!("repo:// protocol not yet implemented: {reference}")
            }
            Self::Store { reference } => {
                anyhow::bail!("store:// protocol not yet implemented: {reference}")
            }
        }
    }
}
```

#### 1.4 Tasks

| Task | Description | Source |
|------|-------------|--------|
| 1.4.1 | Create crate scaffold (Cargo.toml, lib.rs, modules) | new |
| 1.4.2 | Implement `BundleSource::parse()` + `resolve()` | new |
| 1.4.3 | Implement OCI resolution via distributor-client | `wizard.rs:resolve_pack_refs()` |
| 1.4.4 | Port discovery logic | `operator/discovery.rs` |
| 1.4.5 | Add i18n JSON file + wrapper | new |
| 1.4.6 | Tests for BundleSource + discovery | new |

---

### Phase 2: SetupEngine + Plan

**Goal:** Port wizard plan/execute logic into SetupEngine.

#### 2.1 SetupPlan design

```rust
// src/plan.rs

pub enum SetupMode {
    Create,
    Update,
    Remove,
}

pub struct SetupPlan {
    pub mode: SetupMode,
    pub bundle_path: PathBuf,
    pub steps: Vec<SetupStep>,
    pub metadata: SetupMetadata,
}

pub struct SetupMetadata {
    pub tenants: Vec<TenantSelection>,
    pub packs: Vec<DiscoveredPack>,
    pub setup_answers: serde_json::Map<String, serde_json::Value>,
    pub capabilities: Vec<CapabilityRequirement>,
}

pub enum SetupStep {
    /// Discover all packs in bundle
    DiscoverPacks,
    /// Collect answers for pack setup (interactive or from file)
    CollectAnswers { pack_id: String },
    /// Persist secrets to dev store
    SeedSecrets { pack_id: String },
    /// Persist non-secret config
    WriteConfig { pack_id: String },
    /// Run pack's setup_default flow
    RunSetupFlow { pack_id: String },
    /// Register webhooks
    RegisterWebhooks { provider_id: String },
    /// Generate resolved manifest
    ResolveManifest,
    /// Validate bundle integrity
    ValidateBundle,
}
```

#### 2.2 SetupEngine

```rust
// src/engine.rs

pub struct SetupEngine {
    bundle_path: PathBuf,
    config: SetupConfig,
}

pub struct SetupConfig {
    pub tenant: String,
    pub team: Option<String>,
    pub env: String,
    pub locale: String,
    pub answers_path: Option<PathBuf>,
    pub offline: bool,
    pub verbose: bool,
}

impl SetupEngine {
    pub fn new(bundle: PathBuf, config: SetupConfig) -> Result<Self>;

    /// Discover packs and build a setup plan.
    pub fn plan(&self) -> Result<SetupPlan>;

    /// Execute a setup plan.
    pub fn execute(&self, plan: &SetupPlan) -> Result<SetupReport>;

    /// Validate bundle + answers without executing.
    pub fn validate(&self, plan: &SetupPlan) -> Result<Vec<ValidationWarning>>;

    /// Print plan to stderr (human-readable).
    pub fn print_plan(&self, plan: &SetupPlan);

    /// Emit answers template JSON.
    pub fn emit_answers(&self, plan: &SetupPlan, path: &Path) -> Result<()>;
}
```

#### 2.3 Pack categorization

Currently packs are discovered by directory location. Add explicit categorization:

```rust
pub enum PackCategory {
    /// App packs (flows, components) — in packs/
    App,
    /// Provider packs (messaging, events, etc.) — in providers/{domain}/
    Provider { domain: Domain },
    /// Extension packs (capabilities like state-redis, telemetry) — in packs/
    Extension,
    /// Validator packs — in validators/
    Validator { domain: Domain },
}
```

Discovery infers category from:
1. Pack manifest `pack_type` field (if present)
2. Directory location (providers/ → Provider, validators/ → Validator)
3. Capability declarations in manifest (has `cap_id` → Extension)
4. Default → App

#### 2.4 Tasks

| Task | Description | Source |
|------|-------------|--------|
| 2.4.1 | Define `SetupPlan` + `SetupStep` + `SetupMetadata` | new (from wizard.rs types) |
| 2.4.2 | Port `apply_create()` → `SetupEngine::plan()` | `wizard.rs:361-465` |
| 2.4.3 | Port `execute_create_plan()` → `SetupEngine::execute()` | `wizard.rs:930-1003` |
| 2.4.4 | Implement `emit_answers()` | `cli.rs:3084-3100` |
| 2.4.5 | Implement `print_plan()` with i18n | `wizard.rs:print_plan_summary()` |
| 2.4.6 | Add pack categorization | new |
| 2.4.7 | Tests for plan + execute | new |

---

### Phase 3: QA engine + answers

**Goal:** Port QA logic, support `--answers` and `--emit-answers` round-trip.

#### 3.1 QaEngine design

```rust
// src/qa/engine.rs

pub struct QaEngine {
    locale: String,
}

impl QaEngine {
    /// Load FormSpec from a pack's QA definition.
    pub fn load_spec(&self, pack_path: &Path) -> Result<Option<FormSpec>>;

    /// Collect answers interactively (CLI prompts or adaptive cards).
    pub fn collect_interactive(
        &self,
        spec: &FormSpec,
        existing: Option<&Value>,
    ) -> Result<Value>;

    /// Validate answers against spec (including visible_if conditions).
    pub fn validate(
        &self,
        spec: &FormSpec,
        answers: &Value,
    ) -> Result<Vec<ValidationError>>;

    /// Load answers from JSON file.
    pub fn load_answers(path: &Path) -> Result<AnswersDocument>;

    /// Emit answers template with all questions as keys.
    pub fn emit_template(
        &self,
        specs: &[(String, FormSpec)],  // (pack_id, spec)
    ) -> Result<AnswersDocument>;
}
```

#### 3.2 Answers JSON format (round-trip compatible)

```json
{
  "greentic_setup_version": "1.0.0",
  "bundle_source": "./my.gtbundle",
  "tenant": "demo",
  "team": null,
  "env": "dev",
  "setup_answers": {
    "messaging-telegram": {
      "bot_token": "",
      "public_base_url": "https://example.com"
    },
    "messaging-slack": {
      "bot_token": "",
      "app_token": "",
      "slack_app_id": "",
      "slack_configuration_token": "",
      "public_base_url": "https://example.com"
    },
    "state-redis": {
      "redis_url": "redis://localhost:6379/0",
      "key_prefix": "greentic",
      "default_ttl_seconds": 0
    },
    "telemetry-otlp": {
      "preset": "azure",
      "otlp_endpoint": "",
      "azure_connection_string": "",
      "sampling_ratio": "1.0"
    }
  }
}
```

**Round-trip guarantee:**
1. `gtc setup ./bundle --dry-run --emit-answers template.json` → generates template with all fields
2. User fills in values
3. `gtc setup ./bundle --answers template.json --execute` → uses filled values

#### 3.3 Conditional question support

Wire greentic-qa's existing `visible_if`:

```rust
// In qa/engine.rs::collect_interactive()

fn collect_interactive(&self, spec: &FormSpec, existing: Option<&Value>) -> Result<Value> {
    let mut answers = existing.cloned().unwrap_or(json!({}));

    loop {
        // Compute visibility based on current answers
        let visibility = qa_spec::resolve_visibility(spec, &answers, VisMode::Edit);

        // Render next card (skips answered + invisible questions)
        let (card, next_id) = qa_spec::build_render_payload(spec, &answers, &visibility)?;

        if next_id.is_none() {
            break; // All questions answered
        }

        // Show card, collect answer
        let answer = self.prompt_from_card(&card)?;
        merge_answer(&mut answers, &answer);
    }

    Ok(answers)
}
```

#### 3.4 Tasks

| Task | Description | Source |
|------|-------------|--------|
| 3.4.1 | Port `QaEngine` from `qa_setup_wizard.rs` | `qa_setup_wizard.rs:30-69` |
| 3.4.2 | Port `qa/bridge.rs` (provider QA → FormSpec) | `qa_bridge.rs` |
| 3.4.3 | Port `qa/persist.rs` | `qa_persist.rs` |
| 3.4.4 | Port `qa/input.rs` (answers loader) | `setup_input.rs` |
| 3.4.5 | Implement `emit_template()` for `--emit-answers` | new |
| 3.4.6 | Wire `visible_if` in interactive collection | new |
| 3.4.7 | Implement secret masking in CLI prompts | partial (rpassword exists) |
| 3.4.8 | Tests for QA round-trip | new |

---

### Phase 4: Secrets persistence

**Goal:** Port secrets setup, ensure ALL pack types get secrets persisted.

#### 4.1 Fix: persist secrets for ALL packs (not just domain providers)

Current bug: wizard only persists domain provider secrets (see `wizard.rs:seed_setup_answers`).
Capability packs (state-redis, telemetry-otlp) are skipped.

**Fix in greentic-setup:** `SetupEngine::execute()` iterates ALL packs with setup_answers, regardless of domain.

```rust
// In engine.rs::execute()
for (pack_id, answers) in &plan.metadata.setup_answers {
    // Persist for ALL packs — no domain filtering
    self.secrets.persist_all(bundle, env, tenant, team, pack_id, answers, pack_path)?;
}
```

#### 4.2 Tasks

| Task | Description | Source |
|------|-------------|--------|
| 4.2.1 | Port `SecretsPersistence` from `secrets_setup.rs` + `qa_persist.rs` | both files |
| 4.2.2 | Ensure ALL packs get secrets persisted | fix current bug |
| 4.2.3 | Port `seed_secret_requirement_aliases()` | `qa_persist.rs:215-268` |
| 4.2.4 | Tests for secrets persistence | new |

---

### Phase 5: i18n

**Goal:** All user-facing strings go through greentic-i18n.

#### 5.1 i18n string catalog

Create `greentic-setup/i18n/en.json`:

```json
{
  "setup.discovering_packs": "Discovering packs in bundle...",
  "setup.found_packs": "Found {count} pack(s): {ids}",
  "setup.plan.step.discover": "Discover packs",
  "setup.plan.step.collect_answers": "Collect answers for {pack_id}",
  "setup.plan.step.seed_secrets": "Seed secrets for {pack_id}",
  "setup.plan.step.write_config": "Write config for {pack_id}",
  "setup.plan.step.run_setup": "Run setup flow for {pack_id}",
  "setup.plan.step.webhooks": "Register webhooks for {provider_id}",
  "setup.plan.step.resolve": "Generate resolved manifest",
  "setup.plan.step.validate": "Validate bundle",
  "setup.execute.success": "Setup complete: {bundle}",
  "setup.execute.failed": "Setup failed: {error}",
  "setup.answers.emitted": "Answers template written to {path}",
  "setup.answers.loaded": "Loaded answers from {path}",
  "setup.dry_run.header": "Setup plan (dry-run):",
  "setup.error.bundle_not_found": "Bundle not found: {path}",
  "setup.error.protocol_not_supported": "Protocol not yet supported: {protocol}"
}
```

#### 5.2 tools/i18n.sh integration

```bash
# In greentic-setup/tools/i18n.sh
EN_PATH=i18n/en.json
# Running `tools/i18n.sh translate` will use greentic-i18n-translator
# to generate es.json, de.json, fr.json, etc.
```

#### 5.3 Tasks

| Task | Description |
|------|-------------|
| 5.3.1 | Create `i18n/en.json` with all user-facing strings |
| 5.3.2 | Create `src/i18n.rs` wrapper using `greentic_i18n::resolve_message()` |
| 5.3.3 | Replace all hardcoded strings in engine/qa with i18n calls |
| 5.3.4 | Add `tools/i18n.sh` for translation generation |
| 5.3.5 | Run `tools/i18n.sh translate` to generate initial translations |

---

### Phase 6: Operator passthrough

**Goal:** greentic-operator delegates ALL setup to greentic-setup.

#### 6.1 Operator changes

```rust
// cli.rs — replace wizard implementation

// Before (current):
impl DemoWizardArgs {
    fn run(&self) -> Result<()> {
        // 500+ lines of setup logic
    }
}

// After (passthrough):
impl DemoWizardArgs {
    fn run(&self) -> Result<()> {
        let source = BundleSource::parse(&self.bundle)?;
        let config = SetupConfig {
            tenant: self.tenant.clone(),
            team: self.team.clone(),
            env: resolve_env(None),
            locale: self.locale.clone(),
            answers_path: self.answers.clone(),
            offline: self.offline,
            verbose: self.verbose,
        };
        let engine = greentic_setup::SetupEngine::new(source.resolve()?, config)?;
        let plan = engine.plan()?;

        if self.dry_run {
            engine.print_plan(&plan);
            if let Some(path) = &self.emit_answers {
                engine.emit_answers(&plan, path)?;
            }
            return Ok(());
        }

        if self.execute || self.apply {
            engine.execute(&plan)?;
        }
        Ok(())
    }
}
```

#### 6.2 Files to delete from operator (after migration)

| File | Lines | Replacement |
|------|-------|-------------|
| `wizard.rs` | 2,801 | `greentic_setup::engine` |
| `wizard_plan_builder.rs` | ~200 | `greentic_setup::plan` |
| `qa_setup_wizard.rs` | 612 | `greentic_setup::qa::wizard` |
| `qa_persist.rs` | 424 | `greentic_setup::qa::persist` |
| `setup_input.rs` | 391 | `greentic_setup::qa::input` |
| `secrets_setup.rs` | 160 | `greentic_setup::secrets` |
| `demo/qa_bridge.rs` | 356 | `greentic_setup::qa::bridge` |
| **Total** | **~4,944** | |

#### 6.3 Files to keep in operator (thin wrappers)

| File | Purpose |
|------|---------|
| `cli.rs` | CLI arg parsing → delegates to greentic-setup |
| `onboard/api.rs` | HTTP handlers → delegates to greentic-setup |
| `capability_bootstrap.rs` | Operator-specific bootstrap (telemetry, state-redis) |
| `discovery.rs` | May keep for runtime discovery (or import from greentic-setup) |

#### 6.4 Tasks

| Task | Description |
|------|-------------|
| 6.4.1 | Add `greentic-setup` dependency to operator's Cargo.toml |
| 6.4.2 | Replace `DemoWizardArgs::run()` with passthrough |
| 6.4.3 | Replace `DemoSetupArgs::run()` with passthrough |
| 6.4.4 | Replace `DemoSetupWizardArgs::run()` with passthrough |
| 6.4.5 | Update onboard API to use greentic-setup |
| 6.4.6 | Delete migrated files |
| 6.4.7 | Ensure all existing tests pass |
| 6.4.8 | Update CLAUDE.md with new architecture |

---

## Implementation Order

```
Phase 1: Crate scaffold + BundleSource   ██████░░░░░░░░░░░░░░  ~3 days
Phase 2: SetupEngine + Plan              ████████████░░░░░░░░  ~5 days
Phase 3: QA engine + answers             ████████░░░░░░░░░░░░  ~4 days
Phase 4: Secrets persistence             ████░░░░░░░░░░░░░░░░  ~2 days
Phase 5: i18n                            ████░░░░░░░░░░░░░░░░  ~2 days
Phase 6: Operator passthrough            ██████░░░░░░░░░░░░░░  ~3 days
                                                        Total: ~19 days
```

### Dependencies

```
Phase 1 ──→ Phase 2 (engine needs bundle source)
Phase 2 ──→ Phase 3 (engine calls QA engine)
Phase 3 ──→ Phase 4 (QA outputs → secrets persistence)
Phase 1-4 ─→ Phase 5 (i18n wraps all user strings)
Phase 1-5 ─→ Phase 6 (operator delegates to complete greentic-setup)
```

---

## Testing Strategy

| Level | Scope | Tool |
|-------|-------|------|
| Unit | BundleSource parsing, plan building | `cargo test` |
| Unit | QA round-trip (emit → load → validate) | `cargo test` + `insta` snapshots |
| Unit | Secrets persistence (mock DevStore) | `cargo test` |
| Integration | Full `gtc setup ./bundle --answers` flow | `assert_cmd` |
| Integration | OCI bundle resolution | `cargo test` (with mock registry) |
| Regression | Existing operator `demo wizard` still works | `cargo test -p greentic-operator` |

---

## Migration Checklist

- [ ] greentic-setup crate compiles independently
- [ ] `gtc setup ./bundle --dry-run --emit-answers` generates valid template
- [ ] `gtc setup ./bundle --answers filled.json --execute` works end-to-end
- [ ] ALL pack types get secrets persisted (including capability packs)
- [ ] Conditional questions (visible_if) work in interactive mode
- [ ] i18n strings in en.json, tools/i18n.sh generates translations
- [ ] greentic-operator `demo wizard` delegates to greentic-setup
- [ ] greentic-operator `demo setup` delegates to greentic-setup
- [ ] `/api/onboard/qa/*` endpoints delegate to greentic-setup
- [ ] ~4,944 lines removed from greentic-operator
- [ ] All existing tests pass
- [ ] CLAUDE.md updated
