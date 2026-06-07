# Provider Setup Web Components And Backend Contracts

Provider packs can declare provider-owned setup UI with
`greentic.setup.web-component.v1`. `greentic-setup` mounts the declared custom
element and owns the generic wizard audit trail.

Provider packs can also declare setup-owned backend behavior with
`greentic.setup.backend-contract.v1`. This is a setup backend contract, not a
runtime contract. `gtc setup` / `greentic-setup` owns admin setup. `gtc start` /
`greentic-start` owns runtime message routing and may report runtime evidence
such as "first inbound provider webhook received", but it must not configure
admin/cloud/app resources.

When a setup backend contract exists, `greentic-setup` handles the declared
`/v1/messaging/setup/{provider}/{tenant}` setup routes locally. A development
runtime proxy via `GREENTIC_SETUP_RUNTIME_URL` remains a fallback for packs
without a setup backend contract, or for explicit local development harnesses.

The current generic backend-contract host supports provider-agnostic state,
config persistence, server-owned key stripping, and clear unsupported-action
errors when a contract does not provide executable generic action metadata.
Provider-specific provisioning must be expressed as generic contract actions or
implemented through an explicit extension point; it should not be hardcoded in
the setup UI host.

## Setup Backend Contract

`greentic.setup.backend-contract.v1` describes setup-owned backend routes and
state ownership. The host reads the contract from the installed pack and exposes
it in provider metadata for diagnostics.

The extension can either contain the full contract inline, or it can be an
asset descriptor:

```json
{
  "schema_id": "greentic.setup.backend-contract.v1",
  "provider_id": "example-provider",
  "asset": "assets/setup/backend-contract.json"
}
```

When `asset` is present, `greentic-setup` loads that JSON file from the pack and
uses it as the effective runtime contract. The asset must be a safe relative
pack path, must have the same schema id, and must match the declaring provider
id. If the asset cannot be loaded, or the effective contract has no
`required_order`, setup is reported as blocked. An empty contract is never
treated as complete.

The host persists contract state under the active bundle using the setup scope:

```text
state/setup-backends/{env}/{tenant}/{team}/{provider_id}.json
```

Browser-submitted config is filtered through the contract's
`server_owned_config_keys`. Keys such as OAuth device-code state and access
tokens are ignored when submitted by the browser. Those values must be created
and persisted server-side only.

If the effective contract declares `actions_schema_id:
"greentic.setup.actions.v1"` and an `actions` array, `POST
/v1/messaging/setup/{provider}/{tenant}/next` executes the first pending
`required_order` step by finding the action with the same `id` and dispatching
on `action.executor.kind`. The host does not inspect provider-specific step
names.

The generic executor kinds currently handled by `greentic-setup` are:

- `oauth_device_code`
- `microsoft_graph_application`
- `bot_framework_registration`
- `microsoft_graph_teams_app_catalog_publish`
- `microsoft_graph_teams_app_user_install`
- `runtime_observation`

Executors must provide the metadata needed by that kind, such as config keys,
token store keys, asset paths, state store keys, completion paths, and URL
templates. Unknown executor kinds return a clear error naming the missing kind.

Before executing actions, `greentic-setup` injects generic host-derived defaults
into setup backend config. This includes `public_base_url` from setup/public
environment variables, an active setup tunnel, persisted static-routes/runtime
endpoint artifacts, or the local runtime proxy in development. It also includes
known host capability URLs such as `bot_framework_registration_url` when a
compatible runtime is available through `GREENTIC_SETUP_RUNTIME_URL`.

Contracts should reference host-provided values through generic placeholders
such as `{public_base_url}` and `{bot_framework_registration_url}`, or through
future explicit metadata such as `host_capability:
"bot_framework_registration"`. If the setup host cannot provide a required
capability, the action returns a blocked result naming the missing capability
instead of asking the user for internal URLs.

The route response shape remains generic and component-friendly:

```json
{
  "ok": true,
  "values": {
    "config": {},
    "last_setup_result": null,
    "backend": null
  },
  "setup_status": {
    "ok": false,
    "items": [],
    "selected": {},
    "blocked": null,
    "last_step": null,
    "next": "Click Run next setup step."
  }
}
```

Completion is still correlated generically by provider id and
`setup_status.ok`.

## Event Logging Contract

When a provider setup component is mounted, `greentic-setup` listens for generic
DOM events whose names start with `greentic-provider-setup-`, including:

- `greentic-provider-setup-state`
- `greentic-provider-setup-request-start`
- `greentic-provider-setup-request-success`
- `greentic-provider-setup-request-error`
- `greentic-provider-setup-action-start`
- `greentic-provider-setup-action-complete`
- `greentic-provider-setup-action-timeout`
- `greentic-provider-setup-result`
- `greentic-provider-setup-error`
- `greentic-provider-setup-device-login`
- `greentic-provider-setup-complete`

Events must include `detail.providerId` or `detail.provider_id` so the setup UI
can correlate the event to the mounted provider. Logging is best-effort: a log
write failure must not block or fail the setup wizard.

For request/action events, providers should include any available safe
diagnostic fields in `detail`, such as current step id, progress, action name,
request method/path, HTTP status, response body/error, and correlation ids.

The browser posts sanitized event records to:

```text
POST /api/provider-setup-events
```

Recent events can be read for diagnostics:

```text
GET /api/provider-setup-events?provider_id=<provider>&tenant=<tenant>&team=<team>&env=<env>
```

## Log Location

Events are stored as JSONL under the active bundle:

```text
state/logs/setup/{env}/{tenant}/{team}/{provider_id}.jsonl
```

Each record includes:

- `timestamp` as Unix milliseconds
- `tenant`
- `team`
- `env`
- `provider_id`
- `event_name`
- `current_step_id`
- `current_progress`
- `action_name`
- `request_method`
- `request_path`
- `http_status`
- `response_body`
- `error`
- `correlation_id`
- `event_detail`
- `setup_session_id`
- `setup_ui_url`

For generic backend-contract `/next` execution, `greentic-setup` also appends a
`greentic-provider-setup-backend-next` record. Its `event_detail` includes the
incoming request path/body, selected contract step, selected executor, resolved
capability route or URL templates, upstream/runtime URL and status/body when
available, and setup state before and after execution.

The setup UI includes an Advanced diagnostics panel showing the most recent 10
provider setup diagnostics for the mounted provider. If a provider component
reports a timeout or request error, the UI uses the most recent relevant
diagnostic details instead of only showing a generic timeout message.

Provider runtimes may keep their own deeper runtime logs. The JSONL files above
are the generic `greentic-setup` wizard audit trail and should be sufficient to
reconstruct what the setup UI observed.

## Redaction

The browser sanitizes event details before sending them, and the backend applies
the same redaction again before writing JSONL.

The following keys are redacted recursively, ignoring case, `_`, and `-`:

- `access_token`
- `refresh_token`
- `id_token`
- `client_secret`
- `bot_app_password`
- `device_code`
- `oauth_device_code`

`user_code` is not stored directly. It is replaced with a short hash marker so
support can correlate repeated events without retaining an active code.

Safe diagnostic fields such as `error`, `error_codes`, `status`, `step`, `next`,
`trace_id`, `correlation_id`, `request-id`, and `client-request-id` are retained.
