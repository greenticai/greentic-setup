# Repository Overview

## 1. High-Level Purpose

greentic-setup is a Rust crate (v0.4.x) that provides end-to-end bundle setup for the Greentic platform. It handles pack discovery, QA-driven configuration (via greentic-qa FormSpec with conditional `visible_if` support), bundle-level static hosting policy collection, secrets persistence, admin API types, hot reload diffing, and bundle lifecycle management (create/update/remove).

Previously this logic was embedded in greentic-operator (~5,000 lines). This crate extracts it into a reusable library so it can be consumed by the operator, runner, CLI tools, and admin APIs.

**Technologies:** Rust (edition 2024), serde/serde_json, anyhow, tokio, greentic-qa (qa-spec), greentic-secrets-lib, greentic-distributor-client, serde_cbor, zip.

## 2. Main Components and Functionality

### Library Entry Point
- **Path:** `src/lib.rs`
- **Exports:** all modules, `resolve_env()`, `canonical_secret_uri()`, `canonical_team()`
- **Re-exports:** `SetupEngine`, `SetupMode`, `SetupPlan`, `SetupStep`, `SetupStepKind`

### Setup Engine (`src/engine.rs`)
- Orchestrates plan building and basic execution for create/update/remove workflows
- Types: `SetupEngine`, `SetupRequest`, `SetupConfig`
- Functions: `apply_create()`, `apply_update()`, `apply_remove()`, `print_plan_summary()`, `emit_answers()`, `load_answers()`
- Persists provider setup answers to `state/config/<provider>/setup-answers.json`
- Persists normalized bundle-level static hosting policy to `state/config/platform/static-routes.json`

### Plan Types (`src/plan.rs`)
- `SetupPlan`, `SetupStep`, `SetupStepKind`, `SetupMode`, `SetupPlanMetadata`
- `TenantSelection`, `UpdateOp`, `RemoveTarget`, `PackScope`
- `PackListing`, `ResolvedPackInfo`, `SetupExecutionReport`, `QaSpec`

### Bundle Management (`src/bundle.rs`)
- `create_demo_bundle_structure()`, `validate_bundle_exists()`
- `gmap_path()`, `load_provider_registry()`, `write_provider_registry()`

### Pack Discovery (`src/discovery.rs`)
- Scans bundle directories for `.gtpack` files
- CBOR/JSON manifest reading for pack_id extraction
- `discover()`, `discover_with_options()`, `persist()`

### QA Modules (`src/qa/`)
- **bridge.rs:** Provider QA JSON → FormSpec conversion with `visible_if` parsing
- **wizard.rs:** Interactive wizard, validation (visibility-aware), card rendering, `compute_visibility()`
- **persist.rs:** Secrets + config persistence (visibility-aware — skips invisible questions)

### Secrets (`src/secrets.rs`, `src/secret_name.rs`)
- Dev store path resolution, `SecretsSetup` for pack secrets seeding
- `canonical_secret_name()` normalization

### Setup Input (`src/setup_input.rs`, `src/setup_to_formspec.rs`)
- Answers loading from JSON/YAML, legacy setup.yaml → FormSpec conversion

### Platform Setup (`src/platform_setup.rs`)
- Bundle-level static hosting policy types and normalization
- Interactive prompting for public web/static hosting enablement
- Validation for origin-style `public_base_url`, `public_surface_policy`, and default policy normalization
- Persistence helpers for `state/config/platform/static-routes.json`

### Admin API Types (`src/admin/`)
- **tls.rs:** `AdminTlsConfig` for mTLS endpoint configuration with validation
- **routes.rs:** `AdminRequest`, `AdminResponse`, `BundleDeployRequest`, `QaSubmitRequest`, `BundleStatus`

### Hot Reload (`src/reload.rs`)
- `BundleDiff` — computes added/removed/changed packs between two discovery states
- `ReloadPlan` — generates `ReloadAction` list (load/unload/reload component, update routes, run resolver)
- `diff_discoveries()`, `plan_reload()`

### Adaptive Card Setup (`src/card_setup.rs`)
- `CardSetupSession` — multi-step card-based onboarding with TTL and answer accumulation
- `SetupLinkConfig` — setup URL generation
- `CardSetupResult` — submission result

### Other
- **webhook.rs:** Stub — `has_webhook_url()` helper only

### CI & Tooling
- `ci/local_check.sh` — fmt, clippy, test, build, doc, package dry-run
- `tools/i18n.sh` — i18n translation wrapper
- `.github/workflows/ci.yml`, `.github/workflows/publish.yml`

## 3. Work In Progress, TODOs, and Stubs

| Location | Status | Description |
|----------|--------|-------------|
| `src/engine.rs` | partial | Bundle execution writes provider setup answers and bundle-level static hosting policy, but resolver/runtime/operator-specific execution remains outside this crate. |
| `src/webhook.rs` | stub | Only `has_webhook_url()`. Actual webhook registration requires operator WASM runtime. |
| `src/admin/` | done (types) | Request/response types + TLS config. Actual HTTP server lives in operator. |
| `src/reload.rs` | done (diff+plan) | Diff and plan generation. Actual runtime reload (ArcSwap, drain) lives in operator. |
| `src/card_setup.rs` | done (types) | Session types and link generation. Card rendering uses qa-spec. JWT signing is placeholder. |
| Phase 6 | TODO | Operator passthrough — replace ~4,944 lines in operator with greentic-setup dependency |

## 4. Broken, Failing, or Conflicting Areas

None. Current checks pass:
- `cargo fmt --check` — clean
- `cargo clippy -- -D warnings` — clean
- `cargo test` — 108 tests pass
- `cargo doc` — clean (no warnings)
- `cargo package --allow-dirty` — clean
- `cargo publish --dry-run --allow-dirty` — clean
- `ci/local_check.sh` — steps 1-5 pass locally; package/publish dry-run steps require network access to crates.io in this environment

## 5. Notes for Future Work

- **Execution logic:** Plan execution functions depend on operator-specific modules (distributor-client fetch, gmap policy, project sync). Will be ported in Phase 6.
- **Static hosting policy:** bundle-level replay and persistence now exist, but runtime route assembly and serving still belong to start/operator.
- **Operator integration:** greentic-operator should add `greentic-setup` as dependency and delegate, removing ~4,944 lines of duplicated logic.
- **Admin server:** Actual HTTP server with mTLS needs `axum-server` + `rustls` in the consumer.
- **Hot reload runtime:** ArcSwap-based component swapping and connection draining live in operator.
- **JWT signing:** `card_setup.rs` uses session_id as token. Production should use signed JWTs.
- **Reuse-first:** uses qa-spec, greentic-secrets-lib, greentic-distributor-client.
- **Version:** 0.4.x (matches greentic ecosystem). Includes both library APIs and the `greentic-setup` CLI binary.
