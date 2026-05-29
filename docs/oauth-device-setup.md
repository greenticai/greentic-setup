# OAuth Device-Code Setup Metadata

Providers can use `messaging.oauth_device_code.v1` metadata to complete setup after a device-code login. The setup runner maps token response fields through `secrets_out` and `config_out`, then writes secret outputs to the dev secrets store and non-secret config outputs to `state/config/<provider>/setup-answers.json`.

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

Persist both the ID and label when runtime behavior depends on a selected resource. IDs are for API calls; labels are for readable setup state, diagnostics, and later UX.

## Interaction Modes

Keep instructions aligned with the actual provider mode.

- App or bot modes should provide explicit setup action instructions for installing, opening, or searching for the app/bot.
- Channel or subscription modes should describe resource selection and subscription setup. Do not describe a channel/subscription integration as a searchable bot unless the provider also exposes a real bot/app interaction path.

Mode-specific instructions belong in setup action metadata such as `instructions`, `success_message`, and provider-specific labels. Discovery metadata should stay focused on selecting and persisting resources.
