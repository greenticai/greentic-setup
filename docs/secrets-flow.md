# Secrets flow — greentic-setup (the WRITE side)

Guidance for Claude Code (and humans) on how setup persists secrets, and the
cross-system contract it shares with **greentic-start**, which READS them. Read
this together with greentic-start's mirror doc: `docs/secrets-flow.md` in that
repo (the READ side). The two MUST stay in agreement — a mismatch makes a
setup-provisioned secret silently go "missing" at runtime, which has been a
recurring bug.

## The one-line contract

> greentic-setup writes a secret at a canonical URI into the **env store**;
> greentic-start reads the **same URI** from the **same file**. Same string,
> same path, both sides.

Both halves are pinned by DO-NOT-CHANGE guard tests (see below). Do not edit
either side's derivation without a new secrets plan verified end-to-end.

## Secret URI grammar

```
secrets://{env}/{tenant}/{team}/{provider}/{key}
```

| Segment    | Rule                                                             |
|------------|-----------------------------------------------------------------|
| `env`      | environment id, e.g. `local`, `dev`, `prod`                     |
| `tenant`   | the tenant setup is provisioning for                            |
| `team`     | team, or `_` when the team is `default`/absent                  |
| `provider` | pack provider slug, **hyphens → underscores** (`messaging-webex` → `messaging_webex`) |
| `key`      | secret key, e.g. `webex_bot_token`                              |

Built by `canonical_secret_uri(env, tenant, team, provider, key)` in `lib.rs`.
Canonical example: `secrets://local/demo/_/messaging_webex/webex_bot_token`.

## Where setup MUST write: the env store

There are two on-disk DevStore files, and picking the wrong one is the classic
seam:

- **Env store** — `‹LocalFsStore root›/‹env›/.greentic/dev/.dev.secrets.env`.
  This is the file **greentic-start reads** at serve time. **All write paths must
  target this**, keyed by the explicit `env` — never `$GREENTIC_ENV`.
- **Bundle-local store** — `‹bundle_root›/.greentic/dev/.dev.secrets.env`.
  Legacy/`$GREENTIC_ENV`-gated. Under `gtc setup`, the UI-server process runs
  with `$GREENTIC_ENV` **unset**, so any bare (env-implicit) write lands here —
  where the runtime never looks.

### Use the env-explicit helpers on every write path

`secrets.rs`:

- `open_dev_store_for_env(bundle_root, env)` — opens the env store for an explicit
  `env`. **Prefer this on all write paths.**
- `ensure_path_for_env(bundle_root, env)` — the same path resolution without
  opening.
- `open_dev_store(bundle_root)` / `ensure_path(bundle_root)` — bare forms that
  gate on `$GREENTIC_ENV`. Fine for tests (isolation), dangerous on real write
  paths.

The webex_bot_token bug: `persist_all_config_as_secrets` computed the env path
but opened the **bundle-local** store (`open_dev_store(bundle_root)`), so every
secret landed where the runtime never reads. Fixed by opening
`open_dev_store_for_env(bundle_root, env)`. Write paths now audited:
`qa/persist.rs` (`persist_all_config_as_secrets`, `persist_qa_results`),
`oauth_callback.rs`, and `engine/executors.rs` (registration store).

## Write path (this repo)

The per-provider persist emits redacted WRITE logs (`setup secret WRITE … uri=…
value_len=N store_path=…`, never the value) so the journey is auditable against
the runtime's READ log. Note: `greentic-setup` has no info-level `tracing`
subscriber under `gtc setup`, so these `tracing::info!` lines only surface when a
subscriber is installed — for a live probe, prefer `eprintln!`/operator logging.

Alias seeding: `seed_secret_requirement_aliases` (from `secret-requirements.json`)
lets WASM components look secrets up by their canonical requirement key
(`WEBEX_BOT_TOKEN` → `webex_bot_token`) even when the answers file used a shorter
key. When `pack_path` is `None` the aliases are NOT seeded (logged as a warning) —
short answer keys may then be unresolvable at runtime.

## The guard tests (DO NOT CHANGE)

- **This repo:** `lib.rs::webex_secret_uri_contract_do_not_change`
  — asserts `canonical_secret_uri(...) == the exact URI the runtime reads`.
- **greentic-start:** `secrets_gate.rs::webex_secret_read_uri_contract_do_not_change`
  — asserts the runtime's `canonicalize_provider_segment` lands on the same
  golden string.

Both hard-code `secrets://local/demo/_/messaging_webex/webex_bot_token`. They are
intentionally brittle: if either derivation drifts, the build breaks before a
user hits a silent "missing secret." Changing the scheme requires a new plan
verified on **both binaries** (setup + start), **both backends** (local dev-store
+ cloud vault), and public.

## Checklist when touching secret persistence

1. Open the store with `open_dev_store_for_env(bundle_root, env)` — never the bare
   form on a real write path.
2. Build URIs with `canonical_secret_uri` — do not hand-assemble.
3. Keep team `default` → `_` and provider hyphen → underscore.
4. If the runtime can't find it, compare setup's WRITE `store_path` against the
   runtime's READ store path — they must be the same env-store file.

See also: greentic-start `docs/secrets-flow.md` (the READ side).
