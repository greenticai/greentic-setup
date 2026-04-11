# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

greentic-setup is a Rust crate (v0.4.x) providing end-to-end bundle setup for the Greentic platform. It handles pack discovery, QA-driven configuration (via greentic-qa FormSpec), secrets persistence, admin API types, hot reload diffing, bundle lifecycle management, and — as of v0.4.22 — a Phase 1a interactive dashboard UI.

Part of the greentic-ai mono-workspace (55+ repos). greentic-operator is the primary consumer, delegating all setup logic here.

**Dual-mode:**
- **Library** — core APIs for programmatic use by greentic-operator, runner, and other tools
- **CLI binary** — `greentic-setup <bundle>` launches the dashboard (default); `--no-ui` keeps headless/scriptable mode for all subcommands

## Build & Test

```bash
cargo build                                         # Build (default features: oci + squashfs + ui)
cargo test --features test-helpers                  # Test (193 tests; test-helpers required for UI tests)
cargo clippy -- -D warnings                         # Lint
cargo fmt --all --check                             # Format check
bash ci/local_check.sh                              # Full CI (fmt + clippy + test + build + doc + package)
```

> **test-helpers feature**: `cargo test` (without `--features test-helpers`) will fail to compile several UI test files because `BundleMeta::test_fixture()` is gated behind that feature flag. Always pass `--features test-helpers` when running tests locally or in CI.

## i18n

```bash
tools/i18n.sh all               # Translate + validate + status (200 per batch)
tools/i18n.sh translate         # Generate translations for all languages
```

English source: `i18n/en.json`. Requires `greentic-i18n` at `../greentic-i18n/`.

## Project Structure

```
src/
├── lib.rs                      Library entry point, resolve_env(), canonical_secret_uri()
├── answers_crypto.rs           Encrypted answers file support (AES-GCM-SIV)
├── bundle.rs                   Bundle directory creation, gmap paths, provider registry
├── bundle_source.rs            BundleSource enum (local path, OCI ref, URL)
├── capabilities.rs             Capability discovery and feature-flag resolution
├── card_setup.rs               CardSetupSession for adaptive card flows
├── cli_args.rs                 Top-level Clap argument definitions
├── cli_i18n.rs                 CLI i18n wiring (greentic-i18n integration)
├── config_envelope.rs          ConfigEnvelope — typed wrapper around bundle config
├── deployment_targets.rs       DeploymentTarget enum (local, cloud, edge)
├── discovery.rs                Pack discovery from .gtpack files (CBOR + JSON)
├── flow.rs                     Flow reference types and resolution helpers
├── gtbundle.rs                 .gtbundle creation and extraction (SquashFS)
├── plan.rs                     SetupPlan, SetupStep, SetupMode, metadata types
├── reload.rs                   BundleDiff, ReloadPlan for hot reload
├── secret_name.rs              Canonical secret name normalization
├── secrets.rs                  Dev store path resolution, SecretsSetup
├── setup_input.rs              Setup answers loading from JSON/YAML
├── tenant_config.rs            TenantConfig and per-tenant overrides
│
├── admin/
│   ├── mod.rs                  Admin module entry
│   ├── routes.rs               Admin API request/response types
│   └── tls.rs                  AdminTlsConfig for mTLS
│
├── bin/
│   └── greentic_setup.rs       CLI binary entry point
│
├── cli_commands/
│   ├── mod.rs                  CLI command dispatch
│   ├── inspect.rs              `bundle inspect` — show bundle/pack details
│   ├── lifecycle.rs            `bundle init/add/remove/build` commands
│   └── setup.rs                `bundle setup/update/status/list` commands
│
├── cli_helpers/
│   ├── mod.rs                  CLI helper re-exports
│   ├── bundle.rs               Bundle path resolution and validation helpers
│   ├── env_vars.rs             Environment variable helpers (GREENTIC_ENV etc.)
│   └── prompts.rs              Interactive prompt utilities
│
├── engine/
│   ├── mod.rs                  SetupEngine: plan building for create/update/remove
│   ├── answers.rs              Answer document loading and merging
│   ├── executors.rs            Plan step executors (secrets, config, webhook)
│   ├── plan_builders.rs        Plan construction logic per SetupMode
│   └── types.rs                Engine-internal types
│
├── platform_setup/
│   ├── mod.rs                  Platform setup orchestration
│   ├── persistence.rs          Platform config persistence helpers
│   ├── prompts.rs              Platform-specific interactive prompts
│   ├── types.rs                PlatformSetupConfig and related types
│   └── url.rs                  URL normalization and validation for platforms
│
├── qa/
│   ├── bridge.rs               Provider QA JSON → FormSpec (+ visible_if parsing)
│   ├── persist.rs              Secrets + config persistence (visibility-aware)
│   ├── prompts.rs              QA prompt rendering
│   ├── shared_questions.rs     Common questions reused across providers
│   └── wizard.rs               Interactive wizard, validation, visibility evaluation
│
├── setup_to_formspec/
│   ├── mod.rs                  Legacy setup.yaml → FormSpec conversion entry
│   ├── convert.rs              Conversion logic
│   ├── inference.rs            Field type inference from setup.yaml shape
│   └── pack.rs                 Per-pack FormSpec assembly
│
├── ui/                         Phase 1a dashboard (Alpine.js SPA, Axum server)
│   ├── mod.rs                  UI module entry and feature-gated re-exports
│   ├── assets.rs               Embedded static assets (include_bytes! manifest)
│   ├── auth.rs                 Bearer token + Origin check middleware
│   ├── routes.rs               Axum router wiring (API + asset routes)
│   ├── server.rs               Server bind (127.0.0.1:random), browser open, shutdown
│   ├── sse.rs                  Server-Sent Events stream for live reload notifications
│   ├── state.rs                AppState, BundleMeta, ScopeId, scope switching DTOs
│   └── api/
│       ├── mod.rs              API handler re-exports
│       ├── bundle.rs           GET /api/bundle — bundle metadata response
│       ├── error.rs            Unified JSON error envelope (ApiError)
│       ├── locale.rs           GET /api/locale + POST /api/shutdown
│       ├── overview.rs         GET /api/overview — provider status summary
│       └── wizard.rs           GET /api/wizard/start|session/:id, POST /api/wizard/next|execute
│
└── webhook/
    ├── mod.rs                  Webhook module entry
    └── instructions.rs         Webhook URL validation and instruction generation
```

## Key Architectural Decisions

- **visible_if**: Provider QA questions can include `visible_if` expressions (field equality, truthy, or full qa-spec Expr). Invisible questions are skipped in validation, prompts, and persistence.
- **Visibility-aware persistence**: `persist_qa_secrets` resolves visibility before persisting — invisible/conditional questions whose conditions aren't met are not written to the secrets store.
- **Plan-execute separation**: Plans are deterministic (sorted/deduped). Execution is a separate concern handled by the consumer (operator).
- **Admin API is types-only**: This crate defines request/response types and TLS config. Actual HTTP routing lives in greentic-operator.
- **Hot reload is diff-based**: `diff_discoveries()` computes what changed between two bundle states. `plan_reload()` generates actions. Actual execution (ArcSwap, drain) lives in the consumer.
- **Dashboard binds 127.0.0.1 only**: The Axum server listens on a random available port on loopback, never on a network interface. A random bearer token is generated at startup and injected into the browser URL as a query param.
- **Auth: bearer + origin check**: Every API request must present the startup-generated bearer token (Authorization header or `?token=` param). Origin header is validated against the expected loopback origin. Both checks are enforced by `ui/auth.rs` middleware.
- **All UI assets embedded**: CSS, JavaScript, fonts, and the mascot PNG are compiled into the binary via `include_bytes!` in `ui/assets.rs`. No separate asset directory is needed at runtime.
- **Alpine.js v3 vendored**: The frontend uses Alpine.js v3 as a single minified JS file (`assets/js/alpine.min.js`), vendored into the repo. No build step (webpack/vite) required.
- **Wizard engine is a Phase 1a stub**: The `/api/wizard/*` endpoints return structurally correct responses but the underlying FormSpec wiring to the real `SetupEngine` is deferred to Phase 1b. The stub is sufficient for UI development and integration testing.
- **test-helpers feature flag**: `BundleMeta::test_fixture()` and related test constructors are gated behind `#[cfg(feature = "test-helpers")]`. This keeps test-only helpers out of production builds. All UI integration tests require `--features test-helpers`.

## Conventions

- Edition 2024, version 0.4.x (matches greentic ecosystem)
- **Library + CLI** — exports library APIs and `greentic-setup` CLI binary
- **Reuse-first**: uses greentic-types, greentic-secrets-lib, qa-spec, greentic-distributor-client
- All user-facing strings via greentic-i18n (keys in `i18n/en.json`)
- Follow `.codex/global_rules.md` for PR workflow (repo overview + CI check)

## CLI Commands

Invoking `greentic-setup <bundle-path>` without subcommands launches the Phase 1a interactive dashboard in the default browser. Pass `--no-ui` to suppress the dashboard and use headless/scriptable mode.

```bash
greentic-setup <BUNDLE_PATH>                                    # Launch dashboard UI (default)
greentic-setup <BUNDLE_PATH> --no-ui                            # Headless mode (no browser)
greentic-setup bundle init [PATH] --name <NAME>                 # Initialize bundle directory
greentic-setup bundle add <PACK_REF> --bundle <DIR>             # Add pack to bundle
greentic-setup bundle setup [PROVIDER_ID] --answers <FILE>      # Run setup flow
greentic-setup bundle update [PROVIDER_ID] --answers <FILE>     # Update provider config
greentic-setup bundle remove <PROVIDER_ID> --force              # Remove provider
greentic-setup bundle build --out <DIR>                         # Build portable bundle
greentic-setup bundle list --domain <DOMAIN>                    # List packs/flows
greentic-setup bundle status [--format json]                    # Show bundle status
```

This binary is invoked via `gtc setup ...` passthrough in the greentic repo.

## Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 1 | Extract crate from operator | Done (11 modules, 69 tests) |
| Phase 2 | Conditional questions (visible_if) | Done |
| Phase 3 | Admin endpoint types (mTLS) | Done (types only) |
| Phase 4 | Hot reload diffing | Done (diff + plan) |
| Phase 5 | Adaptive card setup | Done (session types) |
| Phase 6 | CLI binary | Done (bundle init/add/setup/update/remove/build/list/status) |
| Phase 7 | gtc passthrough | TODO — add `gtc setup` → `greentic-setup` delegation in greentic repo |
| Phase 1a Dashboard | Rebuilt web UI (Alpine SPA + scope switcher + embedded wizard) | Done |

## Phase 1a Dashboard

### Spec and Plan

- Spec: `docs/superpowers/specs/2026-04-11-greentic-setup-dashboard-phase-1a-design.md`
- Plan: `docs/superpowers/plans/2026-04-11-greentic-setup-dashboard-phase-1a.md`

### Architecture

The Phase 1a dashboard is a single-page application served by an Axum HTTP server bound exclusively to `127.0.0.1` on a randomly chosen available port. The server is started by `greentic-setup <bundle>` and a browser tab is opened automatically with the bearer token embedded in the URL.

**Frontend stack:**
- Alpine.js v3 (vendored, no build step) for reactive UI
- Poppins font (4 weights, vendored as WOFF2)
- Custom CSS design system (`tokens.css`, `base.css`, `layout.css`, `components.css`, `animations.css`)
- Mascot PNG embedded in binary via `include_bytes!`

**Backend stack:**
- Axum 0.8 on Tokio, feature-gated behind `ui` (default)
- Bearer token + Origin validation on every request (`ui/auth.rs`)
- Security headers middleware: `Content-Security-Policy`, `X-Frame-Options`, `X-Content-Type-Options`, `Referrer-Policy`
- Server-Sent Events (`ui/sse.rs`) for live reload notifications
- All static assets embedded at compile time (`ui/assets.rs`)

**API surface (Phase 1a):**
- `GET /api/bundle` — bundle metadata and scope list
- `GET /api/overview` — provider status summary for active scope
- `GET /api/locale` — locale string map for the UI
- `POST /api/shutdown` — graceful server shutdown
- `GET /api/wizard/start` — start a new wizard session (stub)
- `GET /api/wizard/session/:id` — fetch session state (stub)
- `POST /api/wizard/next` — advance wizard step (stub)
- `POST /api/wizard/execute` — apply wizard answers (stub)

### Phase 1b (Planned)

Phase 1b will add:
- Real FormSpec wiring: replace stub wizard engine with live `SetupEngine` calls
- Secrets CRUD UI: read/write/delete individual secrets from the dashboard
- Provider extension management: add/remove packs from a running bundle
- Capability toggles: enable/disable provider capabilities without re-running setup
- Rebuild trigger: one-click `.gtbundle` rebuild from the dashboard

## Release

1. Bump version in `Cargo.toml`
2. Tag: `git tag v0.4.x && git push origin v0.4.x`
3. GitHub Actions publishes to crates.io

Required secret: `CARGO_REGISTRY_TOKEN`
