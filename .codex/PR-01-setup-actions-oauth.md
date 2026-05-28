# PR 1 — Add Provider-Agnostic Setup Actions and OAuth Install Flow

## Title

Add provider-agnostic setup actions and OAuth install-button flow

## Actual-Code Review

This repo does not currently have a separate live "provider setup result" executor that invokes arbitrary provider setup/apply WASM operations and receives a structured result. The current setup path is:

- CLI setup/update loads answers in `src/cli_commands/setup.rs`.
- `SetupRequest.setup_answers` is planned into `SetupPlanMetadata.setup_answers`.
- `SetupEngine::execute` calls `execute_apply_pack_setup` in `src/engine/executors.rs`.
- `execute_apply_pack_setup` writes `state/config/{provider_id}/setup-answers.json`, persists every answer into the dev secrets store, writes provider config envelopes, syncs tenant config, and handles declared webhook/oauth ops through `src/webhook/mod.rs`.
- The browser setup UI in `src/ui/mod.rs` uses the same engine via `execute_setup`, and already returns `manual_steps`.

So the implementation should treat `setup_actions` as optional data present in provider setup output/config answers today, while leaving room for future provider-operation invocation. Do not add a new mandatory provider operation or change the fixed messaging operation surface.

Also note:

- `Cargo.toml` already includes `hmac`, `sha2`, `base64`, `rand`, `ureq`, `url`, `axum`, and `tokio`, so the signed-state and token-exchange work should not need broad dependency churn.
- `platform_setup` already persists and resolves `platform_setup.static_routes.public_base_url`, and can load runtime endpoints from `state/runtime/{tenant}.{team}/endpoints.json`.
- `webhook::registration_result_from_declared_ops` already passes through `oauth_ops`, but does not implement OAuth callback handling.
- `qa::persist::oauth_authorize_stub` is only a placeholder and should either be replaced or left unused after setup actions are introduced.
- `discovery` currently reads only pack id/display name from manifests. OAuth metadata lookup will need a small, generic extension reader for `messaging.oauth.v1` or a more general manifest-extension helper.

## Summary

Add a generic setup-action mechanism so provider packs can return user-facing follow-up actions during setup. The initial supported action kind is `oauth_install_button`.

The immediate Slack use case should work without Slack-specific setup logic:

1. Provider setup/config output includes an `oauth_install_button` action.
2. `greentic-setup` persists the pending action.
3. CLI/UI render the provider-supplied install URL.
4. A generic OAuth callback validates signed state, resolves OAuth metadata from the pack, exchanges the code, persists returned secrets/config, and marks the action complete.

## Contract

Add a shared model, likely in a new `src/setup_actions.rs` module and re-export from `src/lib.rs`.

```json
{
  "setup_actions": [
    {
      "id": "slack-install-demo-default",
      "kind": "oauth_install_button",
      "label": "Add to Slack",
      "provider_id": "messaging-slack",
      "tenant": "demo",
      "team": "default",
      "authorize_url": "https://slack.com/oauth/v2/authorize?...",
      "callback_path": "/oauth/callback/slack",
      "state": "signed-state-token",
      "status": "pending"
    }
  ]
}
```

Supported now:

- `oauth_install_button`

Reserved for future compatibility:

- `open_url`
- `copy_secret`
- `manual_step`
- `download_file`
- `admin_consent_button`

Unknown action kinds should be preserved in state where practical, but only `oauth_install_button` needs rendering/callback behavior.

## Implementation Plan

1. Add setup action models and parsing.

   Create `src/setup_actions.rs` with:

   - `SetupAction`
   - `SetupActionKind`
   - `SetupActionStatus`
   - `PendingSetupActionsOutput`
   - helpers to extract `setup_actions` from a provider config/result `serde_json::Value`

   Parsing should accept optional/missing `provider_id`, `tenant`, and `team` from the action, then fill them from the current setup scope. This keeps provider output compact and avoids requiring providers to repeat scope fields.

2. Persist setup actions.

   Persist under bundle state using existing conventions:

   `state/config/setup-actions/{tenant}/{team}/{provider_id}.json`

   Use `default` for missing team in filenames/paths, matching other runtime endpoint conventions.

   Example payload:

   ```json
   {
     "provider_id": "messaging-slack",
     "tenant": "demo",
     "team": "default",
     "actions": [
       {
         "id": "slack-install-demo-default",
         "kind": "oauth_install_button",
         "label": "Add to Slack",
         "authorize_url": "...",
         "callback_path": "/oauth/callback/slack",
         "status": "pending",
         "created_at": "2026-05-21T00:00:00Z",
         "completed_at": null
       }
     ]
   }
   ```

   Add merge/upsert behavior by `action.id` so rerunning setup updates URLs/state without duplicating actions.

3. Hook parsing into actual setup execution.

   In `execute_apply_pack_setup`:

   - After each provider answer/config value is available, extract `setup_actions`.
   - Persist pending actions.
   - Add pending actions to `SetupExecutionReport` via a new `pending_setup_actions: Vec<SetupAction>` field.
   - Avoid persisting `setup_actions` itself as a provider secret. Filter it out before calling `persist_all_config_as_secrets` and before writing provider config envelopes, unless the current runtime explicitly needs the raw field.

   Keep the no-action path identical for existing providers.

4. CLI rendering.

   After `engine.execute(&plan)` in `src/cli_commands/setup.rs`, render any pending `oauth_install_button` actions:

   ```text
   Add to Slack:
   https://slack.com/oauth/v2/authorize?...
   ```

   In interactive mode, do not block unless a local callback server is explicitly active. In `--non-interactive`, only print/emit the actions and exit.

5. UI rendering.

   Extend `src/ui/mod.rs`:

   - Add `pending_setup_actions` to `ExecutionResult`.
   - Return action data from `execute_setup`.
   - Update `assets/setup-ui/app.js` and CSS to render `oauth_install_button` as a real link/button using `authorize_url`.

   Do not add in-app explanatory text beyond normal labels; the button label comes from the provider action.

6. Signed OAuth state.

   Implement a generic helper in `src/setup_actions.rs` or `src/oauth_state.rs`.

   Payload fields:

   ```json
   {
     "provider_id": "messaging-slack",
     "tenant": "demo",
     "team": "default",
     "action_id": "slack-install-demo-default",
     "nonce": "...",
     "expires_at": "..."
   }
   ```

   Requirements:

   - Sign with HMAC-SHA256.
   - Use URL-safe base64.
   - Expire old states.
   - Do not store secrets in the token.
   - Include nonce.
   - Reject malformed, unsigned, mismatched, or expired state.

   Signing key source should be provider-agnostic. Prefer an existing setup/admin secret if one exists; otherwise create and persist a local setup state signing key under bundle state, for example `.greentic/setup-oauth-state-key`, with restricted permissions where supported.

7. OAuth metadata discovery.

   Add a generic manifest extension reader, likely near `src/discovery.rs`, to read extension payloads by key from `manifest.cbor`/`pack.manifest.json`.

   The callback should resolve metadata such as:

   ```yaml
   messaging.oauth.v1:
     auth_type: oauth2
     authorize_url: https://slack.com/oauth/v2/authorize
     token_url: https://slack.com/api/oauth.v2.access
     redirect_path: /oauth/callback/slack
     scopes:
       - chat:write
     secret_keys:
       - SLACK_BOT_TOKEN
   ```

   The implementation must not special-case Slack. It should locate the provider by `provider_id`, read the pack extension, and use `token_url`, `redirect_path`/`callback_path`, and configured secret mapping.

8. Public base URL resolution.

   Reuse existing static-route/runtime behavior:

   1. Explicit provider answer `public_base_url`
   2. `platform_setup.static_routes.public_base_url`
   3. `load_runtime_public_base_url(bundle, tenant, team)`

   If an OAuth install action requires a callback and no public URL exists, return:

   `This provider requires a public_base_url to generate OAuth callback and webhook URLs.`

   If providers already return a full `authorize_url`, setup should persist/render it as-is. Public base URL is required when generating state/redirect URI or when provider output indicates a callback is required but the URL cannot be completed.

9. OAuth callback route/handler.

   For the setup UI server, add a generic route:

   - `GET /oauth/callback/{provider_or_tail}` or route by the provider-supplied `callback_path`

   For library/admin consumers, expose a provider-agnostic function that can be wired into an external router:

   - accept `code` and `state`
   - validate signed state
   - load pending action by provider/tenant/team/action_id
   - verify `callback_path` and provider metadata
   - exchange code using `ureq` or an injectable client for tests
   - map returned token fields to configured secret keys
   - persist through the existing dev secrets store/canonical secret URI helpers
   - mark the action complete

   Keep testability in mind: token exchange should be isolated behind a small trait/function seam so unit tests can use a fake response without making network calls.

10. Secret mapping.

   Store returned tokens according to provider metadata. The minimum generic behavior:

   - If metadata has `secret_keys: ["SLACK_BOT_TOKEN"]`, map the primary OAuth access token from `access_token` or provider-declared response path into that key.
   - Canonicalize through existing `canonical_secret_uri`, which already normalizes secret key names.
   - If metadata later grows explicit mappings, honor those before falling back to the primary token convention.

11. No-UI output.

   Add `pending_setup_actions` to `SetupExecutionReport`. CLI and admin/UI paths can serialize this directly:

   ```json
   {
     "pending_setup_actions": [
       {
         "id": "slack-install-demo-default",
         "kind": "oauth_install_button",
         "label": "Add to Slack",
         "authorize_url": "https://slack.com/oauth/v2/authorize?...",
         "provider_id": "messaging-slack",
         "tenant": "demo",
         "team": "default",
         "callback_path": "/oauth/callback/slack",
         "status": "pending"
       }
     ]
   }
   ```

   Do not block indefinitely in `--non-interactive`.

## Non-Goals

- No Slack-specific branches in `greentic-setup`.
- No new mandatory provider operation.
- No global change to the fixed messaging provider operation list.
- No breaking changes for providers that omit `setup_actions`.
- No live network token exchange during tests.

## Tests

Add focused unit tests near the new modules plus integration-style tests around `execute_apply_pack_setup`:

- Provider config with no `setup_actions` remains unchanged.
- Provider config with `oauth_install_button` is parsed and persisted.
- `setup_actions` is not persisted as a provider secret.
- Existing providers without setup actions still pass.
- Interactive/CLI renderer includes action label and URL.
- UI `ExecutionResult` includes pending actions.
- No-UI setup returns/emits pending actions without blocking.
- Signed state validates happy path.
- Signed state rejects missing, malformed, bad-signature, mismatched, and expired state.
- Callback resolves provider OAuth metadata from pack manifest extension.
- Callback rejects missing OAuth metadata.
- Token response maps `access_token` to configured secret keys.
- Successful callback changes action status from `pending` to `complete`.
- Rerunning setup upserts an existing action instead of duplicating it.

## Suggested File Changes

- `src/setup_actions.rs` new
- `src/oauth_state.rs` new, or keep signed-state helpers inside `setup_actions`
- `src/oauth_callback.rs` new for callback/token-exchange orchestration
- `src/discovery.rs` add generic manifest extension reader
- `src/engine/executors.rs` parse/persist setup actions and filter provider config before secret persistence
- `src/plan.rs` extend `SetupExecutionReport`
- `src/cli_commands/setup.rs` render pending actions after execution
- `src/ui/mod.rs` expose pending actions and callback route
- `assets/setup-ui/app.js` render action buttons
- `assets/setup-ui/style.css` style action buttons
- `src/lib.rs` re-export new modules

## Acceptance Criteria

- Existing setup flows continue to work unchanged.
- Providers may optionally include `setup_actions`.
- Pending setup actions are persisted under bundle state by tenant/team/provider.
- CLI shows `oauth_install_button` label and URL.
- Browser setup UI renders the action as a button/link.
- `--non-interactive` writes/emits pending actions and exits.
- OAuth state is signed and expiry-checked.
- OAuth callback flow is provider-agnostic and uses pack metadata.
- Returned OAuth tokens are persisted through existing secret-store conventions.
- Completed actions are marked complete in state.
- Slack can use this through metadata and setup output only.

