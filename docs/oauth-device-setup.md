# OAuth Device-Code Setup Metadata

Providers should model OAuth device-code setup through the generic setup
contracts, not through legacy pending setup actions.

For new provider setup machines, declare an `oauth_device_code` step in
`greentic.setup.machine.v1`. The runner stores transient device-code session
state under the provider's scoped setup state directory, redacts public output,
polls on resume/retry, and persists access or refresh tokens only through the
server-side secrets path.

For providers still migrating through `greentic.setup.backend-contract.v1`,
declare an action whose `executor.kind` is `oauth_device_code`. The backend
contract runner uses the same generic setup state layout:

```text
state/setup/{tenant}/{team}/{provider}/backend-contract.json
```

Legacy `setup_actions` and `pending_setup_actions` are no longer executed by
bundle setup. Existing legacy state should be archived with
`greentic-setup bundle setup-migrate <provider-id>` and the provider should
move setup behavior into a setup machine or backend contract.

## Finalization Provisioning

OAuth device-code setup can run provider finalization after login by following
the OAuth step with a generic `provider_component_call`, `provider_http`, or
provider-specific backend-contract action. In a setup machine this looks like:

```json
{
  "version": 1,
  "id": "example-setup",
  "entry_step": "login",
  "steps": [
    {
      "id": "login",
      "kind": "oauth_device_code",
      "authority_url_template": "https://login.example.com/{tenant}",
      "client_id": "example-client-id",
      "scopes": ["example.read"]
    },
    {
      "id": "finalize",
      "kind": "provider_component_call",
      "component_ref": "messaging-provider-example",
      "op": "apply-answers",
      "idempotency_key": "finalize:{tenant}",
      "request": {
        "tenant": "{tenant}",
        "team": "{team}",
        "login": "{oauth_device_login}"
      }
    }
  ]
}
```

The provider result must not return `ok:false`; otherwise the setup step is
blocked and retryable. Returned config should be persisted through explicit
`persist_runtime_config` mappings or backend-contract state updates, with token
fields kept out of `setup-answers.json`.

## Discovery Selection

`post_login_discovery` steps can fetch provider resources after login and select an item from a returned array.

```json
{
  "id": "rooms",
  "url": "https://api.example.com/v1/rooms",
  "select": {
    "from": "value",
    "label": "displayName",
    "value": "id",
    "save_as": "room_id",
    "label_save_as": "room_name",
    "default_filter": "{{ bundle_name }}"
  }
}
```

Selection fields:

- `from`: JSON path to the selectable array.
- `label`: JSON path inside each item used for display and label persistence.
- `value`: JSON path inside the selected item used as the stable ID.
- `save_as`: setup answer key for the selected ID.
- `label_save_as`: optional setup answer key for the selected label. When omitted, `*_id` keys also save `*_name`, and other keys save `*_label`.
- `default_label`: optional exact label to prefer.
- `default_filter`: optional label substring to prefer. `{{ bundle_name }}` is replaced with the bundle display name.

Persist both the ID and label when runtime behavior depends on a selected
resource. IDs are for API calls; labels are for readable setup state,
diagnostics, and later UX. In setup machines, use `select_from_json` followed by
`persist_runtime_config`; in backend contracts, store the selected values in the
generic setup state and map only non-server-owned fields into runtime config.

## Interaction Modes

Keep instructions aligned with the actual provider mode.

- App or bot modes should provide explicit setup-step instructions for installing, opening, or searching for the app/bot.
- Channel or subscription modes should describe resource selection and subscription setup. Do not describe a channel/subscription integration as a searchable bot unless the provider also exposes a real bot/app interaction path.

Mode-specific instructions belong in setup machine/backend-contract metadata
such as step labels, `instructions`, `message_key`, `success_message`, and
provider-specific labels. Discovery metadata should stay focused on selecting
and persisting resources.
