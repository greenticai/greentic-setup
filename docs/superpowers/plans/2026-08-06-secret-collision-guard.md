# Secret Collision Guard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop `gtc setup` and the onboarding wizard from silently overwriting a secret another bundle already owns, and tell the operator how to separate the two bundles instead.

**Architecture:** One guard, defined once in `greentic-setup` and exported. `greentic-start` calls the exported function rather than reimplementing it, so the two doors cannot drift. No change to the `secrets://` addressing scheme, and no reader changes in either repo.

**Tech Stack:** Rust 1.95.0 (pinned via `rust-toolchain.toml`), edition 2024, `anyhow`/`thiserror`, `tokio`, `greentic-secrets-lib`.

## Global Constraints

- **Design doc:** `docs/superpowers/specs/2026-08-06-secret-collision-guard-design.md`. Read it before Task 1 — especially "Why the previous approach was abandoned", which explains what NOT to build.
- **Worktrees:** `greentic-setup` → `~/.cache/wt/setup-guard` (branch `fix/secret-collision-guard`, off `origin/main`). `greentic-start` → a fresh worktree off `origin/main`, created in Task 4. Never work in `/home/bima-pangestu/projects/Works/greentic/greentic-{setup,start}` — those checkouts are on `develop` and another session may be using them.
- **Lane:** both branches target `main` (1.1). Forward-port to `develop` (1.2) is Task 6.
- **Do NOT change the `secrets://` address shape, the five-segment URI, or any reader.** The whole point of this design is that addressing is untouched. If a task seems to require an addressing change, stop and report BLOCKED.
- **Commit style:** Conventional Commits (`feat:`, `fix:`, `refactor:`, `docs:`).
- **`greentic-start` FORBIDS Claude co-author trailers on commits.** `greentic-setup` permits them: end those commit messages with exactly `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.
- **English only** in source, tests, comments, commit messages and tracing logs.
- **No `unwrap()` / `panic!()` in production paths.** Test code may use `.expect("msg")`.
- **`#![forbid(unsafe_code)]`** is the norm — do not add `unsafe`.
- **Never build under `/tmp`** — it is tmpfs on this machine and cargo will eat RAM.
- **Per-task gate:** `cargo fmt --all`, the task's own tests, then `cargo clippy --all-targets --all-features -- -D warnings`.
- **Full gate once per repo before pushing** (Task 5): `bash ci/local_check.sh`. It builds the whole dependency tree in release and runs `cargo publish --dry-run`; running it per task wastes many minutes. Git hooks in both repos are empty files and enforce nothing, so Task 5 is the only thing between this work and CI.
- **Every test must fail if its guard is removed.** Revert the production change, run the test, capture the actual failure output, restore. Six tests during the abandoned attempt looked convincing and proved nothing; each was caught only at this step.

---

## File Structure

**`greentic-setup`** (worktree `~/.cache/wt/setup-guard`)

| File | Responsibility |
|---|---|
| `src/secret_collision.rs` | **New.** The whole guard: the decision rule, the error type, and the operator message. Public, so `greentic-start` can call it. |
| `src/qa/persist.rs` | Calls the guard from `persist_all_config_as_secrets`, beside the existing `retain_changed_entries` comparison |
| `src/lib.rs` | `pub mod secret_collision;` |
| `docs/secrets-flow.md` | Documents the guard and the `--team` remedy |

**`greentic-start`** (worktree created in Task 4)

| File | Responsibility |
|---|---|
| `src/qa_persist.rs` | Calls `greentic_setup::secret_collision::check` before seeding |

There is deliberately no `secret_collision.rs` in `greentic-start`. `greentic-start`'s `Cargo.toml:265` already declares `greentic-setup = ">=1.1.0-0, <1.2.0-0"`, so the guard is imported, not mirrored. One definition, no golden-test pair needed, no drift possible.

---

## Task 1: Salvage the tenant-scoped answers work from #255

**Files:**
- Create: branch `fix/tenant-scoped-answers` in `greentic-setup`, off `origin/main`

**Interfaces:**
- Consumes: nothing.
- Produces: a reviewable branch containing ONLY the answers work. Nothing later in this plan depends on it — it is separated here so it is not lost when #255's secret half is abandoned.

**Background:** PR #255 mixes two unrelated fixes. The **answers** half is correct and independently valuable: answers were keyed by provider alone while every other piece of per-provider setup state is tenant-scoped, so setting one bundle up under a second tenant overwrote the first tenant's answers. The **secret-addressing** half is what three review rounds condemned. Only the first survives.

This task is bookkeeping, not engineering. It ends with a branch someone can review; it does not need to be merged before Tasks 2–4.

- [ ] **Step 1: Create the branch**

```bash
cd /home/bima-pangestu/projects/Works/greentic/greentic-setup
git fetch origin main
git worktree add ~/.cache/wt/setup-answers -b fix/tenant-scoped-answers origin/main
```

- [ ] **Step 2: Cherry-pick only the answers commits**

The abandoned branch is `fix/tenant-scoped-secrets-and-answers`. Its commits touching ONLY the answers seam are the ones to keep. Identify them with:

```bash
cd ~/.cache/wt/setup-answers
git log --oneline origin/main..origin/fix/tenant-scoped-secrets-and-answers
```

Keep the commits that touch `src/provider_answers.rs`, the answers-path call sites in `src/engine/executors.rs`, `src/oauth_device.rs`, `src/oauth_callback.rs`, and their tests. Drop everything touching `src/secret_ref.rs`, ref recording, or `merge_write_answers` — that helper exists only to protect recorded refs and has no purpose without them.

Expect conflicts: several commits mix both halves. Where a commit does, take only its answers hunks.

- [ ] **Step 3: Verify the branch stands alone**

```bash
cd ~/.cache/wt/setup-answers
cargo test --all-features --lib 2>&1 | grep -E "^test result"
```

Expected: green. If a test fails because it referenced `secret_ref`, that test belonged to the discarded half — drop it rather than porting it.

- [ ] **Step 4: Commit and report**

No push. Report the branch name and commit range so a human can decide whether to open a PR. State plainly which commits you dropped and why.

---

## Task 2: The guard

**Files:**
- Create: `~/.cache/wt/setup-guard/src/secret_collision.rs`
- Modify: `~/.cache/wt/setup-guard/src/lib.rs` (add `pub mod secret_collision;`)

**Interfaces:**
- Consumes: `greentic_secrets_lib::{DevStore, SecretsStore}`; `serde_json::Value`.
- Produces:
  - `pub struct Collision { pub tenant: String, pub team: Option<String>, pub provider_id: String, pub key: String }`
  - `pub fn message(collision: &Collision) -> String` — the operator-facing text
  - `pub async fn check(store: &DevStore, existing_answers: Option<&Value>, tenant: &str, team: Option<&str>, provider_id: &str, key: &str, uri: &str, new_value: &str) -> Option<Collision>`

**The rule, stated once:** return `Some(Collision)` when ALL of:
1. the store already holds a value at `uri`,
2. that value differs from `new_value`,
3. `existing_answers` has no record for `key`.

Otherwise `None`. Absent store value → nothing to collide with. Same value → idempotent, and deliberate sharing (one LLM key across bundles) must keep working. A record for `key` in this bundle's own answers → this bundle wrote it, an ordinary re-run.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answers_with(key: &str) -> Value {
        json!({ key: "whatever this bundle wrote" })
    }

    #[tokio::test]
    async fn a_different_value_from_another_bundle_is_a_collision() {
        let (_dir, store) = test_store_holding("secrets://dev/demo/_/messaging_telegram/bot_token", "bundle-a-token").await;
        let found = check(
            &store, None, "demo", None, "messaging-telegram", "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token", "bundle-b-token",
        ).await;
        assert!(found.is_some(), "a second bundle overwriting a different value must be refused");
    }

    #[tokio::test]
    async fn the_same_value_is_not_a_collision() {
        let (_dir, store) = test_store_holding("secrets://dev/demo/_/messaging_telegram/bot_token", "shared-token").await;
        let found = check(
            &store, None, "demo", None, "messaging-telegram", "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token", "shared-token",
        ).await;
        assert!(found.is_none(), "deliberate sharing of one value must keep working");
    }

    #[tokio::test]
    async fn this_bundles_own_re_run_is_not_a_collision() {
        let (_dir, store) = test_store_holding("secrets://dev/demo/_/messaging_telegram/bot_token", "old-token").await;
        let answers = answers_with("bot_token");
        let found = check(
            &store, Some(&answers), "demo", None, "messaging-telegram", "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token", "new-token",
        ).await;
        assert!(found.is_none(), "a bundle updating its own secret must not be refused");
    }

    #[tokio::test]
    async fn an_empty_address_is_not_a_collision() {
        let (_dir, store) = empty_test_store().await;
        let found = check(
            &store, None, "demo", None, "messaging-telegram", "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token", "first-token",
        ).await;
        assert!(found.is_none());
    }

    #[test]
    fn the_message_names_the_tenant_provider_key_and_the_remedy() {
        let text = message(&Collision {
            tenant: "demo".into(),
            team: None,
            provider_id: "messaging-telegram".into(),
            key: "bot_token".into(),
        });
        assert!(text.contains("demo"), "{text}");
        assert!(text.contains("messaging-telegram"), "{text}");
        assert!(text.contains("bot_token"), "{text}");
        assert!(text.contains("--team"), "the operator must be told how to proceed: {text}");
    }
}
```

Write `test_store_holding(uri, value)` and `empty_test_store()` as local helpers returning `(tempfile::TempDir, DevStore)`. Build the store the way the existing tests in `src/qa/persist.rs` do — read them first and follow that idiom rather than inventing one. Note those tests take a dev-store guard (`crate::secrets::test_support`); check whether your helpers need it too and say so in your report.

- [ ] **Step 2: Run them to verify they fail**

```bash
cd ~/.cache/wt/setup-guard
cargo test --all-features --lib secret_collision
```

Expected: FAIL — the module does not exist.

- [ ] **Step 3: Implement the module**

```rust
//! Refuse to overwrite a secret another bundle already owns.
//!
//! The dev secret store is one file per environment
//! (`greentic-setup/src/secrets.rs:64`) keyed by
//! `secrets://{env}/{tenant}/{team}/{provider}/{key}` — there is no bundle
//! segment. Two bundles under one tenant therefore compute the same address,
//! and the second `gtc setup` silently overwrites the first; both bundles then
//! resolve the second one's token.
//!
//! An earlier attempt made the address bundle-unique and converted every
//! reader. Three whole-branch reviews found ten Criticals; bundle is not an
//! identity axis anywhere in this platform and there is no chokepoint to add
//! one through. See
//! `docs/superpowers/specs/2026-08-06-secret-collision-guard-design.md`.
//!
//! So the addressing is left exactly as it is and the collision is refused
//! where it would happen, with a message telling the operator to separate the
//! bundles with a distinct `--team`. `team` is already threaded end-to-end.
//!
//! `greentic-start` calls this same function from its onboarding wizard rather
//! than reimplementing the rule — a guard that fires at one door and not the
//! other is worse than no guard.

use greentic_secrets_lib::{DevStore, SecretsStore};
use serde_json::Value;

/// A secret another bundle already owns at this address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collision {
    pub tenant: String,
    pub team: Option<String>,
    pub provider_id: String,
    pub key: String,
}

/// The operator-facing explanation. This is the last point at which the
/// operator can still fix the situation, so it names the remedy, not just the
/// problem.
pub fn message(collision: &Collision) -> String {
    let scope = match collision.team.as_deref() {
        Some(team) if !team.is_empty() => format!("tenant '{}' / team '{}'", collision.tenant, team),
        _ => format!("tenant '{}'", collision.tenant),
    };
    format!(
        "{scope} already holds {key} for {provider}, written by a different bundle.\n\
         \n\
         Two bundles cannot share one (tenant, team) for the same secret. Re-run with a\n\
         distinct team to separate them, for example:\n\
         \n\
             gtc setup --team <name>\n",
        key = collision.key,
        provider = collision.provider_id,
    )
}

/// `Some(Collision)` when writing `new_value` to `uri` would overwrite a value
/// this bundle did not write.
///
/// `existing_answers` is the bundle's own `setup-answers.json`, used ONLY as a
/// did-I-write-this marker — not as an address store. Nothing is unrecoverable
/// if it is missing: the guard then errs toward refusing, which is the safe
/// direction.
pub async fn check(
    store: &DevStore,
    existing_answers: Option<&Value>,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    key: &str,
    uri: &str,
    new_value: &str,
) -> Option<Collision> {
    // A read failure is NOT "no collision". Absence means nothing is there to
    // collide with; any other error means we cannot tell, and guessing "safe"
    // here is how the overwrite this guard exists to stop gets through. Treat
    // "cannot tell" the same as "occupied" and let the answers check decide.
    let held = match store.get(uri).await {
        Ok(bytes) => Some(bytes),
        Err(err) if is_not_found(&err) => None,
        Err(_) => Some(Vec::new()),
    };
    let held = held?;
    if held == new_value.as_bytes() {
        return None;
    }
    let this_bundle_wrote_it = existing_answers
        .and_then(|answers| answers.as_object())
        .is_some_and(|map| map.contains_key(key));
    if this_bundle_wrote_it {
        return None;
    }
    Some(Collision {
        tenant: tenant.to_string(),
        team: team.map(ToString::to_string),
        provider_id: provider_id.to_string(),
        key: key.to_string(),
    })
}
```

Add `pub mod secret_collision;` to `src/lib.rs` beside the other `pub mod` declarations.

You must also write `is_not_found(&err)`. `greentic-start/src/runner_host/mod.rs` already
has an `is_secret_not_found` helper that matches on the error text; read it and follow the
same discrimination rather than inventing a second notion of "absent". If the store's error
type turns out to expose a structured not-found variant, prefer that and say so in your
report — matching on strings is what the existing helper does, not what it should do.

Add a fifth test pinning this, because it is the one branch that is easy to get backwards:

```rust
    #[tokio::test]
    async fn an_unreadable_store_is_treated_as_occupied_not_as_free() {
        // A store that errors for a reason other than not-found.
        let (_dir, store) = unreadable_test_store().await;
        let found = check(
            &store, None, "demo", None, "messaging-telegram", "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token", "token-b",
        ).await;
        assert!(
            found.is_some(),
            "when the store cannot be read we must refuse, not assume the address is free",
        );
    }
```

If you cannot construct an unreadable store cleanly at this seam, say so plainly and test
`is_not_found` directly instead — do not fake the end-to-end case.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd ~/.cache/wt/setup-guard
cargo test --all-features --lib secret_collision
```

Expected: 5 passed.

- [ ] **Step 5: Prove the rule discriminates**

Change `if held == new_value.as_bytes()` to `if true`, run the tests, and confirm `a_different_value_from_another_bundle_is_a_collision` fails. Restore. Then change `if this_bundle_wrote_it` to `if false`, run, and confirm `this_bundles_own_re_run_is_not_a_collision` fails. Restore. Report both failures verbatim.

Two mutations, because the rule has two independent conditions and one test each. A single mutation would leave half the rule unpinned.

- [ ] **Step 6: Gate and commit**

```bash
cd ~/.cache/wt/setup-guard
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
git add src/secret_collision.rs src/lib.rs
git commit -m "feat(setup): add a secret collision guard

The dev store is one file per env keyed by a five-segment uri with no bundle
segment, so two bundles under one tenant compute the same address and the
second setup silently overwrites the first.

This refuses that write instead, naming the tenant, provider, key and the
--team remedy. It fires only when the value actually differs, so deliberate
sharing of one value across bundles keeps working, and only when this bundle's
own answers file has no record of the key.

No caller yet.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Call the guard from `gtc setup`

**Files:**
- Modify: `~/.cache/wt/setup-guard/src/qa/persist.rs` (`persist_all_config_as_secrets`, and beside `retain_changed_entries` at `:117-132`)
- Modify: `~/.cache/wt/setup-guard/docs/secrets-flow.md`

**Interfaces:**
- Consumes: `crate::secret_collision::{check, message, Collision}` from Task 2.
- Produces: `persist_all_config_as_secrets` gains an `existing_answers: Option<&Value>` parameter and returns `Err` on collision.

**Background:** `retain_changed_entries` (`src/qa/persist.rs:117-132`) already does exactly the comparison the guard needs — it calls `store.get(&entry.uri)` and drops the entry when the stored bytes equal the new text. An entry that survives that filter is one where the store either holds nothing or holds something different. The guard distinguishes those two cases.

**Where the call goes, and why not inside `retain_changed_entries`.** The spec says to extend that existing comparison rather than add a second one, and that is the right instinct — but `retain_changed_entries` receives only `Vec<SeedEntry>`. It has neither the answer `key` nor `existing_answers`, so it cannot apply the did-I-write-this half of the rule. Put the call in the loop that BUILDS the entries, where both are in scope, and leave `retain_changed_entries` alone.

That means the store is read twice per key on the collision path. Accept it: this runs once per `gtc setup`, not per request, and merging the two would mean threading answers into a function whose whole job is a value comparison. If you find a cleaner seam while reading, take it and explain the choice in your report.

- [ ] **Step 1: Write the failing test**

```rust
    #[tokio::test]
    async fn a_second_bundle_writing_a_different_value_is_refused() {
        // Build two bundle roots sharing one env store, the way the
        // neighbouring persist tests in this file do.
        let (bundle_a, bundle_b, env) = two_bundles_one_env();

        persist_all_config_as_secrets(
            &bundle_a, &env, "demo", None, "messaging-telegram",
            &json!({ "bot_token": "token-a" }), None, None,
        )
        .await
        .expect("first bundle writes cleanly");

        let err = persist_all_config_as_secrets(
            &bundle_b, &env, "demo", None, "messaging-telegram",
            &json!({ "bot_token": "token-b" }), None, None,
        )
        .await
        .expect_err("the second bundle must be refused");

        let text = err.to_string();
        assert!(text.contains("bot_token"), "{text}");
        assert!(text.contains("--team"), "the error must tell the operator how to proceed: {text}");
    }

    #[tokio::test]
    async fn a_second_bundle_writing_the_same_value_is_allowed() {
        let (bundle_a, bundle_b, env) = two_bundles_one_env();
        for bundle in [&bundle_a, &bundle_b] {
            persist_all_config_as_secrets(
                bundle, &env, "demo", None, "messaging-telegram",
                &json!({ "bot_token": "shared-token" }), None, None,
            )
            .await
            .expect("writing the same value must stay idempotent");
        }
    }

    #[tokio::test]
    async fn a_second_bundle_under_a_different_team_is_allowed() {
        let (bundle_a, bundle_b, env) = two_bundles_one_env();
        persist_all_config_as_secrets(
            &bundle_a, &env, "demo", None, "messaging-telegram",
            &json!({ "bot_token": "token-a" }), None, None,
        ).await.expect("first bundle");
        persist_all_config_as_secrets(
            &bundle_b, &env, "demo", Some("bot-support"), "messaging-telegram",
            &json!({ "bot_token": "token-b" }), None, None,
        ).await.expect("a distinct team is the documented remedy and must work");
    }
```

Write `two_bundles_one_env()` following the fixture idiom of the existing tests in this file — read them first. It must return two distinct bundle roots that resolve to the SAME env store, since that shared store is the whole point.

- [ ] **Step 2: Run them to verify they fail**

```bash
cd ~/.cache/wt/setup-guard
cargo test --all-features --lib a_second_bundle
```

Expected: `a_second_bundle_writing_a_different_value_is_refused` fails — today the second write silently succeeds. The other two should already pass; if either fails, stop and report, because that means the guard is not the only thing standing between these bundles.

- [ ] **Step 3: Thread the answers and call the guard**

Add `existing_answers: Option<&Value>` as the last parameter of `persist_all_config_as_secrets`. In the loop that builds `entries`, before pushing a `SeedEntry`, call the guard and convert a `Some` into an error:

```rust
        if let Some(collision) = crate::secret_collision::check(
            &store,
            existing_answers,
            tenant,
            team,
            provider_id,
            key,
            &uri,
            &text,
        )
        .await
        {
            anyhow::bail!("{}", crate::secret_collision::message(&collision));
        }
```

Update every caller to pass the bundle's current answers. On this branch the answers file is at the legacy unscoped path — read it with the same helper the surrounding code already uses, and if a caller genuinely has no answers in hand, pass `None`: the guard then errs toward refusing, which is the safe direction.

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd ~/.cache/wt/setup-guard
cargo test --all-features --lib a_second_bundle
```

Expected: 3 passed.

- [ ] **Step 5: Prove it discriminates**

Comment out the `bail!` and run `a_second_bundle_writing_a_different_value_is_refused`. Capture the actual failure. Restore.

- [ ] **Step 6: Document it**

In `docs/secrets-flow.md`, record: the store is shared per environment and has no bundle segment; two bundles under one tenant would collide; setup now refuses the second write when the value differs and names the `--team` remedy; sharing one value deliberately still works; and the guard prevents a NEW collision but does not repair one that already exists — an operator already in that state must re-run with a distinct team.

- [ ] **Step 7: Gate and commit**

```bash
cd ~/.cache/wt/setup-guard
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features --lib 2>&1 | grep -E "^test result"
git add src/qa/persist.rs docs/secrets-flow.md
git commit -m "fix(setup): refuse to overwrite a secret another bundle owns

gtc setup now stops instead of silently overwriting, and tells the operator to
re-run with a distinct --team. Writing the same value is still allowed, so
sharing one credential across bundles under one tenant keeps working.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Call the same guard from the onboarding wizard

**Files:**
- Create: worktree for `greentic-start` off `origin/main`
- Modify: `<start worktree>/src/qa_persist.rs` (`persist_qa_secrets`)

**Interfaces:**
- Consumes: `greentic_setup::secret_collision::{check, message}` — the SAME function Task 2 defined, imported through the existing dependency.
- Produces: `persist_qa_secrets` returns `Err` on collision.

**Background:** the wizard is the second door an operator can provision a provider secret through — `POST /api/onboard/qa/submit` → `onboard/wizard.rs:377` → `qa_persist::persist_qa_results` → `persist_qa_secrets`. Note that `admin_server.rs:1117` calls `greentic-setup`'s `persist_qa_results` from the published crate, so that third door inherits Task 3's guard automatically once setup publishes.

**Do not reimplement the rule here.** `Cargo.toml:265` already declares `greentic-setup = ">=1.1.0-0, <1.2.0-0"`. Import the function. A second copy is exactly how this pair of repos accumulated its bugs.

- [ ] **Step 1: Create the worktree**

```bash
cd /home/bima-pangestu/projects/Works/greentic/greentic-start
git fetch origin main
git worktree add ~/.cache/wt/start-guard -b fix/secret-collision-guard origin/main
```

- [ ] **Step 2: Confirm the guard is importable**

```bash
cd ~/.cache/wt/start-guard
grep -n -A2 "\[dependencies.greentic-setup\]" Cargo.toml
```

The published `greentic-setup` will not carry `secret_collision` until Task 2's work is released. Until then, point the dependency at the local worktree with a `path` override so you can build and test:

```toml
[dependencies.greentic-setup]
version = ">=1.1.0-0, <1.2.0-0"
path = "/home/bima-pangestu/.cache/wt/setup-guard"   # TEMPORARY — remove before pushing
```

**This override must not be committed.** Task 5 checks for it. Note it in your report so the reviewer looks for it too.

- [ ] **Step 3: Write the failing test**

```rust
    #[tokio::test]
    async fn the_wizard_refuses_to_overwrite_another_bundles_secret() {
        let (bundle_a, bundle_b, _env) = two_bundles_one_env();

        persist_qa_results(
            &bundle_a, &bundle_a.join("providers"), "demo", None, "messaging-telegram",
            &json!({ "bot_token": "token-a" }), &pack_path(), &form_spec(), false,
        ).await.expect("first bundle writes cleanly");

        let err = persist_qa_results(
            &bundle_b, &bundle_b.join("providers"), "demo", None, "messaging-telegram",
            &json!({ "bot_token": "token-b" }), &pack_path(), &form_spec(), false,
        ).await.expect_err("the wizard must refuse the second bundle");

        assert!(err.to_string().contains("--team"), "{err}");
    }
```

Build `two_bundles_one_env()`, `pack_path()` and `form_spec()` following the fixture idiom of the existing tests in `src/qa_persist.rs` — read them first.

- [ ] **Step 4: Run it to verify it fails**

```bash
cd ~/.cache/wt/start-guard
cargo test -p greentic-start --lib the_wizard_refuses
```

Use `--lib`; a bare name filter also runs empty integration binaries and buries the real result line.

Expected: FAIL — today the second write succeeds.

- [ ] **Step 5: Call the guard**

In `persist_qa_secrets`, before pushing each `SeedEntry`, call `greentic_setup::secret_collision::check` with the same arguments Task 3 passes, and `bail!` with `message(&collision)`. Thread the bundle's answers in the same way. Do not add a local copy of the rule.

- [ ] **Step 6: Run it to verify it passes, then prove it discriminates**

```bash
cd ~/.cache/wt/start-guard
cargo test -p greentic-start --lib 2>&1 | grep -E "^test result"
```

Then comment out the `bail!`, re-run the new test, capture the failure verbatim, restore.

- [ ] **Step 7: Gate and commit**

```bash
cd ~/.cache/wt/start-guard
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
git add src/qa_persist.rs
git commit -m "fix(start): refuse to overwrite a secret another bundle owns

The onboarding wizard is the second door a provider secret can be written
through. It now calls greentic-setup's collision guard rather than
reimplementing the rule - a guard that fires at one door and not the other is
worse than no guard."
```

**No `Co-Authored-By` trailer** — this repo forbids it.

---

## Task 5: Ship

- [ ] **Step 1: Remove the path override**

```bash
cd ~/.cache/wt/start-guard
grep -n "path = " Cargo.toml
```

Expected: no line pointing at `~/.cache/wt/setup-guard`. If one is there, remove it and re-run the gate — a committed path override breaks every build but yours.

- [ ] **Step 2: Full gate, both repos**

```bash
cd ~/.cache/wt/setup-guard && cargo fmt --all && bash ci/local_check.sh 2>&1 | tail -20
cd ~/.cache/wt/start-guard && cargo fmt --all && bash ci/local_check.sh 2>&1 | tail -20
```

Both must be green. A failure outside this plan's scope goes in the PR summary, not under the rug.

- [ ] **Step 3: Sequence the pushes**

`greentic-setup` first — `greentic-start` cannot compile against `secret_collision` until setup publishes. Push setup, wait for its release, bump start's floor if needed, then push start.

State this explicitly in both PR descriptions so a reviewer does not merge them in the wrong order.

- [ ] **Step 4: Write both PR descriptions**

Each must say: which bug it closes, that the addressing scheme is deliberately unchanged, and why — linking the spec's "Why the previous approach was abandoned" section. Reviewers who do not know that history will ask why this is not bundle-scoped.

- [ ] **Step 5: Manual verification**

Two bundles, one tenant, two different Telegram tokens, via `gtc setup`. The second must refuse with the `--team` message. Re-run the second with `--team bot-support` and confirm both bundles then resolve their own token at runtime. Ask Maarten to confirm on his environment — he offered to test.

---

## Task 6: Forward-port to develop (1.2)

- [ ] **Step 1: Branch and merge**

```bash
cd /home/bima-pangestu/projects/Works/greentic/greentic-setup && git fetch origin && \
  git worktree add ~/.cache/wt/setup-guard-fp -b forward-port/main-to-develop-20260806 origin/develop
cd ~/.cache/wt/setup-guard-fp && git merge origin/main
```

Repeat for `greentic-start`. Follow the `forward-port/main-to-develop-YYYYMMDD` convention both repos already use.

- [ ] **Step 2: Gate both**

```bash
cd ~/.cache/wt/setup-guard-fp && cargo fmt --all && bash ci/local_check.sh 2>&1 | tail -20
```

- [ ] **Step 3: Confirm the guard survived the port**

```bash
cd ~/.cache/wt/setup-guard-fp && cargo test --all-features --lib secret_collision
```

Expected: the same 5 tests pass on the 1.2 lane. If they do not, the lanes have diverged and the port is not done.

- [ ] **Step 4: Open both PRs against `develop` and merge once green.**

- [ ] **Step 5: Clean up worktrees**

```bash
cd /home/bima-pangestu/projects/Works/greentic/greentic-setup && git worktree remove ~/.cache/wt/setup-guard && git worktree remove ~/.cache/wt/setup-guard-fp
cd /home/bima-pangestu/projects/Works/greentic/greentic-start && git worktree remove ~/.cache/wt/start-guard && git worktree remove ~/.cache/wt/start-guard-fp
```

The abandoned worktrees (`setup-255`, `start-489`) and their branches can go too, once someone has decided what to do with `fix/tenant-scoped-answers` from Task 1.

---

## Out of scope

Recorded so they are visibly not covered rather than appearing handled:

1. **The DekCache bug** (`greentic-secrets-core/src/crypto/dek_cache.rs:19`) — `CacheKey` omits the secret name, so two names under one category fail AEAD through a shared store handle. Real, pre-existing, affects any provider with more than one secret key. This design neither worsens nor fixes it.
2. **Repairing an existing collision.** The guard prevents a new one. An operator already in the collided state must re-run with a distinct team.
3. **Any write path neither door covers.** If a value is overwritten by some path outside `gtc setup` and the onboarding wizard, the runtime still resolves the wrong token silently.
4. **Bug (A)**, the tunnel fix, is already shipped as `greentic-start` PR #489 and is untouched here.
