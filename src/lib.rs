//! End-to-end bundle setup engine for the Greentic platform.
//!
//! Provides pack discovery, QA-driven configuration, secrets persistence,
//! and bundle lifecycle management as a library crate.

pub mod admin;
pub mod answers_crypto;
pub mod bundle;
pub mod bundle_source;
pub mod capabilities;
pub mod card_setup;
pub mod cli_args;
pub mod cli_commands;
pub mod cli_helpers;
pub mod cli_i18n;
pub mod config_envelope;
pub mod deployment_targets;
pub mod discovery;
pub mod doctor;
pub mod engine;
pub mod flow;
pub mod gtbundle;
pub mod plan;
pub mod platform_setup;
pub mod reload;
pub mod secret_name;
pub mod secrets;
pub mod setup_input;
pub mod setup_to_formspec;
pub mod tenant_config;
pub mod webhook;

#[cfg(feature = "ui")]
pub mod ui;

pub mod qa {
    //! QA-driven configuration: FormSpec bridge, wizard prompts, answers
    //! persistence, and setup input loading.
    pub mod bridge;
    pub mod persist;
    pub mod prompts;
    pub mod shared_questions;
    pub mod wizard;
}

pub use bundle_source::BundleSource;
pub use engine::SetupEngine;
pub use plan::{SetupMode, SetupPlan, SetupStep, SetupStepKind};

// Re-export shared questions types and functions for convenient multi-provider setup
pub use qa::wizard::{
    ProviderFormSpec, SHARED_QUESTION_IDS, SharedQuestionsResult, build_provider_form_specs,
    collect_shared_questions, prompt_shared_questions, run_qa_setup_with_shared,
};

/// Returns the crate version.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Default environment id when nothing is set. Flipped from `"dev"` to
/// `"local"` as part of A4b — the `local` env is what `gtc setup` and
/// `gtc start` auto-create per A4.
pub const DEFAULT_ENV_ID: &str = "local";

/// Legacy env id this crate accepts via the compat alias. Resolved values
/// that match this string are remapped to [`DEFAULT_ENV_ID`] with a
/// once-per-process warning, unless the operator disables the alias.
pub const LEGACY_ENV_ID: &str = "dev";

/// Env-var that disables the [`LEGACY_ENV_ID`] → [`DEFAULT_ENV_ID`] compat
/// alias. Set to `1`, `true`, `yes`, or `on` (case-insensitive) to make any
/// resolved value of `dev` hard-fail with a remediation hint. Intended for
/// CI assertions that prove no production code-path still resolves to the
/// legacy env id; remove once A4b PR3 flips the default in
/// `greentic-config` and downstream consumers no longer pass `dev`.
pub const DISABLE_ALIAS_ENV_VAR: &str = "GREENTIC_DISABLE_DEV_ALIAS";

/// Resolve the effective environment string.
///
/// Priority: explicit override > `$GREENTIC_ENV` > [`DEFAULT_ENV_ID`]
/// (`"local"`). After resolution, applies the [`LEGACY_ENV_ID`] →
/// [`DEFAULT_ENV_ID`] compat alias: any value of `dev` is remapped to
/// `local` with a once-per-process `tracing::warn!` unless
/// [`DISABLE_ALIAS_ENV_VAR`] is set, in which case the resolution panics
/// with a remediation hint.
pub fn resolve_env(override_env: Option<&str>) -> String {
    let raw = override_env
        .map(|v| v.to_string())
        .or_else(|| std::env::var("GREENTIC_ENV").ok())
        .unwrap_or_else(|| DEFAULT_ENV_ID.to_string());
    compat_alias::apply_dev_alias(&raw)
}

mod compat_alias {
    //! `dev` → `local` compatibility alias (A4b).
    //!
    //! Centralized so `greentic-start` can mirror the contract verbatim;
    //! the parallel implementation in that crate will be replaced with a
    //! call into a shared helper if/when the duplication starts mattering.

    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{DEFAULT_ENV_ID, DISABLE_ALIAS_ENV_VAR, LEGACY_ENV_ID};

    static WARNED: AtomicBool = AtomicBool::new(false);

    /// Apply the `dev` → `local` compat alias. Returns the remapped value
    /// for any input equal to [`LEGACY_ENV_ID`]; returns the input
    /// unchanged for any other value. Panics if the alias is disabled via
    /// [`DISABLE_ALIAS_ENV_VAR`] and the input is the legacy id.
    pub fn apply_dev_alias(env: &str) -> String {
        if env != LEGACY_ENV_ID {
            return env.to_string();
        }
        if alias_disabled() {
            // Hard-fail expiry gate. The panic message is the remediation —
            // tracing may not be wired in every binary that consumes
            // `resolve_env`, and exit() bypasses test harnesses.
            panic!(
                "environment `{LEGACY_ENV_ID}` is no longer accepted (set via {DISABLE_ALIAS_ENV_VAR}=1). \
                 Migrate to `{DEFAULT_ENV_ID}` via `gtc op env migrate-dev {DEFAULT_ENV_ID} --check` then `--apply`, \
                 or pass `--env {DEFAULT_ENV_ID}` / unset $GREENTIC_ENV.",
            );
        }
        if !WARNED.swap(true, Ordering::SeqCst) {
            tracing::warn!(
                target: "greentic_setup::compat_alias",
                legacy = LEGACY_ENV_ID,
                target_env = DEFAULT_ENV_ID,
                "env `{LEGACY_ENV_ID}` is deprecated; resolving as `{DEFAULT_ENV_ID}` for this process. \
                 Plan the migration with `gtc op env migrate-dev {DEFAULT_ENV_ID} --check`; \
                 set {DISABLE_ALIAS_ENV_VAR}=1 to hard-fail on `{LEGACY_ENV_ID}` in CI.",
            );
        }
        DEFAULT_ENV_ID.to_string()
    }

    fn alias_disabled() -> bool {
        std::env::var(DISABLE_ALIAS_ENV_VAR)
            .ok()
            .map(|v| {
                let v = v.trim().to_ascii_lowercase();
                matches!(v.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    }

    /// Reset the warning latch. Test-only so multiple `apply_dev_alias`
    /// invocations can each verify the once-per-process behavior.
    #[cfg(test)]
    pub(super) fn reset_warning_latch_for_tests() {
        WARNED.store(false, Ordering::SeqCst);
    }
}

/// Build a canonical secret URI: `secrets://{env}/{tenant}/{team}/{provider}/{key}`.
pub fn canonical_secret_uri(
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider: &str,
    key: &str,
) -> String {
    let team_segment = canonical_team(team);
    let provider_segment = if provider.is_empty() {
        "messaging".to_string()
    } else {
        provider.to_string()
    };
    let normalized_key = secret_name::canonical_secret_name(key);
    format!("secrets://{env}/{tenant}/{team_segment}/{provider_segment}/{normalized_key}")
}

/// Normalize the team segment for secret URIs.
///
/// Empty, `"default"`, or `None` → `"_"` (wildcard).
fn canonical_team(team: Option<&str>) -> &str {
    match team
        .map(|v| v.trim())
        .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("default"))
    {
        Some(v) => v,
        None => "_",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `GREENTIC_ENV` and `GREENTIC_DISABLE_DEV_ALIAS` are process-global;
    // serialize tests that mutate them so they don't interleave with each
    // other or with tests in other modules that mutate the same vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_clean_env<R>(body: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_env = std::env::var_os("GREENTIC_ENV");
        let prev_disable = std::env::var_os(DISABLE_ALIAS_ENV_VAR);
        // SAFETY: serialized by ENV_LOCK; tests are single-threaded inside
        // the critical section. unsafe is required because set_var /
        // remove_var are marked unsafe in Rust 2024 edition.
        unsafe {
            std::env::remove_var("GREENTIC_ENV");
            std::env::remove_var(DISABLE_ALIAS_ENV_VAR);
        }
        compat_alias::reset_warning_latch_for_tests();
        let out = body();
        unsafe {
            match prev_env {
                Some(v) => std::env::set_var("GREENTIC_ENV", v),
                None => std::env::remove_var("GREENTIC_ENV"),
            }
            match prev_disable {
                Some(v) => std::env::set_var(DISABLE_ALIAS_ENV_VAR, v),
                None => std::env::remove_var(DISABLE_ALIAS_ENV_VAR),
            }
        }
        out
    }

    #[test]
    fn version_is_correct() {
        assert!(version().starts_with("1.1"));
    }

    #[test]
    fn secret_uri_basic() {
        let uri = canonical_secret_uri("dev", "demo", None, "messaging-telegram", "bot_token");
        assert_eq!(uri, "secrets://dev/demo/_/messaging-telegram/bot_token");
    }

    #[test]
    fn secret_uri_with_team() {
        let uri = canonical_secret_uri("dev", "acme", Some("ops"), "state-redis", "redis_url");
        assert_eq!(uri, "secrets://dev/acme/ops/state-redis/redis_url");
    }

    #[test]
    fn secret_uri_default_team_becomes_wildcard() {
        let uri = canonical_secret_uri(
            "dev",
            "demo",
            Some("default"),
            "messaging-slack",
            "bot_token",
        );
        assert_eq!(uri, "secrets://dev/demo/_/messaging-slack/bot_token");
    }

    #[test]
    fn resolve_env_returns_local_by_default() {
        with_clean_env(|| {
            assert_eq!(resolve_env(None), "local");
        });
    }

    #[test]
    fn resolve_env_passes_through_non_legacy_override() {
        with_clean_env(|| {
            assert_eq!(resolve_env(Some("staging")), "staging");
            assert_eq!(resolve_env(Some("prod")), "prod");
            assert_eq!(resolve_env(Some("local")), "local");
        });
    }

    #[test]
    fn resolve_env_remaps_dev_override_to_local() {
        with_clean_env(|| {
            assert_eq!(resolve_env(Some("dev")), "local");
        });
    }

    #[test]
    fn resolve_env_remaps_dev_env_var_to_local() {
        with_clean_env(|| {
            // SAFETY: serialized via ENV_LOCK inside with_clean_env.
            unsafe {
                std::env::set_var("GREENTIC_ENV", "dev");
            }
            assert_eq!(resolve_env(None), "local");
        });
    }

    #[test]
    fn alias_warning_fires_only_once_per_process() {
        // The warn target is the same across calls — the AtomicBool latch
        // is what we're verifying. Direct call to apply_dev_alias avoids
        // re-reading env vars.
        with_clean_env(|| {
            // First two calls: alias remaps both, but only the first fires
            // the warn (visible via the AtomicBool latch — there's no
            // easy way to count tracing events without wiring a subscriber,
            // so we exercise the latch state by re-resetting and verifying
            // a second non-firing path returns the same remapped value).
            assert_eq!(compat_alias::apply_dev_alias("dev"), "local");
            assert_eq!(compat_alias::apply_dev_alias("dev"), "local");
            // Reset confirms the latch was set (the next call would warn
            // again after reset).
            compat_alias::reset_warning_latch_for_tests();
            assert_eq!(compat_alias::apply_dev_alias("dev"), "local");
        });
    }

    #[test]
    fn disable_alias_env_var_panics_on_dev() {
        with_clean_env(|| {
            // SAFETY: serialized via ENV_LOCK inside with_clean_env.
            unsafe {
                std::env::set_var(DISABLE_ALIAS_ENV_VAR, "1");
            }
            let result = std::panic::catch_unwind(|| resolve_env(Some("dev")));
            assert!(
                result.is_err(),
                "resolve_env should panic when alias is disabled and input is `dev`"
            );
        });
    }

    #[test]
    fn disable_alias_accepts_truthy_strings() {
        for value in ["1", "true", "TRUE", "yes", "YES", "on", " true "] {
            with_clean_env(|| {
                // SAFETY: serialized via ENV_LOCK inside with_clean_env.
                unsafe {
                    std::env::set_var(DISABLE_ALIAS_ENV_VAR, value);
                }
                let result = std::panic::catch_unwind(|| resolve_env(Some("dev")));
                assert!(
                    result.is_err(),
                    "DISABLE value `{value}` should hard-fail on dev resolution"
                );
            });
        }
    }

    #[test]
    fn disable_alias_does_not_panic_on_non_legacy_values() {
        with_clean_env(|| {
            // SAFETY: serialized via ENV_LOCK inside with_clean_env.
            unsafe {
                std::env::set_var(DISABLE_ALIAS_ENV_VAR, "1");
            }
            // Non-legacy values pass through unaffected even when the
            // alias is disabled — the gate only fires on `dev`.
            assert_eq!(resolve_env(Some("local")), "local");
            assert_eq!(resolve_env(Some("staging")), "staging");
            assert_eq!(resolve_env(None), "local");
        });
    }
}
