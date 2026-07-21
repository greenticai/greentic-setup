# CLAUDE.md — greentic-setup

Bundle setup engine and CLI — pack discovery, QA-driven configuration wizards,
secrets persistence, config-envelope emission, environment manifests, and hot
reload. Invoked via `gtc setup` (delegation chain) or directly as
`greentic-setup`.

For wizard-replay semantics, cross-repo ownership rules, and agent documentation
conventions see [docs/coding-agents.md](docs/coding-agents.md).

## Crate Info

Single crate (no workspace). Version `1.2.0-dev.0`, edition 2024,
`rust-version = "1.95"`. Toolchain pinned to 1.95.0 via `rust-toolchain.toml`.

Binary: `src/bin/greentic_setup.rs`.

## Cargo Features

| Feature | Default | What it gates |
|---------|---------|---------------|
| `oci` | yes | OCI/distributor pack fetching via `greentic-distributor-client` |
| `squashfs` | yes | SquashFS `.gtbundle` read/write via `backhand` |
| `ui` | yes | Local Axum web UI for browser-driven setup (`axum`, `open`) |

## Source Layout (`src/`)

| Module | What it does |
|--------|-------------|
| `engine/` | Setup engine: plan builders, executors, answer loading/encryption, type definitions. Main entry `SetupEngine` |
| `admin/` | mTLS admin API types (routes, TLS config). Server lives in `greentic-start` |
| `cli_commands/` | Clap subcommands: `setup`, `doctor`, `inspect`, `lifecycle` |
| `cli_helpers/` | CLI utilities: bundle source resolution, env-var handling, prompts |
| `cli_args.rs` | Top-level Clap arg definitions |
| `cli_i18n.rs` | CLI i18n facade |
| `answers_crypto.rs` | AES-GCM-SIV encryption for secret answers on disk |
| `config_envelope.rs` | CBOR config envelope: provider config + `secrets://` URI refs (no plaintext after B12a) |
| `discovery.rs` | Pack discovery and resolution from bundle projects |
| `deployment_targets.rs` | Deployment target definitions |
| `doctor.rs` | Bundle health diagnostics |
| `env_mode.rs` | `env-manifest.v1` routing: `--answers <manifest>` porcelain over deployer env-apply |
| `env_wizard.rs` | `--env` interactive wizard: author/gap-fill env-manifest, persist, hand off to deployer |
| `flow.rs` | Flow inspection and validation |
| `gtbundle.rs` | `.gtbundle` SquashFS read/write/inspection |
| `no_ui_oauth.rs`, `oauth_callback.rs`, `oauth_device.rs` | OAuth flows: device-code grant, callback server, headless fallback |
| `plan.rs` | `SetupPlan`, `SetupStep`, `SetupMode` — deterministic plan-then-execute |
| `platform_setup/` | Platform persistence: static routes, tunnel config, URL helpers |
| `provider_state.rs` | Per-provider setup state tracking |
| `qa/` | QA subsystem: FormSpec bridge, wizard prompts, shared questions, answer persistence |
| `reload.rs` | Hot-reload watcher for live setup changes |
| `secrets.rs`, `secret_name.rs` | Secrets persistence (via `greentic-secrets-lib`) and naming |
| `setup_actions.rs` | Concrete setup action implementations |
| `setup_input.rs` | Setup input loading and validation |
| `setup_to_formspec/` | Conversion: setup inputs to FormSpec (inference, pack handling) |
| `setup_tunnel.rs` | Tunnel configuration for provider callbacks |
| `tenant_config.rs` | Tenant/team configuration management |
| `ui/` | Browser-based setup UI (Axum, feature-gated on `ui`) |
| `webhook/` | Webhook instruction rendering |
| `bundle.rs`, `bundle_source.rs` | Bundle model and source resolution |
| `capabilities.rs` | Capability extraction and validation |
| `card_setup.rs` | Adaptive Card setup wizard |
| `generated_secrets.rs` | Materialises provider-declared generated secrets into the local dev secrets store |
| `schema_validation.rs` | Minimal JSON Schema validation subset for provider setup contracts |
| `setup_machine.rs` | Provider-declared setup state machines: metadata loading, contract validation, resumable execution |
| `setup_backend_contract.rs` | Shared helpers for provider setup backend contract execution (UI-independent mutation rules) |
| `setup_final_actions.rs` | Resolved final setup actions (post-setup URLs, labels, copy targets) |
| `shared_tunnel.rs` | Machine-wide shared quick-tunnel record, file-protocol compatible with greentic-start |

## Build and Test

```bash
cargo build --all-features                                 # build with all features
cargo test --all-features                                  # test
cargo clippy --all-targets --all-features -- -D warnings   # lint
cargo fmt --all -- --check                                 # format check
bash ci/local_check.sh                                     # full local CI gate
```

## Key Dependencies and Invariants

- **serde_yaml_gtc** (imported as `serde_yaml_bw`): Hardened YAML fork. Never
  use upstream `serde_yaml`.
- **greentic-deployer**: Env-manifest routing delegates to the deployer's
  env-apply engine. Setup is porcelain; deployer owns the plan/execute logic.
- **greentic-runner-host**: Runtime host types for pack/flow execution context.
- **greentic-secrets-lib** (`providers-dev` feature): Secret storage backends.
- **greentic-types** (`serde` feature): Shared domain primitives.
- **qa-spec**: QA specification types consumed by the FormSpec bridge.
- **Config envelopes**: CBOR-serialized, carry `secrets://` URI refs for
  secret-marked keys (no plaintext after B12a). Consumers dereference via
  `SecretsManager`.
- **Answers encryption**: `answers_crypto.rs` uses AES-GCM-SIV; encrypted
  answers written to the setup-state directory.
- **Plan-execute separation**: `SetupPlan` is deterministic; execution is a
  separate concern driven by `engine/executors.rs`.

## Tests and Benchmarks

```bash
cargo test --all-features                      # all unit + integration tests
cargo bench                                    # Criterion benchmarks (benches/perf.rs)
```

Integration tests in `tests/`:
- `env_wizard_locale_catalog.rs` — locale catalog coverage for the env wizard
- `perf_scaling.rs` — setup scaling characteristics
- `perf_timeout.rs` — timeout behavior under load

Helper scripts in `scripts/`:
- `demo.sh` — end-to-end demo run
- `install-hooks.sh` — one-time git hook setup (points `core.hooksPath` at `.githooks/`)
- `make_test_bundle.sh` — builds a deployable test bundle carrying the default-welcome pack (and optionally a messaging provider) for e2e testing with greentic-start
- `test_default_welcome.sh` — e2e smoke test: verifies the default welcome pack renders its welcome card when the `default` flow runs
- `test_provider.sh` — provider setup smoke test

`tools/i18n.sh` regenerates i18n catalogs.

## i18n

66 locale JSON files under `i18n/` (Arabic variants, Bengali, Chinese, Czech, …).
CLI strings go through `cli_i18n.rs`; regenerate catalogs with `tools/i18n.sh`.

## CI Gate Detail

`ci/local_check.sh` runs exactly the same commands documented in "Build and Test" above, in this order:
1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-features`
4. `cargo build --all-features`
5. `cargo doc --no-deps --all-features`
6. `cargo package` + `cargo publish --dry-run` (per publishable crate)

## CLI Quick Reference

```bash
greentic-setup --help                    # top-level
greentic-setup setup --help              # run setup wizard
greentic-setup doctor --help             # diagnose bundle health
greentic-setup --env <name> --help       # environment wizard
greentic-setup --answers <file> --help   # apply answers non-interactively
```
