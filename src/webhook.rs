//! Webhook auto-setup placeholder.
//!
//! This module will handle automatic webhook registration for providers
//! that support it (Telegram, Slack, Webex) during bundle setup.
//! Currently a stub — the actual implementation depends on provider-specific
//! WASM component invocation which lives in the operator.

/// Check whether a provider's answers contain a valid `public_base_url`
/// suitable for webhook registration.
pub fn has_webhook_url(answers: &serde_json::Value) -> Option<&str> {
    answers
        .as_object()?
        .get("public_base_url")?
        .as_str()
        .filter(|url| !url.is_empty() && url.starts_with("https://"))
}
