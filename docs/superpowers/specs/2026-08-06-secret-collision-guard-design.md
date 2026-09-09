# Secret collision guard

**Date:** 2026-08-06
**Repos:** `greentic-setup`, `greentic-start`
**Lane:** 1.1 (`main`) first, then forward-port to 1.2 (`develop`)
**Replaces:** the bundle-scoped secret addressing attempted in `greentic-setup#255` and
`greentic-start#489`. See "Why the previous approach was abandoned".

## Problem

Maarten reported: create two Telegram bots, assign one to each bundle, and both bots end
up using the same bundle.

Root cause, verified in source: the dev secret store is one file per environment, resolved
from `$HOME` and not from the bundle —

```rust
// greentic-setup/src/secrets.rs:64
LocalFsStore::default_root().map(|root| root.join(env).join(".greentic/dev/.dev.secrets.env"))
```

— keyed by a five-segment URI with no bundle component:

```
secrets://{env}/{tenant}/{team}/{provider}/{key}
```

Two bundles under one tenant therefore compute an identical key into an identical file.
The second `gtc setup` silently overwrites the first, and both bundles then resolve the
second bundle's token.

## Why the previous approach was abandoned

The first attempt added the bundle as a new identity axis: the writer minted a
bundle-unique address and recorded the resulting `secrets://` URI in the bundle's own
answers file, and every reader was converted to read that recorded address.

That failed, three times, in the same way. Bundle is not an identity axis anywhere in this
platform — it is absent from `SecretUri { scope, category, name, version }`, from
`SetupConfig { tenant, team, env, offline, verbose }`, and from
`greentic_types::TenantCtx`. Introducing it meant teaching every address-derivation site
about it, and there are many, spread across both repos and reached by paths that do not
share a chokepoint.

Three whole-branch reviews found ten Critical defects between them. Each round fixed the
sites it knew about and thereby created a fresh asymmetry with a site it did not:

- Round 1 (4 Critical): readers in `oauth_callback.rs` and `ui/mod.rs` still derived the
  bare address; the answers file — newly load-bearing — was blind-overwritten by three
  writers; every config key, not just secret-marked ones, was recorded as a ref, so a
  `secrets://` string could reach `oauth_device::lookup_client_id` and be sent to Microsoft
  as an OAuth client id.
- Round 2 (1 Critical): the answers path had no tenant fallback, so under a gtunnel alias
  tenant the reader silently fell back to the shared address — the very collision being
  fixed — and all five answers readers returned nothing.
- Round 3 (5 Critical): the mint was unconditional while the record was conditional, so
  non-secret keys got a fresh random address on every run and the shared store grew without
  bound; `migrate_backend_state` permanently deleted the recorded-ref map, making Teams
  backend OAuth secrets unrecoverable; requirement-key aliases landed at the suffixed
  category, recorded nowhere, so consumers missed them and a placeholder was seeded — and
  the provider-secret gate then passed on a fake credential.

One of those Criticals survived controller-run mutation testing: the mutation proved the
code's behaviour changed, but both consumers of the changed function gate on
`!info.secret`, so the branch that was proven was the branch nobody observes.

The lesson is not that the implementations were careless. It is that changing an
addressing scheme in a codebase with many independent derivation sites cannot be done
incrementally without a chokepoint, and this codebase has none.

## Design

Do not change the addressing scheme. Detect the collision at the moment it would happen
and stop, with a message that tells the operator how to proceed.

If `gtc setup` refuses, the runtime never sees a corrupted store — so this is a write-side
guard only. No reader changes, in either repo.

### The discriminator

The store records a value at an address but not who wrote it. The bundle's own
`state/config/<provider>/<tenant>/<team>/setup-answers.json` already records which keys
that bundle wrote. That is enough:

| Address holds a value | This bundle has a record for the key | Conclusion |
|---|---|---|
| yes | yes | this bundle wrote it — an ordinary re-run |
| yes | no | another bundle wrote it |
| no | — | nothing to collide with |

The answers file is consulted only as a *did I write this* marker. It is not an address
store, nothing is unrecoverable if it is lost, and no reader depends on it. That is a far
weaker dependency than the abandoned design placed on the same file.

### The trigger

Refuse only when the **value differs**.

Writing the same value is idempotent and harms nobody, so deliberate sharing — one LLM API
key used by several bundles under one tenant — keeps working. What is refused is the
overwrite that actually breaks the first bundle.

`retain_changed_entries` (`greentic-setup/src/qa/persist.rs:127-142`) already performs a
per-URI comparison against the store to drop unchanged entries before seeding. The
implementation should extend that existing comparison rather than introduce a second,
subtly different one — but read it first and confirm what it actually compares, because
the exact semantics were not re-verified while writing this spec.

### Behaviour on collision

A hard error, not a warning. This is the last point at which the operator can still fix
the situation before anything is damaged, and a warning in a long setup log is a warning
nobody reads.

The message must name the tenant, the provider, the key, and the remedy:

```
ERROR: tenant 'demo' already holds bot_token for messaging-telegram,
written by a different bundle.

Two bundles cannot share one (tenant, team). Re-run with a distinct team
to separate them, for example:

    gtc setup --team bot-support
```

### Both write paths

The same guard applies at both places an operator can provision a provider secret:

1. `greentic-setup` — `qa::persist::persist_all_config_as_secrets`, the `gtc setup` path.
2. `greentic-start` — `qa_persist::persist_qa_secrets`, reached from the onboarding wizard
   at `onboard/wizard.rs:377` via `POST /api/onboard/qa/submit`.

Note that `greentic-start/src/admin_server.rs:1117` calls `greentic-setup`'s
`persist_qa_results` from the published crate, so that door inherits the setup-side guard
automatically once setup publishes.

The check is the same rule in both places, and it MUST be impossible for the two to drift
apart silently. A guard that fires in one repo and not the other is worse than no guard:
it makes the collision look handled while leaving one door open.

The plan must satisfy that requirement one of two ways, and state which:

- a shared definition both repos depend on, or
- a faithful twin with a golden test on each side pinning the same decision for the same
  inputs — the pattern this codebase already uses for
  `webex_secret_read_uri_contract_do_not_change`.

The second is acceptable only if the golden tests genuinely discriminate. Three tests of
exactly that shape were found during the abandoned attempt to have stopped guarding
anything: one validated an address nobody wrote any more, one exercised a helper rather
than the contract, and one bypassed the code path it claimed to cover.

## Testing

Four cases. Three of them must NOT fire — a guard that refuses too much blocks flows that
have always been legitimate, and that failure is more visible to operators than the bug it
replaces.

| Scenario | Expected |
|---|---|
| Two bundles, one tenant, **different** values | **Error** |
| Two bundles, one tenant, **same** value | passes — deliberate sharing |
| Same bundle, `gtc setup` re-run | passes — it has its own record |
| Two bundles, **different** `--team` | passes — already separated |

Every test must fail if the guard is removed. Verify that by reverting the guard and
capturing the actual failure, not by reading the test.

## What is kept and what is discarded

**Kept, untouched:**

- Bug (A), the tunnel fix, already shipped as `greentic-start` PR #489.
- The tenant-scoped **answers** work from #255. That fixes a different, real bug — one
  bundle set up under two tenants overwrote the first tenant's answers — and has nothing
  to do with secret addressing.

**Discarded:** the bundle-scoped secret addressing on both branches — `secret_ref.rs`, ref
recording, the answers-file merge that existed to protect those refs, `secret_resolve.rs`,
the unified resolver, the tenant-alias fallback, and `resolve_write_uri`.

Separating the kept half of #255 from the discarded half is a delivery question for the
implementation plan, not a design question.

## Known limits, not closed by this design

- **The DekCache bug** (`greentic-secrets-core/src/crypto/dek_cache.rs:19`) is real and
  unrelated to this change: `CacheKey { env, tenant, team, category }` omits the secret
  name, so two names under one category fail AEAD on the second decrypt through a shared
  store handle. It affects any provider with more than one secret key. Repro and suggested
  fix are written up separately; this design neither worsens nor fixes it.
- **This guard prevents a new collision; it does not repair an existing one.** An operator
  whose store is already in the collided state must re-run setup with a distinct team.
- **Nothing detects the collision after the fact.** If a value is overwritten by some path
  neither guard covers, the runtime still resolves the wrong token silently.
