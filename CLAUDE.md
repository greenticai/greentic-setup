# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

greentic-setup is a Rust library crate (v0.4.x) providing end-to-end bundle setup for the Greentic platform. It handles pack discovery, QA-driven configuration (via greentic-qa FormSpec), secrets persistence, admin API types, hot reload diffing, and bundle lifecycle management. Designed as a library (not CLI binary) to reduce moving parts in production.

Part of the greentic-ai mono-workspace (33 repos). greentic-operator is the primary consumer, delegating all setup logic here.

## Build & Test

```bash
cargo build                     # Build
cargo test                      # Test (69 tests)
cargo clippy -- -D warnings     # Lint
cargo fmt --all --check         # Format check
bash ci/local_check.sh          # Full CI (fmt + clippy + test + build + doc + package)
```

## i18n

```bash
tools/i18n.sh all               # Translate + validate + status (200 per batch)
tools/i18n.sh translate         # Generate translations for all languages
```

English source: `i18n/en.json`. Requires `greentic-i18n` at `../greentic-i18n/`.

## Project Structure

```
src/
├── lib.rs              Library entry point, resolve_env(), canonical_secret_uri()
├── engine.rs           SetupEngine: plan building for create/update/remove
├── plan.rs             SetupPlan, SetupStep, SetupMode, metadata types
├── bundle.rs           Bundle directory creation, gmap paths, provider registry
├── discovery.rs        Pack discovery from .gtpack files (CBOR + JSON)
├── secrets.rs          Dev store path resolution, SecretsSetup
├── setup_input.rs      Setup answers loading from JSON/YAML
├── setup_to_formspec.rs  Legacy setup.yaml → FormSpec conversion
├── secret_name.rs      Canonical secret name normalization
├── webhook.rs          Webhook URL validation stub
├── reload.rs           BundleDiff, ReloadPlan for hot reload
├── card_setup.rs       CardSetupSession for adaptive card flows
├── qa/
│   ├── bridge.rs       Provider QA JSON → FormSpec (+ visible_if parsing)
│   ├── wizard.rs       Interactive wizard, validation, visibility evaluation
│   └── persist.rs      Secrets + config persistence (visibility-aware)
└── admin/
    ├── mod.rs          Admin module entry
    ├── tls.rs          AdminTlsConfig for mTLS
    └── routes.rs       Admin API request/response types
```

## Key Architectural Decisions

- **visible_if**: Provider QA questions can include `visible_if` expressions (field equality, truthy, or full qa-spec Expr). Invisible questions are skipped in validation, prompts, and persistence.
- **Visibility-aware persistence**: `persist_qa_secrets` resolves visibility before persisting — invisible/conditional questions whose conditions aren't met are not written to the secrets store.
- **Plan-execute separation**: Plans are deterministic (sorted/deduped). Execution is a separate concern handled by the consumer (operator).
- **Admin API is types-only**: This crate defines request/response types and TLS config. Actual HTTP routing lives in greentic-operator.
- **Hot reload is diff-based**: `diff_discoveries()` computes what changed between two bundle states. `plan_reload()` generates actions. Actual execution (ArcSwap, drain) lives in the consumer.

## Conventions

- Edition 2024, version 0.4.x (matches greentic ecosystem)
- **Library-only** — no `[[bin]]` targets
- **Reuse-first**: uses greentic-types, greentic-secrets-lib, qa-spec, greentic-distributor-client
- All user-facing strings via greentic-i18n (keys in `i18n/en.json`)
- Follow `.codex/global_rules.md` for PR workflow (repo overview + CI check)

## Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 1 | Extract crate from operator | Done (11 modules, 69 tests) |
| Phase 2 | Conditional questions (visible_if) | Done |
| Phase 3 | Admin endpoint types (mTLS) | Done (types only) |
| Phase 4 | Hot reload diffing | Done (diff + plan) |
| Phase 5 | Adaptive card setup | Done (session types) |
| Phase 6 | Operator passthrough | TODO — replace operator code with greentic-setup dependency |

## Release

1. Bump version in `Cargo.toml`
2. Tag: `git tag v0.4.x && git push origin v0.4.x`
3. GitHub Actions publishes to crates.io

Required secret: `CARGO_REGISTRY_TOKEN`
