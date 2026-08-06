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

use greentic_secrets_lib::core::Error as SecretError;
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
        Some(team) if !team.is_empty() => {
            format!("tenant '{}' / team '{}'", collision.tenant, team)
        }
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

/// Distinguishes "nothing is stored at this address" from "the store could
/// not be read".
///
/// `greentic-start/src/runner_host/mod.rs` has an `is_secret_not_found` that
/// matches on the rendered error text (`"not found"`, `"NotFound"`, ...). That
/// function takes `impl std::fmt::Display`, because it has to work across
/// whatever error types different secret backends surface to it, so text
/// matching is the only seam it has. `check` below is typed concretely against
/// `DevStore`, whose `SecretsStore::get` resolves to
/// `greentic_secrets_lib::core::Error` — a `thiserror` enum with a structured
/// `NotFound { entity }` variant. That variant is already the discrimination
/// this same crate uses elsewhere (`src/secrets.rs`, `ensure_pack_secrets`), so
/// matching on it structurally is both more precise and consistent with
/// existing code, rather than inventing a second, string-based notion of
/// "absent" alongside it.
fn is_not_found(err: &SecretError) -> bool {
    matches!(err, SecretError::NotFound { .. })
}

/// `Some(Collision)` when writing `new_value` to `uri` would overwrite a value
/// this bundle did not write.
///
/// `existing_answers` is the bundle's own `setup-answers.json`, used ONLY as a
/// did-I-write-this marker — not as an address store. Nothing is unrecoverable
/// if it is missing: the guard then errs toward refusing, which is the safe
/// direction.
#[allow(clippy::too_many_arguments)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_secrets_lib::SecretFormat;
    use serde_json::json;

    fn answers_with(key: &str) -> Value {
        json!({ key: "whatever this bundle wrote" })
    }

    /// Points the dev store at a fresh temp dir and opens it, following the
    /// `src/qa/persist.rs` fixture idiom: a `StoreOverride` guard installs
    /// `GREENTIC_DEV_SECRETS_PATH` for the duration of `open_dev_store_for_env`
    /// (satisfying `assert_store_access_is_guarded` and keeping the write off
    /// the developer's real `~/.greentic`), then is dropped.
    ///
    /// The guard only needs to be held across that one call: it gates
    /// `crate::secrets::override_path()`, which `DevStore::with_path` (invoked
    /// once, inside `open_dev_store_for_env`) consults to resolve its path, but
    /// the returned `DevStore` never re-consults it — `SecretsStore::get`/`put`
    /// operate on the path already baked into the store at construction. So,
    /// unlike the persist.rs tests (which keep their guard bound for the whole
    /// test body because they also call the higher-level persist helpers that
    /// resolve paths themselves), this helper's guard can be scoped tightly to
    /// construction and dropped before returning.
    async fn empty_test_store() -> (tempfile::TempDir, DevStore) {
        let dir = tempfile::tempdir().expect("store isolation dir");
        let store = {
            let _guard = crate::secrets::test_support::StoreOverride::in_dir(dir.path());
            crate::secrets::open_dev_store_for_env(dir.path(), "dev").expect("open dev store")
        };
        (dir, store)
    }

    /// Same as [`empty_test_store`], seeded with one value at `uri`.
    async fn test_store_holding(uri: &str, value: &str) -> (tempfile::TempDir, DevStore) {
        let (dir, store) = empty_test_store().await;
        store
            .put(uri, SecretFormat::Text, value.as_bytes())
            .await
            .expect("seed store value");
        (dir, store)
    }

    #[tokio::test]
    async fn a_different_value_from_another_bundle_is_a_collision() {
        let (_dir, store) = test_store_holding(
            "secrets://dev/demo/_/messaging_telegram/bot_token",
            "bundle-a-token",
        )
        .await;
        let found = check(
            &store,
            None,
            "demo",
            None,
            "messaging-telegram",
            "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token",
            "bundle-b-token",
        )
        .await;
        assert!(
            found.is_some(),
            "a second bundle overwriting a different value must be refused"
        );
    }

    #[tokio::test]
    async fn the_same_value_is_not_a_collision() {
        let (_dir, store) = test_store_holding(
            "secrets://dev/demo/_/messaging_telegram/bot_token",
            "shared-token",
        )
        .await;
        let found = check(
            &store,
            None,
            "demo",
            None,
            "messaging-telegram",
            "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token",
            "shared-token",
        )
        .await;
        assert!(
            found.is_none(),
            "deliberate sharing of one value must keep working"
        );
    }

    #[tokio::test]
    async fn this_bundles_own_re_run_is_not_a_collision() {
        let (_dir, store) = test_store_holding(
            "secrets://dev/demo/_/messaging_telegram/bot_token",
            "old-token",
        )
        .await;
        let answers = answers_with("bot_token");
        let found = check(
            &store,
            Some(&answers),
            "demo",
            None,
            "messaging-telegram",
            "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token",
            "new-token",
        )
        .await;
        assert!(
            found.is_none(),
            "a bundle updating its own secret must not be refused"
        );
    }

    #[tokio::test]
    async fn an_empty_address_is_not_a_collision() {
        let (_dir, store) = empty_test_store().await;
        let found = check(
            &store,
            None,
            "demo",
            None,
            "messaging-telegram",
            "bot_token",
            "secrets://dev/demo/_/messaging_telegram/bot_token",
            "first-token",
        )
        .await;
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
        assert!(
            text.contains("--team"),
            "the operator must be told how to proceed: {text}"
        );
    }

    /// `DevStore::get` (via `BrokerStore`, `greentic-secrets-core` 1.1.6) reads
    /// purely from in-memory state loaded once when the store is opened —
    /// `DevBackend::get` never touches the filesystem again after
    /// construction. So a real `DevStore` whose `.get()` fails for a reason
    /// other than "not found" cannot be built at this seam without either:
    ///
    /// - passing a malformed uri (a caller bug, not an unreadable store), or
    /// - forcing a decrypt/MAC-mismatch by racing `GREENTIC_DEV_MASTER_KEY`
    ///   between the writer and reader — an *unexported* `const` inside
    ///   `greentic-secrets-provider-dev`, several dependency hops from
    ///   `greentic-secrets-lib`'s public surface and not covered by its semver
    ///   contract.
    ///
    /// Neither is a clean fixture, so per the brief's fallback this pins
    /// `is_not_found` directly instead of faking an end-to-end unreadable
    /// store.
    #[test]
    fn is_not_found_distinguishes_absence_from_read_failure() {
        assert!(is_not_found(&SecretError::NotFound {
            entity: "secrets://dev/demo/_/messaging_telegram/bot_token".into()
        }));
        assert!(!is_not_found(&SecretError::Storage("disk full".into())));
        assert!(!is_not_found(&SecretError::Backend(
            "broker unreachable".into()
        )));
    }
}
