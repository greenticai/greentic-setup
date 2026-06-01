# PR 2 — Generic OAuth Device-Code Setup Flow

## Title

Add provider-agnostic OAuth device-code setup flow

## Context

The Teams Graph tester proved the simplest self-hosted Microsoft Teams setup path:

1. Device-code request against `https://login.microsoftonline.com/organizations/oauth2/v2.0/devicecode`.
2. Show `https://microsoft.com/devicelogin` and the returned `user_code`.
3. User signs in and consents in the browser.
4. Setup polls `https://login.microsoftonline.com/organizations/oauth2/v2.0/token` with the same tenant alias.
5. Setup stores `access_token`, `refresh_token`, and `client_id` into provider secrets/config.
6. Setup runs post-login Graph discovery and writes provider config such as `tenant_id`, `user_id`, `team_id`, and `channel_id`.

This PR should implement that flow generically in `greentic-setup`, so providers can declare device-code setup metadata instead of requiring provider-specific setup code.

This PR complements the existing `.codex/PR-01-setup-actions-oauth.md`, which focuses on setup actions and redirect-style OAuth install buttons. Device-code flow is a separate action kind and should not require redirect URLs, callback servers, app registration creation, client secrets, or public base URLs.

## Actual-Code Review

This repo already has the PR 1 setup-action foundation:

- `src/setup_actions.rs` defines `SetupAction`, `SetupActionKind`, `SetupActionStatus`, persistence, signed redirect OAuth state, and callback token mapping helpers.
- `src/engine/executors.rs` extracts `setup_actions` from provider setup answers, persists them, strips them from provider config/secrets, and returns them as `pending_setup_actions`.
- `src/cli_commands/setup.rs` only renders pending `oauth_install_button` actions after setup execution.
- `assets/setup-ui/app.js` only renders pending `oauth_install_button` actions as links.
- `src/oauth_callback.rs` completes redirect-style OAuth callbacks from provider pack metadata.

So PR 2 should not assume a separate provider setup result executor exists, and it should not add a new mandatory provider operation. It should extend the existing setup-action model and add a small generic action-execution surface for device-code start/poll/finish.

Separate these three data shapes:

1. Provider metadata: stable OAuth device-code configuration declared by the provider pack, loaded via `discovery::read_pack_extension` or a nearby manifest-extension helper.
2. Pending action: user-visible setup action returned from setup answers and persisted under `state/config/setup-actions/...`.
3. Runtime session state: server-side device-code state needed for polling. This must not be logged, returned in pending setup reports, or persisted as user-visible action JSON.

## Goal

Add a generic setup action kind:

```text
oauth_device_code
```

It should support CLI and UI setup for providers such as Microsoft Teams without hardcoded Teams logic.

The action should be executable through generic setup code. Providers declare what to do; `greentic-setup` owns the HTTP device-code request, token polling, secret/config persistence, and generic post-login discovery.

## Non-Goals

- Do not create Microsoft app registrations.
- Do not require or generate redirect/callback URLs.
- Do not require client secrets in the default device-code flow.
- Do not implement Azure Bot Service or Bot Framework logic.
- Do not special-case Teams beyond provider-declared metadata and generic Graph/API discovery steps.

## Provider Metadata Contract

Provider packs should declare stable device-code metadata in a manifest extension, not directly in the pending action payload. Use a specific extension key such as:

```text
messaging.oauth_device_code.v1
```

The exact key can be adjusted to fit existing provider conventions, but implementation must document it and load it generically from the provider pack. A representative metadata payload:

```json
{
  "provider": "microsoft",
  "label": "Connect Microsoft Teams",
  "tenant_alias": "organizations",
  "device_code_url": "https://login.microsoftonline.com/organizations/oauth2/v2.0/devicecode",
  "token_url": "https://login.microsoftonline.com/organizations/oauth2/v2.0/token",
  "verification_uri": "https://microsoft.com/devicelogin",
  "client_id_config_key": "client_id",
  "client_id_secret_key": "MS_GRAPH_CLIENT_ID",
  "scopes": [
    "offline_access",
    "openid",
    "profile",
    "User.Read",
    "Team.ReadBasic.All",
    "Channel.ReadBasic.All",
    "ChannelMessage.Send",
    "ChannelMessage.Read.All",
    "Chat.Read"
  ],
  "secrets_out": {
    "client_id": "MS_GRAPH_CLIENT_ID",
    "refresh_token": "MS_GRAPH_REFRESH_TOKEN",
    "access_token": "MS_GRAPH_ACCESS_TOKEN"
  },
  "config_out": {
    "tenant_id": "tenant_id",
    "client_id": "client_id",
    "user_id": "user_id",
    "team_id": "team_id",
    "channel_id": "channel_id",
    "chat_id": "chat_id"
  },
  "post_login_discovery": [
    {
      "id": "me",
      "method": "GET",
      "url": "https://graph.microsoft.com/v1.0/me",
      "save": {
        "id": "user_id",
        "tenant_id": "tenant_id"
      }
    },
    {
      "id": "joined_teams",
      "method": "GET",
      "url": "https://graph.microsoft.com/v1.0/me/joinedTeams",
      "select": {
        "from": "value",
        "label": "displayName",
        "value": "id",
        "save_as": "team_id"
      }
    },
    {
      "id": "channels",
      "method": "GET",
      "url_template": "https://graph.microsoft.com/v1.0/teams/{team_id}/channels",
      "requires": ["team_id"],
      "select": {
        "from": "value",
        "label": "displayName",
        "value": "id",
        "save_as": "channel_id"
      }
    }
  ]
}
```

Exact field names may be adapted to local Rust types, but the implementation must preserve these capabilities:

- provider-declared device-code endpoint
- provider-declared token endpoint
- same tenant alias used for device-code request and token polling
- provider-declared scopes
- provider-declared secret/config mappings
- optional provider-declared post-login discovery calls

## Pending Action Contract

Provider setup answers may include a pending action instance shaped roughly like:

```json
{
  "setup_actions": [
    {
      "id": "teams-device-code-demo-default",
      "kind": "oauth_device_code",
      "label": "Connect Microsoft Teams",
      "provider_id": "messaging-teams",
      "tenant": "demo",
      "team": "default",
      "status": "pending"
    }
  ]
}
```

The pending action may include non-secret display fields such as `label`, `provider_id`, `tenant`, `team`, `status`, and optional provider-facing hints. It must not include raw `device_code`, access tokens, refresh tokens, or other credentials.

Runtime start/poll responses may return:

- `verification_uri`
- `user_code`
- `expires_at`
- `interval`
- provider-supplied checklist/help text
- a setup-session ID or opaque handle

They must not return the raw OAuth `device_code`.

## Microsoft Teams Defaults

The Teams provider will declare:

- `tenant_alias = "organizations"`
- device-code URL:
  `https://login.microsoftonline.com/organizations/oauth2/v2.0/devicecode`
- token URL:
  `https://login.microsoftonline.com/organizations/oauth2/v2.0/token`
- verification URL:
  `https://microsoft.com/devicelogin`
- scopes:
  `offline_access openid profile User.Read Team.ReadBasic.All Channel.ReadBasic.All ChannelMessage.Send ChannelMessage.Read.All Chat.Read`

The token request must use the same tenant alias as the device-code request. Do not switch token polling to the discovered tenant ID mid-flow.

## CLI Behavior

In interactive `gtc setup`:

1. Gather `client_id` through the existing provider form/answers path, or read it from an existing configured secret/config value declared by metadata.
2. Run setup as today and emit/persist an `oauth_device_code` pending action.
3. If interactive action execution is enabled for this action, start the device-code flow:
   - resolve provider metadata from the pack extension
   - POST the device-code request
   - store raw polling state server-side or in an internal state file that is not included in `pending_setup_actions`
4. Display:

   ```text
   Open https://microsoft.com/devicelogin
   Enter code: ABCD-EFGH
   ```

5. Offer a simple continue/poll step:

   ```text
   Press Enter after approving, or wait while setup polls...
   ```

6. Poll token endpoint until:
   - success
   - `authorization_pending` continues
   - `slow_down` increases interval
   - `expired_token`, `authorization_declined`, `bad_verification_code`, or another terminal error stops with a clear message

7. Persist mapped secrets/config.
8. Run post-login discovery and prompt for selections when configured.
9. Persist discovered values through the existing answers/config/secrets path and mark the setup action complete.

In non-interactive mode:

- By default, emit the `oauth_device_code` pending action and exit with pending manual steps.
- If a future flag enables waiting, poll with a bounded timeout.
- Never block indefinitely.
- Never require browser callback servers or public base URLs.

## UI Behavior

For browser setup UI:

- Extend `src/ui/mod.rs` with generic endpoints for this action, for example:
  - start device login for a pending action
  - poll or finish device login for a session/action
  - submit discovery selections when configured
- Render `oauth_device_code` pending actions as a button to start device login.
- After start succeeds, render:
  - verification URL
  - `user_code`
  - poll/finish button
- Display terminal errors with the provider-supplied checklist.
- On success, run discovery and show selectable teams/channels/chats using generic select metadata.
- Save chosen values into the existing answers/config path.

For card setup, keep this PR limited to returning/rendering the pending action unless there is already a card-specific API surface that can safely run the same start/poll endpoints.

## Token Handling

Device-code request:

```http
POST {device_code_url}
Content-Type: application/x-www-form-urlencoded

client_id={client_id}
scope={space-separated scopes}
```

Token polling request:

```http
POST {token_url}
Content-Type: application/x-www-form-urlencoded

client_id={client_id}
grant_type=urn:ietf:params:oauth:grant-type:device_code
device_code={device_code}
```

Default flow must not send a client secret.

Persist:

- `refresh_token` to mapped secret such as `MS_GRAPH_REFRESH_TOKEN`
- `access_token` to mapped secret such as `MS_GRAPH_ACCESS_TOKEN` if configured as test/dev/current-session secret
- `client_id` to mapped secret such as `MS_GRAPH_CLIENT_ID` and/or config key `client_id`

Use existing secret canonicalization and dev secret store helpers.

Never log raw access tokens, refresh tokens, or device codes. Never place raw `device_code`, access tokens, or refresh tokens in `SetupAction.extra`, `pending_setup_actions`, stdout, stderr, UI JSON responses, or setup reports.

## Runtime State

Add an internal state model for device-code sessions. It should be keyed by provider/action/session and scoped by tenant/team. It may store:

- raw `device_code`
- polling interval
- expiry
- action ID
- provider ID
- tenant/team
- client ID reference or value if required for polling

This state is operational setup state, not provider config. Store it separately from the persisted visible setup action JSON. Expired sessions should fail clearly and be safe to replace by starting the action again.

## Discovery

Add a generic post-login discovery executor:

- method: initially `GET`
- URL or URL template
- bearer token auth using the just-acquired access token
- save simple scalar fields
- support selecting one item from an array result
- support later calls depending on earlier selections

For Teams:

- `GET https://graph.microsoft.com/v1.0/me`
- `GET https://graph.microsoft.com/v1.0/me/joinedTeams`
- `GET https://graph.microsoft.com/v1.0/teams/{team_id}/channels`

Persist selected/discovered values:

- `tenant_id`
- `user_id`
- `team_id`
- `channel_id`
- optional `chat_id`

## Error Checklist

For Microsoft device-code failures or Teams subscription-read consent issues, show a concise checklist when metadata provides it:

- App registration must be multi-tenant: `signInAudience = AzureADMultipleOrgs`
- Public client/device-code flow must be enabled
- Tenant may require admin consent
- Teams message subscriptions require read scopes such as `ChannelMessage.Read.All`
- Device/token endpoints must use `organizations`, not the developer tenant ID

## Implementation Areas

Likely files/modules:

- `src/setup_actions.rs`
- `src/oauth_device.rs` or similar new module
- `src/discovery_actions.rs` or similar new module
- `src/engine/executors.rs`
- `src/cli_commands/setup.rs`
- `src/ui/mod.rs`
- `assets/setup-ui/app.js`
- `assets/setup-ui/*.css`
- `src/discovery.rs` or a new manifest extension helper
- `src/qa/persist.rs` if answer/secret filtering needs adjustment
- tests under `tests/` and/or module unit tests

Prefer small reusable modules:

- `oauth_device.rs` for device-code metadata, request/polling models, runtime state, and redaction-safe reports
- `discovery_actions.rs` for authenticated discovery execution
- preserve existing setup action APIs from PR 1 if already implemented

## Tests

Add tests for:

- parsing `oauth_device_code` action metadata
- loading provider device-code metadata from the chosen pack extension key
- device-code request form:
  - correct URL
  - `client_id`
  - scopes
  - no `client_secret`
- token polling request form:
  - same token URL/tenant alias as device-code URL
  - `grant_type=urn:ietf:params:oauth:grant-type:device_code`
  - no `client_secret`
- polling behavior:
  - `authorization_pending`
  - `slow_down`
  - success
  - terminal errors
- secret mapping:
  - `refresh_token -> MS_GRAPH_REFRESH_TOKEN`
  - `client_id -> MS_GRAPH_CLIENT_ID`
- discovery:
  - `/me` saves `user_id` / tenant when present
  - joined teams selection saves `team_id`
  - channels selection saves `channel_id`
- token redaction in reports/logs
- pending action persistence does not include raw `device_code` or tokens
- CLI/UI start responses contain verification URL / user code without raw `device_code` or tokens

Use a fake HTTP client or injectable transport. Do not hit Microsoft in tests.

## Acceptance Criteria

- `gtc setup` can emit a generic `oauth_device_code` pending action and interactive setup can execute it without provider-specific Rust code.
- The default Microsoft tenant alias is `organizations` when declared by the provider.
- Device-code and token requests both use the same declared tenant alias.
- Setup shows `https://microsoft.com/devicelogin` and the `user_code`.
- Setup polls and stores refresh token/client ID using provider mappings.
- Setup can run generic post-login discovery and save team/channel selections.
- No redirect URL is required.
- No client secret is sent in the default device-code flow.
- No app registration is created.
- Raw `device_code`, access tokens, and refresh tokens are never included in visible setup action JSON or UI/CLI reports.
- No Teams-specific setup Rust code is required beyond metadata-driven discovery.
- Existing tests pass.

## Paired Provider PR

The paired provider-pack PR lives in:

```text
greentic-messaging-providers/.codex/TEAMS-DEVICE-CODE-SETUP-PR.md
```

That PR updates Teams metadata/schema/docs so this generic setup flow has everything it needs.
