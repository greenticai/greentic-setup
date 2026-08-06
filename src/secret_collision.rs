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
//! Two message functions exist ([`message`] and [`message_unattributed`])
//! because not every caller can actually back up "a different bundle wrote
//! this" — see [`message_unattributed`]'s doc comment.
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

/// The `tenant '...'` / `tenant '...' / team '...'` prefix shared by both
/// operator-facing messages below.
fn scope_description(collision: &Collision) -> String {
    match collision.team.as_deref() {
        Some(team) if !team.is_empty() => {
            format!("tenant '{}' / team '{}'", collision.tenant, team)
        }
        _ => format!("tenant '{}'", collision.tenant),
    }
}

/// The operator-facing explanation for a collision guarded by a reliable
/// did-I-write-this marker (the operator-answer loop, and the
/// requirement-key aliases derived from it — see
/// `qa::persist::seed_secret_requirement_aliases`). In both cases
/// `existing_answers` is this bundle's own recorded answers file, so
/// "the marker doesn't have this key" really does mean a different bundle
/// wrote the value: this is the last point at which the operator can still
/// fix the situation, so it names both the fact and the remedy.
pub fn message(collision: &Collision) -> String {
    let scope = scope_description(collision);
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

/// The operator-facing explanation for a collision guarded with no usable
/// did-I-write-this marker at all — currently only
/// `generated_secrets::introduce_into_store`, whose generated values are
/// never recorded as operator answers and can't be compared by value either
/// (a fresh random value is generated on every call). `check()` there runs
/// fail-closed: any pre-existing, differing value is refused, whether it was
/// written by another bundle or by this same bundle on a prior run.
///
/// [`message`] would be misleading here: it asserts "written by a different
/// bundle" as fact, but that may be false — the guard genuinely cannot tell.
/// An operator sent looking for a second bundle that does not exist would be
/// chasing a phantom. This message instead states only what the guard
/// verified (this run did not write the current value) and offers the same
/// `--team` remedy as a possibility, not a diagnosis.
pub fn message_unattributed(collision: &Collision) -> String {
    let scope = scope_description(collision);
    format!(
        "{scope} already holds {key} for {provider}, and this run did not write it.\n\
         \n\
         This may be a different bundle's value, or a prior run of this same bundle whose\n\
         value cannot be re-derived. Two bundles cannot share one (tenant, team) for the\n\
         same secret — if this is a collision, re-run with a distinct team to separate\n\
         them, for example:\n\
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

/// What reading `uri` from the store told us.
///
/// Kept as three explicit outcomes rather than folding "cannot tell" into
/// `Value(Vec::new())`: an empty `Vec<u8>` is a legitimate stored value (an
/// empty-string secret), and comparing an unreadable read against
/// `new_value.as_bytes()` would make an unreadable store compare equal to an
/// empty write, taking the same-value path and silently returning `None` —
/// exactly the bypass the "cannot tell" fallback exists to prevent.
enum StoreRead {
    /// Nothing is stored at this address.
    Absent,
    /// The stored bytes, verbatim.
    Value(Vec<u8>),
    /// The store returned an error other than "not found" — the address
    /// could not be resolved or the read otherwise failed. We cannot tell
    /// what, if anything, is stored here.
    Unreadable,
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
    let read = match store.get(uri).await {
        Ok(bytes) => StoreRead::Value(bytes),
        Err(err) if is_not_found(&err) => StoreRead::Absent,
        Err(_) => StoreRead::Unreadable,
    };
    let value_differs = match read {
        StoreRead::Absent => return None,
        StoreRead::Value(bytes) => bytes != new_value.as_bytes(),
        // We cannot tell what is stored, so we cannot tell it is the same
        // value either — assume it differs and let the answers check decide.
        StoreRead::Unreadable => true,
    };
    if !value_differs {
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

    /// `message_unattributed` must not repeat `message`'s "written by a
    /// different bundle" claim — that's true for the reliable-marker paths
    /// but not for generated secrets, where the guard cannot tell who wrote
    /// the existing value. It must still name the address and the `--team`
    /// remedy.
    #[test]
    fn the_unattributed_message_does_not_claim_a_different_bundle() {
        let collision = Collision {
            tenant: "demo".into(),
            team: None,
            provider_id: "messaging-webchat-gui".into(),
            key: "jwt_signing_key".into(),
        };
        let text = message_unattributed(&collision);
        assert!(text.contains("demo"), "{text}");
        assert!(text.contains("messaging-webchat-gui"), "{text}");
        assert!(text.contains("jwt_signing_key"), "{text}");
        assert!(
            text.contains("--team"),
            "the operator must still be told how to proceed: {text}"
        );
        assert!(
            !text.contains("written by a different bundle"),
            "the guard cannot actually verify this, so it must not assert it: {text}"
        );
    }

    /// `DevStore::get` (via `BrokerStore`, `greentic-secrets-core` 1.1.6) reads
    /// purely from in-memory state loaded once when the store is opened —
    /// `DevBackend::get` never touches the filesystem again after
    /// construction, so an I/O-style failure cannot be induced post-open.
    /// This unit-pins the classifier itself, in isolation from any store, as
    /// a belt-and-braces check on the two `SecretError` shapes it must tell
    /// apart. `an_unreadable_address_is_treated_as_occupied_not_as_free`
    /// below exercises the same discrimination through `check()` end-to-end
    /// with a real `DevStore` and a malformed uri.
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

    /// `BrokerStore::get` (`greentic-secrets-core/src/seed.rs`) calls
    /// `SecretUri::parse(uri)?` before it ever touches the backend, and
    /// `SecretUri::parse` on a string that doesn't start with `secrets://`
    /// returns `Error::InvalidScheme` — not `NotFound`. That is a real,
    /// ordinary way for `store.get` to fail for a reason other than
    /// "absent", with a plain `DevStore` and no fixture trickery at all: a
    /// malformed `uri` argument is enough. `check` must not treat that as
    /// "free to write".
    #[tokio::test]
    async fn an_unreadable_address_is_treated_as_occupied_not_as_free() {
        let (_dir, store) = empty_test_store().await;
        let found = check(
            &store,
            None,
            "demo",
            None,
            "messaging-telegram",
            "bot_token",
            "not-a-secrets-uri",
            "token-b",
        )
        .await;
        assert!(
            found.is_some(),
            "when the store cannot be read we must refuse, not assume the address is free",
        );
    }
}
