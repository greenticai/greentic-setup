//! End-to-end bundle setup engine for the Greentic platform.
//!
//! Provides pack discovery, QA-driven configuration, secrets persistence,
//! and bundle lifecycle management as a library crate.

pub mod admin;
pub mod bundle;
pub mod card_setup;
pub mod discovery;
pub mod engine;
pub mod plan;
pub mod reload;
pub mod secret_name;
pub mod secrets;
pub mod setup_input;
pub mod setup_to_formspec;
pub mod webhook;

pub mod qa {
    //! QA-driven configuration: FormSpec bridge, wizard prompts, answers
    //! persistence, and setup input loading.
    pub mod bridge;
    pub mod persist;
    pub mod wizard;
}

pub use engine::SetupEngine;
pub use plan::{SetupMode, SetupPlan, SetupStep, SetupStepKind};

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

    #[test]
    fn version_is_correct() {
        assert!(version().starts_with("0.4"));
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
}
