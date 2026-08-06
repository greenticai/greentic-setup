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

## Cross-bundle secret collisions (the guard)

The env store above is **one file per environment**, keyed by
`secrets://{env}/{tenant}/{team}/{provider}/{key}` — there is **no bundle
segment**. Two different bundles under the same tenant (and same/absent team)
therefore compute the exact same address for the same provider+key. Before
this guard existed, the second `gtc setup` silently overwrote the first, and
both bundles then resolved the second bundle's value — e.g. two Telegram bots,
one per bundle, both ending up pointed at the same bundle's token.

This is deliberately **not** fixed by making the address bundle-unique — an
earlier attempt did that and converted every reader; three whole-branch
reviews found ten Criticals and it was abandoned (see
`docs/superpowers/specs/2026-08-06-secret-collision-guard-design.md`, "Why the
previous approach was abandoned"). The addressing stays exactly as documented
above.

Instead, `secret_collision::check` (called from inside
`persist_all_config_as_secrets`, in the loop that builds the seed entries) is
consulted before every write:

- If the store holds **nothing** at the address, the write proceeds — nothing
  to collide with.
- If the store holds the **same value** already being written, the write
  proceeds — two bundles deliberately sharing one credential (e.g. one shared
  Telegram bot) keeps working, and it stays idempotent.
- If the store holds a **different** value, and this bundle's own recorded
  answers (`existing_answers`, threaded through as the last parameter of
  `persist_all_config_as_secrets`) show no record of this key, the write is
  **refused** with an error naming the tenant/team, provider, and key, and
  telling the operator to re-run with a distinct `--team` — the same
  `(tenant, team)` scope already used to separate multi-workspace setups
  elsewhere in this repo.
- If the store holds a different value but this bundle's own answers already
  had that key (e.g. rotating its own token on a re-run), the write proceeds —
  this is the bundle updating its own secret, not a collision.
- A store read that fails for a reason other than "not found" is treated as
  **occupied, not free** — the guard errs toward refusing rather than risking
  a silent overwrite it could not actually verify was safe.

This guard prevents a **new** collision; it does not repair one that already
exists. An operator whose two bundles already collided (silently, before this
guard shipped) must still re-run `gtc setup --team <name>` for one of them —
the guard only stops it from happening again going forward.

### The UI's did-I-write-this marker is session-tracked, not file-based

Every write path above (`gtc setup` apply-pack-setup, OAuth callbacks,
device-code polling, backend-contract state) builds `existing_answers` from
something already on disk for this bundle — `setup-answers.json` or the
config envelope — because each of those callers does its one persist call
*after* the prior state it needs to compare against already exists on disk.

The setup UI's debounced draft-autosave path (`persist_ui_draft`, behind
`POST /api/draft` and also invoked once at the top of `execute_setup_action`)
is different: it runs continuously while the operator is still typing, well
before `gtc setup` ever commits `setup-answers.json`. A file-based read would
be empty on every autosave before commit, which would refuse every changed
keystroke as a false collision — worse than the bug, since it would fire
continuously rather than once. It must also **not** use the value map being
written as its own marker: the key being written is by construction always
present in that map, so `check()`'s `contains_key` would always find it and
the guard could never fire — that shape shipped once, and it was a bug (a
draft session's own second autosave could silently overwrite a value another
bundle had already committed to the same address; see the round-1 review of
this task for the reproduction).

Instead, `UiState.autosaved_answer_keys` tracks, per provider, the keys *this
wizard session* has itself successfully autosaved so far. A key already in
that set is this same session continuing to type or refine (e.g. a
registration op replacing a placeholder `client_id` with the value the
provider's own API just issued) — allowed. A key that is **not** in that set,
with the store already holding a different value under it, belongs to another
bundle — refused, on the first autosave that collides. `execute_setup_action`'s
second persist call (after registration) merges this session-tracked set with
the on-disk `setup-answers.json` read, so both "this session just drafted it"
and "a prior committed run already had it" count as this bundle's own.

### Generated secrets and requirement-key aliases have no did-I-write-this marker

`persist_all_config_as_secrets` has two more write paths besides its main
loop, and both now call `secret_collision::check` too — but neither has a
file-based marker to hand it, for different reasons.

**Generated secrets** (`generated_secrets::introduce_into_store`, e.g.
`messaging-webchat-gui`'s `jwt_signing_key`) are never operator answers: setup
synthesises the value itself, so the key never appears in
`state/config/<provider_id>/setup-answers.json`. There is also no way to use
the *stored value* as a stand-in marker the way the main loop's "same value
already there" shortcut does, because [`generate_secret_value`] mints a fresh
random value on every call — even this same bundle regenerating its own
secret would essentially never produce byte-identical output. In practice
this only matters when a pack opts into `regenerate_if_present: true` (the
default, `false`, never reaches a write once a value is present — the
existing "skip if already there" check returns first). `existing_answers` is
threaded through this call anyway, for consistency with the other guarded
callers, but it is honest to say plainly: for this path it is always a
no-op, and the guard runs fail-closed — once a `regenerate_if_present: true`
secret has been written once, any later write to that address, including by
the same bundle, is refused unless a distinct `--team` is used. As of this
writing no pack in the workspace declares `regenerate_if_present: true` (every
instance uses `false`), so this is a real but currently unexercised
trade-off, not a regression of an observed working flow. It is pinned by
`generated_secrets::tests::same_bundle_regenerating_an_existing_secret_is_refused_fail_closed`
so the day a pack does flip the flag, CI catches the refusal instead of an
operator discovering it in production.

Because this path cannot tell a different bundle from this same bundle on a
later run, it does not use `secret_collision::message` — that function
asserts "written by a different bundle" as fact, which may be false here.
It uses `secret_collision::message_unattributed` instead, which states only
what the guard actually verified (this run did not write the current value)
and offers the same `--team` remedy as a possibility rather than a
diagnosis. `message` keeps the stronger, accurate wording for the two paths
below, whose marker really is this bundle's own recorded answers.

**Requirement-key aliases** (`seed_secret_requirement_aliases`, e.g.
`webex_bot_token` aliasing `bot_token`) are guarded independently from the
primary answer they mirror, because each alias occupies its own address in
the one-file-per-env store — a different bundle can hold a different value
under the alias address even when the primary address is completely
uncontested. The alias write does have a usable marker, but it is not the
alias's own key: `canonical_req_key` (e.g. `webex_bot_token`) is a derived
requirement-key spelling that never appears literally in
`setup-answers.json`, so checking `existing_answers` under it would read "not
mine" on a bundle's own legitimate re-run. The check instead runs under the
*primary* answer key (e.g. `bot_token`) — the question this guard needs
answered is "did this bundle ever record the answer this alias mirrors?" —
and the returned `Collision`'s `key` field is corrected to the alias key
before it reaches the operator-facing message, so the error still names the
address that actually collided.

### What remains uncovered

- **`greentic-start`'s onboarding wizard** does not call `secret_collision::check`
  yet — that is a separate, not-yet-started task. Until it lands, a collision
  introduced through `greentic-start`'s own write path is not caught.

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
