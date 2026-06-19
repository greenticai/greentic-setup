# PR-SE-04 — Final Setup Action Resolution And Tests

## Title

Resolve provider final setup actions generically and cover final-screen behavior with tests

## Goal

Add the data plumbing that lets `greentic-setup` turn provider-declared `greentic.setup.actions.v1` pack-extension metadata into final-screen view models. This PR should be implemented before or alongside the UI screen from PR-SE-03.

This is not the legacy setup executor/action system. Do not read or extend legacy `setup.yaml setup_actions` or runtime `pending_setup_actions`; those are migration targets that the current setup-machine doctor path rejects or removes from migrated providers.

Keep compatibility with providers that already publish final Add to X descriptors under the `greentic.setup.actions.v1` pack extension. The implementation must distinguish that extension from the old `setup.yaml setup_actions` field.

The output should be a generic collection of resolved actions. The UI should consume that collection without knowing provider-specific fields such as `slack_add_url`, `bot_username`, or `add_to_teams_url`.

## Resolver Responsibilities

Create a final setup action resolver that accepts:

- metadata/extensions from discovered setup targets, not every included pack
- public setup-machine outputs and status for each provider
- public backend-contract rendered values and status for each provider
- public persisted setup answers only when they are already safe to show in setup UI
- public runtime/context values that setup already exposes to the browser
- tenant/team context used by URL templates

The resolver should return a list like:

```json
[
  {
    "provider_id": "messaging-slack",
    "action_id": "add-to-slack",
    "label": "Add to Slack",
    "kind": "deep_link",
    "url": "https://...",
    "opens_new_window": true,
    "copyable": true,
    "html": "<a ...>Add to Slack</a>"
  }
]
```

Keep unresolved actions out of the returned list and attach diagnostics separately.

## Value Resolution

Resolve placeholders in `url_template` from a merged read-only context built from public setup data.

Suggested lookup precedence:

1. setup-machine `outputs` or backend-contract rendered `values` for the current provider, tenant, and team
2. public provider setup state values after server-owned keys have been stripped
3. public persisted setup answers that are already allowed in setup UI
4. public runtime/context values exposed to setup
5. tenant/team context values

The resolver must not expose secrets in URLs or HTML. If a required value maps to a secret reference, treat the action as unresolved unless the provider explicitly supplied a non-secret public output for that action.

Examples:

- Teams should resolve `add_to_teams_url` from a provider-declared public setup output, not from Graph/Bot Framework tokens.
- Telegram should resolve `bot_username` only when the provider has exposed it as a public output or public setup value.
- OAuth device codes, access tokens, refresh tokens, bot access tokens, and other server-owned keys must never enter the template context.

## Validation

For every action:

- `schema_id` must be `greentic.setup.actions.v1`
- `id`, `label`, `kind`, and `url_template` are required
- initial supported `kind` is `deep_link`
- `requires` must be a list of placeholder names
- `visible_when` paths are evaluated against the same public setup-machine/backend-contract status and values exposed by the setup UI
- unresolved placeholders suppress the action
- resolved URLs must use an allowed scheme
- generated HTML must escape label and URL

Prefer the existing minimal condition semantics already used by setup completion checks, such as path existence/equality, instead of introducing a new expression language.

Do not evaluate templates as code. Only replace `{name}` placeholders.

## Diagnostics

Diagnostics should include:

- provider ID
- action ID
- missing required value names
- unsupported kind
- invalid URL scheme
- failed `visible_when` condition

Diagnostics should be visible to debug logs or setup developer output, but the admin-facing final screen should show only resolved actions.

## Tests

Add focused unit tests for the resolver:

- rejects or ignores legacy `setup.yaml setup_actions` and runtime `pending_setup_actions` for the final-screen resolver
- accepts existing `greentic.setup.actions.v1` pack-extension metadata from providers
- resolves Slack-style `{slack_add_url}` into a button/action model
- resolves Telegram-style `https://t.me/{bot_username}` with URL escaping
- resolves WebEx-style email template with URL escaping
- resolves Teams-style `{add_to_teams_url}`
- collects actions only from setup targets and skips non-target support packs
- returns multiple actions when multiple providers declare actions
- suppresses an action when a required value is missing
- suppresses an action when `visible_when` does not match
- suppresses an action when the only available value is server-owned or secret
- rejects unsupported schemes such as `javascript:`
- generated HTML escapes quotes, ampersands, and angle brackets

Add integration/UI coverage for the final screen:

- no resolved actions skips the screen
- one resolved action shows button, URL, and HTML snippet
- multiple resolved actions show all providers in deterministic order
- copy buttons copy the URL and HTML snippet values

## Acceptance Criteria

- `greentic-setup` has a single generic resolver for provider final setup actions.
- The final screen can render Slack, Teams, WebEx, and Telegram actions using only metadata and resolved values.
- Providers can add future `Add to X` actions without changes to the final-screen UI.
- Tests cover missing values, multiple providers, setup-target filtering, secret redaction, escaping, and the no-actions path.
