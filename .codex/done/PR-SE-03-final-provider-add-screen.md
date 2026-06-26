# PR-SE-03 — Final Provider Add Screen

## Title

Show a conditional final setup screen with provider-declared Add to X actions

## Goal

After the active provider setup flow completes, `greentic-setup` should show an additional final screen only when at least one setup target declares a resolved user-facing add action through provider metadata.

This screen is for administrators. It should explain that the admin can share the generated links or add the generated buttons to an internal page so users can easily add the digital worker to the messaging channel.

The screen must support any number of providers and must not hard-code Slack, Teams, WebEx, or Telegram behavior. Those providers are the first consumers, but the renderer should be generic.

## Provider Contract

Read provider-declared actions from the existing pack extension named `greentic.setup.actions.v1`.

Do not break providers that already publish final Add to X metadata under `greentic.setup.actions.v1`. In this PR, `greentic.setup.actions.v1` means pack-extension metadata for final user-facing links/buttons. It does not mean the legacy `setup.yaml setup_actions` executor field or runtime `pending_setup_actions`.

The current setup migration/doctor work still treats legacy `setup.yaml setup_actions` and `pending_setup_actions` as old executor state that must be removed from migrated providers. Keep that rejection scoped to the legacy setup executor path; do not reject the `greentic.setup.actions.v1` pack extension.

Expected action shape:

```json
{
  "schema_id": "greentic.setup.actions.v1",
  "provider_id": "messaging-slack",
  "actions": [
    {
      "id": "add-to-slack",
      "label": "Add to Slack",
      "kind": "deep_link",
      "url_template": "{slack_add_url}",
      "style": "primary",
      "opens_new_window": true,
      "copyable": true,
      "requires": ["slack_add_url"],
      "visible_when": {
        "setup_status.ok": true
      }
    }
  ]
}
```

Initial providers expected from `greentic-messaging-providers`:

- Slack: `Add to Slack`, requiring `slack_add_url`
- Teams: `Add to Teams`, requiring `add_to_teams_url`
- WebEx: `Add to WebEx`, requiring `bot_email`, URL resolved from `https://web.webex.com/teams/messages/new?email={bot_email}`
- Telegram: `Add to Telegram`, requiring `bot_username`, URL resolved from `https://t.me/{bot_username}`

## Screen Trigger

At setup completion, collect actions only from discovered setup targets for the current bundle, tenant, and team. Do not scan every included pack: secrets providers, deployer packs, app packs that are not setup targets, and other supporting packs must not add final-screen actions just because they are present in the bundle.

For each action:

1. Verify `visible_when` against the public setup-machine/backend-contract state exposed for that provider.
2. Verify every `requires` value can be resolved.
3. Resolve `url_template` with public setup outputs and public rendered setup values. Do not read raw persisted answers, raw runtime config, or server-owned keys directly.
4. Validate the resolved URL scheme. Allow `https:` initially; add explicit support for other schemes only when a provider contract requires it.
5. If the action resolves successfully, include it on the final screen.

If no actions resolve, keep the current completion behavior and do not show the additional screen.

A missing required value should not fail the whole setup if setup itself succeeded. It should suppress that specific action and emit a clear diagnostic so the provider setup can be fixed. Providers that need an add link must publish it as an explicit non-secret setup output, for example `add_to_teams_url`; the resolver should not derive final links from private tokens or hidden config.

## UX Requirements

The screen title should be direct, for example `Share add buttons`.

The explanatory copy should say that users need a link or button to add the digital worker to their workspace or chat app, and that admins can share the links directly or add the HTML buttons to an internal page.

For each resolved action, show:

- the provider/action label
- a working visible button using the provider action label
- the resolved URL in a copyable text field
- a copyable HTML snippet for embedding the button

Example HTML snippet:

```html
<a class="greentic-add-button greentic-add-slack" href="https://..." target="_blank" rel="noopener noreferrer">Add to Slack</a>
```

The generated snippet must HTML-escape the URL and label. The class name can include a sanitized action or provider identifier, but the snippet must remain usable without project-specific CSS.

When multiple providers request buttons, render one repeated action block per provider/action in bundle/provider order. The screen should not assume only one action per provider.

## State And Navigation

This final screen is post-setup guidance, not a blocking setup step.

Requirements:

- setup remains successful even if the user closes the screen
- the screen can be revisited from setup summary/history when action metadata is still available
- copy interactions do not mutate provider state
- provider setup status remains the source of truth for `visible_when`
- both setup-machine providers and backend-contract providers can expose final actions through the same resolved action model

## Acceptance Criteria

- No provider actions: current completion screen/exit behavior is unchanged.
- One provider action: the final screen shows one button, one URL, and one HTML snippet.
- Multiple provider actions: the final screen shows all resolved actions independently.
- Missing required values suppress only the affected action and produce a diagnostic.
- URL and HTML values are escaped and copyable.
- The implementation uses existing `greentic.setup.actions.v1` pack-extension metadata; it does not special-case Slack, Teams, WebEx, or Telegram.
- The resolver ignores or rejects legacy `setup.yaml setup_actions` and runtime `pending_setup_actions` for this final screen, while continuing to accept the `greentic.setup.actions.v1` pack extension.
