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
pub mod generated_secrets;
pub mod gtbundle;
pub mod oauth_callback;
pub mod plan;
pub mod platform_setup;
pub mod provider_state;
pub mod reload;
pub mod schema_validation;
pub mod secret_name;
pub mod secrets;
pub mod setup_actions;
pub mod setup_backend_contract;
pub mod setup_final_actions;
pub mod setup_input;
pub mod setup_machine;
pub mod setup_to_formspec;
pub mod setup_tunnel;
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

/// Resolve the effective environment string.
///
/// Priority: explicit override > `$GREENTIC_ENV` > `"dev"`.
pub fn resolve_env(override_env: Option<&str>) -> String {
    override_env
        .map(|v| v.to_string())
        .or_else(|| std::env::var("GREENTIC_ENV").ok())
        .unwrap_or_else(|| "dev".to_string())
}

/// Build a canonical secret URI: `secrets://{env}/{tenant}/{team}/{provider}/{key}`.
///
/// The team segment is normalized via `greentic-secrets`
/// ([`greentic_secrets_lib::normalize_team`]) — the single source of truth for
/// the "`_` everywhere" rule (empty / `"default"` / `None` → `_`) — and the key
/// via the shared [`secret_name::canonical_secret_name`]. The empty-provider →
/// `messaging` default and the infallible `String` shape are setup-local
/// conveniences kept on top of the shared primitives.
pub fn canonical_secret_uri(
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider: &str,
    key: &str,
) -> String {
    let team_segment = greentic_secrets_lib::normalize_team(team)
        .unwrap_or_else(|| greentic_secrets_lib::TEAM_PLACEHOLDER.to_string());
    // Normalize the provider segment the same way as the key (and as the cloud
    // secret name / env-bridge key already do), so a value written under a
    // provider id like `messaging-webchat-gui` resolves when a component fetches
    // it under `messaging.webchat-gui` — both collapse to `messaging_webchat_gui`.
    let provider_segment = if provider.is_empty() {
        "messaging".to_string()
    } else {
        secret_name::canonical_secret_name(provider)
    };
    let normalized_key = secret_name::canonical_secret_name(key);
    format!("secrets://{env}/{tenant}/{team_segment}/{provider_segment}/{normalized_key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_correct() {
        assert!(version().starts_with("0.5"));
    }

    #[test]
    fn secret_uri_basic() {
        let uri = canonical_secret_uri("dev", "demo", None, "messaging-telegram", "bot_token");
        assert_eq!(uri, "secrets://dev/demo/_/messaging_telegram/bot_token");
    }

    #[test]
    fn secret_uri_with_team() {
        let uri = canonical_secret_uri("dev", "acme", Some("ops"), "state-redis", "redis_url");
        assert_eq!(uri, "secrets://dev/acme/ops/state_redis/redis_url");
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
        assert_eq!(uri, "secrets://dev/demo/_/messaging_slack/bot_token");
    }

    #[test]
    fn secret_uri_normalizes_provider_segment() {
        // The provider segment is normalized like the key, so a secret written
        // under the pack id `messaging-webchat-gui` resolves when fetched under
        // the component's dotted id `messaging.webchat-gui`.
        let stored = canonical_secret_uri(
            "dev",
            "demo",
            None,
            "messaging-webchat-gui",
            "jwt_signing_key",
        );
        let fetched = canonical_secret_uri(
            "dev",
            "demo",
            None,
            "messaging.webchat-gui",
            "jwt_signing_key",
        );
        assert_eq!(stored, fetched);
        assert_eq!(
            stored,
            "secrets://dev/demo/_/messaging_webchat_gui/jwt_signing_key"
        );
    }
}
