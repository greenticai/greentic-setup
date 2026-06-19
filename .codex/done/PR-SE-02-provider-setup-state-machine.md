# PR-SE-02 — Provider Setup State Machine

## Title

Add provider-declared setup state machines with resumable execution and doctor validation

## Goal

Replace brittle provider-specific setup flows with a provider-declared setup state machine that `greentic-setup` can validate, execute, persist, resume, and repair after expected mid-process failures.

Provider packs should describe setup as data. `greentic-setup` should own orchestration, state persistence, retry policy, logging, and doctor validation.

This work should also define the migration path away from the current ad hoc setup mechanisms. The end state is that provider setup runs through the state-machine executor, and the old setup path is removed rather than kept as a parallel implementation.

Important current-codebase constraint: first-party Teams setup already declares a generic setup contract through `greentic.setup.web-component.v1` and `greentic.setup.backend-contract.v1`. The setup-machine work must not break that contract or force Teams to add `greentic.setup.machine.v1` before the schema/executor is complete. Treat backend-contract validation as the current provider-doctor target and setup-machine as the next migration layer.

## Why

Teams setup currently depends on a long sequence of external state:

- public tunnel availability and renewal
- Microsoft OAuth/device-code state
- access-token expiry and refresh-token availability
- Teams app package generation and upload/install status
- Graph subscription creation and renewal
- selected tenant/team/channel/chat IDs
- partially written local config/secrets

If setup stops halfway through, the next run should not guess from scattered artifacts. It should load a single setup state record, validate prerequisites, resume from the correct step, repair known failures where possible, and ask the user only when operator action is required.

## State Machine Contract

Add a setup-process extension that provider packs can expose, for example `greentic.setup.machine.v1`.

Example shape:

```json
{
  "version": 1,
  "id": "messaging-teams-default",
  "display_name": "Microsoft Teams setup",
  "entry_step": "collect_public_endpoint",
  "steps": [
    {
      "id": "collect_public_endpoint",
      "title": "Public endpoint",
      "kind": "platform_public_url",
      "requires": [],
      "outputs": ["public_base_url"],
      "on_success": "microsoft_login",
      "on_failure": [
        {
          "when": "missing_public_base_url",
          "action": "prompt",
          "message_key": "setup.public_url.required"
        }
      ]
    },
    {
      "id": "microsoft_login",
      "title": "Microsoft sign in",
      "kind": "oauth_device_code",
      "requires": ["client_id"],
      "outputs": [
        "MS_GRAPH_ACCESS_TOKEN",
        "MS_GRAPH_REFRESH_TOKEN",
        "tenant_id",
        "user_id"
      ],
      "recover": [
        {
          "when": "access_token_expired",
          "action": "refresh_oauth_token"
        },
        {
          "when": "refresh_token_invalid",
          "action": "reauthorize"
        }
      ],
      "on_success": "select_team_channel"
    }
  ]
}
```

The exact field names can adapt to local pack-manifest conventions, but the schema must support:

- stable machine ID and version
- ordered and named steps
- step prerequisites
- outputs and secret/config destinations
- retry and timeout policy
- recoverable failure rules
- user-action failure rules
- terminal success and terminal failed states
- idempotency keys for external side effects
- optional rollback/cleanup actions for resources created during setup

## Built-In Step Kinds

Support a narrow initial set of generic step kinds:

- `qa_form`: collect or validate FormSpec answers
- `platform_public_url`: resolve persisted static-route policy, runtime endpoint, or tunnel URL
- `cloudflare_tunnel`: create, validate, renew, or restart a setup tunnel
- `oauth_device_code`: start, poll, refresh, and reauthorize device-code OAuth
- `oauth_authorization_code`: render install/admin-consent URL and complete callback
- `http_probe`: make a provider-declared HTTP request and validate status/body
- `http_json`: call a provider-declared HTTP API and map JSON outputs
- `select_from_json`: present choices from a previous JSON output
- `generate_file`: render a provider-declared template into a local file/artifact
- `download_file`: expose a generated artifact to the user
- `manual_action`: wait for operator/admin action and record completion
- `provider_component_call`: invoke a constrained provider component setup operation when data-only steps are insufficient

Unknown step kinds must fail doctor validation unless the pack declares a minimum `greentic-setup` version that supports them.

## State Persistence

Persist setup state under bundle state, separate from runtime config:

```text
state/setup/{tenant}/{team}/{provider_id}/machine.json
state/setup/{tenant}/{team}/{provider_id}/events.jsonl
state/setup/{tenant}/{team}/{provider_id}/locks/
state/setup/{tenant}/{team}/{provider_id}/artifacts/
```

Suggested `machine.json` shape:

```json
{
  "schema_version": 1,
  "provider_id": "messaging-teams",
  "tenant": "demo",
  "team": "default",
  "machine_id": "messaging-teams-default",
  "machine_version": 1,
  "status": "running",
  "current_step": "microsoft_login",
  "completed_steps": ["collect_public_endpoint"],
  "failed_step": null,
  "answers_hash": "sha256:...",
  "pack_fingerprint": "sha256:...",
  "outputs": {
    "public_base_url": {
      "kind": "config",
      "ref": "state/config/platform/static-routes.json#/public_base_url"
    }
  },
  "created_resources": [],
  "last_error": null,
  "updated_at": "2026-06-17T00:00:00Z"
}
```

Secrets should remain in the existing secrets store. `machine.json` should only reference secret keys and metadata, never contain secret values.

## Resume Semantics

On `setup` or `update`, `greentic-setup` should:

1. Discover the provider pack and load the declared state machine.
2. Load existing setup state for the provider/tenant/team.
3. Verify machine ID, schema version, pack fingerprint compatibility, and answers hash.
4. Revalidate prerequisites for the current step.
5. Run recovery actions for expired or invalid intermediate resources.
6. Resume at the first incomplete non-skippable step.
7. Re-run idempotent side-effect steps only with their declared idempotency key.

If the pack changed incompatibly, require either a declared migration or a fresh setup run that archives the old state.

## Error Recovery

Define structured setup errors:

- `missing_prerequisite`
- `invalid_answer`
- `external_timeout`
- `oauth_pending`
- `oauth_denied`
- `oauth_token_expired`
- `oauth_refresh_failed`
- `tunnel_unreachable`
- `tunnel_expired`
- `subscription_expired`
- `subscription_conflict`
- `manual_action_required`
- `provider_contract_error`

Each error should include:

- machine ID
- step ID
- recoverability
- suggested next action
- redacted diagnostic context
- correlation ID

Recovery rules should be declarative and bounded. Setup must not loop forever on token refresh, tunnel renewal, or external API retries.

## Logging And Observability

Write an append-only setup event log as JSONL:

```json
{
  "ts": "2026-06-17T00:00:00Z",
  "level": "info",
  "event": "setup.step.started",
  "provider_id": "messaging-teams",
  "tenant": "demo",
  "team": "default",
  "machine_id": "messaging-teams-default",
  "step_id": "microsoft_login",
  "correlation_id": "..."
}
```

Requirements:

- redact all secrets and OAuth tokens
- include correlation IDs across retries and recovery
- expose a compact `greentic-setup setup status` or equivalent status output
- make UI and CLI read from the same execution report
- keep logs stable enough for support/debugging

## Doctor Validation

Add:

```text
greentic-setup doctor provider <pack>
```

The command should validate setup contracts without running setup.

Minimum checks:

- pack can be opened and manifest extensions can be read
- either `greentic.setup.backend-contract.v1` or `greentic.setup.machine.v1` is present
- current backend-contract JSON matches the supported generic setup contract
- setup state machine JSON matches schema when a setup-machine extension is present
- setup web-component metadata is present when the backend-contract model is used
- backend-contract `required_order` has matching actions
- backend-contract action dependencies reference declared actions
- backend-contract executor kinds are supported by this `greentic-setup`
- backend-contract server-owned config keys are honored by setup/UI code
- machine has one entry step and valid terminal states
- all `on_success`, `on_failure`, and recovery step references exist
- no unreachable required steps
- no cycles unless the cycle is explicitly bounded by retry/timeout policy
- each step kind is supported by the installed `greentic-setup`
- required inputs are satisfiable from FormSpec answers, platform config, previous outputs, or secrets
- output mappings are valid and do not write secret values into config files
- OAuth URLs, scopes, token mappings, and callback paths are internally consistent
- tunnel/public URL requirements are explicit for webhook/subscription steps
- generated artifacts have deterministic paths under setup state
- i18n/message keys referenced by manual actions exist when the pack declares localized setup text
- example/fixture setup state can be replayed if the pack includes fixtures

Doctor should return machine-readable JSON with diagnostics, plus a human-readable summary.

## Migration And Removal Plan

Add an explicit migration plan from the old provider setup path to the state-machine path.

Phases:

1. Harden provider-doctor validation for the current generic backend-contract/web-component setup model.
2. Introduce state-machine parsing, validation, persistence, and execution behind a provider-pack capability check.
3. Support backend-contract providers as the current generic setup surface while setup-machine adoption rolls out.
4. Add migration tooling that can read backend-contract/legacy setup artifacts and produce equivalent setup-machine state where possible.
5. Update all known provider packs to declare setup-machine metadata once the executor is ready.
6. Make `greentic-setup doctor provider <pack>` fail for providers that still depend on non-generic legacy setup contracts.
7. Remove the legacy provider setup executor, legacy setup action glue, and dead compatibility branches.

Migration should cover existing artifacts such as:

- `state/config/{provider_id}/setup-answers.json`
- provider config envelopes
- secrets created from legacy setup answers
- pending setup actions
- backend-contract setup state and provider setup event logs
- OAuth/device-code state if it exists outside the new setup state directory
- generated Teams app/setup artifacts
- runtime endpoint/static-route values used by setup

The migration command should be explicit, for example:

```text
greentic-setup setup migrate --provider <provider-id>
```

Requirements:

- migration is idempotent
- migration never copies secret values into setup state
- migration writes an event-log entry and a before/after summary
- migration archives old setup state before deleting or superseding it
- removal of old setup-action execution code happens in this PR; backend-contract remains supported as a generic setup contract until providers adopt setup-machine metadata and fixtures
- the final PR should delete tests that assert legacy behavior and replace them with state-machine tests

## CLI / UI Behavior

CLI should support:

- `greentic-setup setup --provider <pack>` starts or resumes setup
- `greentic-setup setup status --provider <provider-id>`
- `greentic-setup setup retry --provider <provider-id> --step <step-id>`
- `greentic-setup setup reset --provider <provider-id>` with explicit confirmation
- `greentic-setup doctor provider <pack>`

UI should:

- render current step and known pending manual actions
- survive browser/server restarts by loading persisted state
- show recovery-required states without losing completed work
- use provider-supplied labels/messages from the machine contract

Current setup-side implementation bridge:

- `greentic-setup doctor provider <pack>` validates `greentic.setup.backend-contract.v1` today and independently validates `greentic.setup.machine.v1` whenever that extension is present, including mixed packs that temporarily declare both during migration.
- Setup-machine doctor validation now rejects unreachable steps, invalid transitions, unsupported step kinds, unbounded transition/recovery cycles unless the cycle declares bounded retry or timeout policy, generated/download artifact paths that are missing or escape setup artifacts, malformed HTTP step declarations, invalid `cloudflare_tunnel` declarations, invalid `oauth_device_code` declarations, invalid `oauth_authorization_code` declarations including missing/unsafe `token_url`, invalid `select_from_json` declarations, invalid `persist_runtime_config` declarations including declared writes to server-owned config keys, invalid `provider_component_call` declarations, side-effecting `http_json`/`provider_component_call` steps without an `idempotency_key`, template placeholders that do not resolve to built-ins, setup FormSpec answers, or declared setup outputs, missing setup `message_key` references when the provider declares `greentic.setup.i18n.v1` localized setup text, and setup fixture states that cannot be replayed against the declared machine id/version and step graph.
- `greentic-setup bundle setup-status <provider-id>` and `bundle setup-next <provider-id>` prefer `greentic.setup.machine.v1` when a provider declares it. The setup-machine execution slice persists `machine.json`, appends setup-machine events, resumes the current step after restart, executes `platform_public_url` from static-route/runtime defaults, resolves/validates existing `cloudflare_tunnel` HTTPS public URLs with retryable tunnel diagnostics, starts and polls `oauth_device_code` logins through a private transient session file while keeping device codes/tokens out of `machine.json`, renders signed `oauth_authorization_code` authorization URLs, completes setup-machine OAuth callbacks through the existing no-UI/UI callback boundary, exchanges authorization codes via the step `token_url`, persists mapped tokens only as secrets, keeps token material out of `machine.json`, runs generic `http_probe`/`http_json` steps with retryable diagnostics and JSON output capture, renders `generate_file` setup artifacts under setup state, performs deterministic `select_from_json` selections or pauses for operator choice, persists non-server-owned runtime config from `persist_runtime_config`, invokes constrained `provider_component_call` operations when pack provenance is available, completes explicitly `auto_complete` steps, and pauses with structured retryable diagnostics for manual/operator-required or unsupported future step kinds.
- Setup-machine execution now follows declared `recover`/`on_failure` transition targets for matching structured error codes. When a recovery target is selected, the runner persists the target as the next resume step, includes `recovery_step` in the output/event detail, and appends a `setup.step.recovery_scheduled` event.
- Setup-machine resume now stamps provider pack fingerprints and setup-answer hashes into new machine state when those inputs are available. Persisted state schema, provider scope, machine ID/version, pack fingerprint, and setup-answer hash mismatches are treated as incompatible saved state. `setup-next` persists the blocked state, appends a `setup.machine.incompatible_state` event, and requires explicit `setup-migrate` or `setup-reset` instead of continuing with stale setup inputs.
- `greentic-setup bundle setup-status <provider-id>` reads the generic backend-contract state from `state/setup/{tenant}/{team}/{provider_id}/backend-contract.json`.
- `greentic-setup bundle setup-next <provider-id>` selects the next resumable backend-contract action, executes safe headless `oauth_device_code` start/poll actions, external `provider_http` actions with explicit `url_template`/body, same-origin `provider_http` actions whose `path_template` resolves to a pack-declared `greentic.http-routes.v1` setup-component route, `runtime_observation` actions when the observed state is already present in persisted setup state, `microsoft_graph_application` app registration/reuse once Graph OAuth is available, `microsoft_graph_teams_app_catalog_publish` using provider-pack Teams app assets, and `microsoft_graph_teams_app_user_install` including the manual-link path when user Graph OAuth is unavailable. Missing runtime observations are recorded as retryable waiting/blocked results. Unsupported/future headless executors are recorded as structured blocked results, and every action appends `state/setup/{tenant}/{team}/{provider_id}/events.jsonl`.
- `greentic-setup bundle setup-retry <provider-id> [--step <step-id>]` clears retry-blocking result/resume markers without discarding completed setup state. For setup-machine packs it clears paused/failed diagnostics for the selected/current step while preserving `completed_steps`.
- `greentic-setup bundle setup-reset <provider-id> --yes` archives and clears persisted generic setup state for both backend-contract and setup-machine providers.
- `greentic-setup bundle setup-migrate <provider-id>` now routes by pack capability. For setup-machine packs it initializes or preserves `machine.json`, archives/removes legacy backend/setup-action artifacts without copying old config or token-like values into machine state, and writes a setup-machine migration event. For backend-contract packs it migrates legacy backend-contract state from `state/setup-backends/{env}/{tenant}/{team}/{provider}.json` into the generic setup state directory, archives and removes that legacy artifact plus the older `state/config/setup-actions/{tenant}/{team}/{provider}.json` action file when present, and writes a migration event. Normal setup status/next/retry reads no longer silently fall back to legacy setup stores; explicit migration is the compatibility path.
- The legacy `ApplyPackSetup` plan step and executor have been removed. Bundle create/update/remove no longer applies provider setup answers implicitly or schedules/persists legacy `setup_actions` from setup answers or `setup.yaml`; explicit setup-machine/backend-contract commands are now the setup execution surfaces. The old pending-action planner/executor branch, pending-action execution report/UI field, no-UI OAuth callback server, and action-backed OAuth device-code UI endpoints have also been removed.
- OAuth callback completion now targets setup-machine OAuth authorization-code steps only. Legacy persisted setup-action callbacks no longer mark action files complete; existing legacy action files are handled by explicit migration/archive commands instead of active execution.
- Provider doctor now fails migrated packs that still declare legacy `setup_actions` in `setup.yaml`, so first-party providers cannot carry the old setup path forward once they declare a generic setup contract.
- The setup UI exposes setup-machine metadata in `/api/providers` and handles generic `greentic.setup.machine.v1` state through the existing `/v1/messaging/setup/{provider}/{tenant}` route family. `GET /.../{tenant}` renders persisted machine status, `POST /next` advances through the shared setup-machine runner, `POST /retry` clears paused/failed diagnostics while preserving completed steps, and `POST /reset` archives/removes machine state.
- The setup UI now uses the shared backend-contract state/result helpers for config mutation, state persistence, OAuth-resume recovery, blocked retryability, action-result event logging, and Microsoft Graph/Teams backend-contract executors that are also used by the CLI.

## Files Likely Touched

- `src/discovery.rs`
- `src/engine.rs`
- `src/engine/executors.rs`
- `src/plan.rs`
- `src/setup_actions.rs` or a new `src/setup_machine.rs`
- `src/cli_commands/setup.rs`
- `src/ui/mod.rs`
- `src/platform_setup.rs`
- `src/webhook.rs`
- new `src/doctor.rs`
- new `schemas/setup-backend-contract.v1.schema.json`
- new `schemas/setup-machine.v1.schema.json`
- tests under `tests/` or focused module tests
- `.codex/repo_overview.md`

## Acceptance Criteria

- Provider packs can declare a setup state machine extension.
- `greentic-setup` validates the extension before execution.
- Setup state persists separately from runtime config and excludes secret values.
- A stopped setup can resume from the correct incomplete step.
- Known recoverable failures can run declared recovery actions.
- CLI and UI receive the same execution report.
- `greentic-setup doctor provider <pack>` validates setup scripts and emits structured diagnostics.
- Existing providers without a setup machine/backend-contract no longer run legacy `setup_actions`; they must migrate setup behavior into the generic setup contract.
- Legacy setup artifacts can be migrated or safely archived.
- The old setup execution path is removed; explicit migration/archive commands handle old artifacts.

## Non-Goals

- Replacing all QA/FormSpec setup in one PR.
- Teams-specific Rust logic in `greentic-setup`.
- Full operator/runtime reconciliation of provider resources.
- Automatic deletion of externally created resources without explicit provider-declared cleanup.
- Keeping the old provider setup implementation indefinitely.

## Open Design Questions

- Should the schema live in this repo only, or be published in a shared provider-schema crate so providers can validate at build time?
- Should setup machine execution be purely data-driven, or should `provider_component_call` be required for complex provider-specific probes?
- How strict should pack fingerprint compatibility be when only setup text or docs change?
- Should state migrations be embedded in the machine contract or handled as separate versioned migration files?
- What is the exact boundary between `greentic-setup doctor provider <pack>` and existing pack doctor/build validation commands?
