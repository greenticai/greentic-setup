//! Webhook registration for messaging providers during setup.
//!
//! Provider packs implement the `setup_webhook` WASM operation to register
//! webhooks with external APIs (e.g. Telegram `setWebhook`, Slack manifest
//! update, Webex webhook management). The setup engine invokes this operation
//! generically via `invoke_provider_op("setup_webhook", ...)`.
//!
//! This module provides the `register_webhook` entry point which checks for
//! declared ops in config before falling back to the provider WASM operation.

mod instructions;

use serde_json::{Value, json};

// Re-export public functions from submodules
pub use instructions::{
    ProviderInstruction, collect_post_setup_instructions, print_post_setup_instructions,
};

/// Extract registration result from declared ops in config.
///
/// If the config contains `webhook_ops`, return a result indicating
/// declared ops mode instead of performing live registration.
pub fn registration_result_from_declared_ops(config: &Value) -> Option<Value> {
    let webhook_ops = config.get("webhook_ops")?.as_array()?;
    if webhook_ops.is_empty() {
        return None;
    }
    let subscription_ops = config
        .get("subscription_ops")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let oauth_ops = config
        .get("oauth_ops")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Some(json!({
        "ok": true,
        "mode": "declared_ops",
        "webhook_ops": webhook_ops,
        "subscription_ops": subscription_ops,
        "oauth_ops": oauth_ops,
    }))
}

/// Check whether a provider's answers contain a valid `public_base_url`
/// suitable for webhook registration.
pub fn has_webhook_url(answers: &Value) -> Option<&str> {
    answers
        .as_object()?
        .get("public_base_url")?
        .as_str()
        .filter(|url| !url.is_empty() && url.starts_with("https://"))
}

/// Register a webhook for a provider based on its setup answers.
///
/// Returns `Some(result)` with status JSON from declared ops, or `None` if
/// the provider doesn't declare webhook ops in its config. The actual webhook
/// registration is handled by the provider's `setup_webhook` WASM operation,
/// invoked by the setup engine after this function returns `None`.
pub fn register_webhook(
    _provider_id: &str,
    config: &Value,
    _tenant: &str,
    _team: Option<&str>,
) -> Option<Value> {
    // Use declared ops from provider setup flow output if available
    registration_result_from_declared_ops(config)
}

/// Build the webhook URL for a provider.
pub fn build_webhook_url(
    public_base_url: &str,
    provider_id: &str,
    tenant: &str,
    team: &str,
) -> String {
    format!(
        "{}/v1/messaging/ingress/{}/{}/{}",
        public_base_url.trim_end_matches('/'),
        provider_id,
        tenant,
        team,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_webhook_url_valid() {
        let config = json!({"public_base_url": "https://example.com"});
        assert_eq!(has_webhook_url(&config), Some("https://example.com"));
    }

    #[test]
    fn registration_result_from_declared_ops_uses_declared_ops() {
        let config = json!({
            "webhook_ops": [{"op": "register", "url": "https://example.com/webhook"}],
            "subscription_ops": [{"op": "sync"}],
            "oauth_ops": []
        });
        let result =
            registration_result_from_declared_ops(&config).expect("declared ops registration");
        assert_eq!(result["ok"], Value::Bool(true));
        assert_eq!(result["mode"], Value::String("declared_ops".to_string()));
        assert_eq!(
            result["webhook_ops"][0]["op"],
            Value::String("register".to_string())
        );
    }

    #[test]
    fn register_webhook_prefers_declared_ops() {
        let config = json!({
            "public_base_url": "http://example.com",
            "webhook_ops": [{"op": "register", "url": "https://example.com/webhook"}]
        });
        let result = register_webhook("messaging-unknown", &config, "demo", None)
            .expect("declared ops fallback");
        assert_eq!(result["mode"], Value::String("declared_ops".to_string()));
    }

    #[test]
    fn has_webhook_url_http_rejected() {
        let config = json!({"public_base_url": "http://example.com"});
        assert_eq!(has_webhook_url(&config), None);
    }

    #[test]
    fn has_webhook_url_empty_rejected() {
        let config = json!({"public_base_url": ""});
        assert_eq!(has_webhook_url(&config), None);
    }

    #[test]
    fn register_webhook_returns_none_without_declared_ops() {
        let config = json!({"public_base_url": "https://example.com", "bot_token": "x"});
        assert!(register_webhook("messaging-telegram", &config, "demo", None).is_none());
    }

    #[test]
    fn register_webhook_returns_none_without_public_url() {
        let config = json!({"bot_token": "x"});
        assert!(register_webhook("messaging-telegram", &config, "demo", None).is_none());
    }

    #[test]
    fn build_webhook_url_format() {
        let url = build_webhook_url(
            "https://example.com",
            "messaging-telegram",
            "demo",
            "default",
        );
        assert_eq!(
            url,
            "https://example.com/v1/messaging/ingress/messaging-telegram/demo/default"
        );
    }

    #[test]
    fn build_webhook_url_trims_trailing_slash() {
        let url = build_webhook_url(
            "https://example.com/",
            "messaging-telegram",
            "demo",
            "default",
        );
        assert_eq!(
            url,
            "https://example.com/v1/messaging/ingress/messaging-telegram/demo/default"
        );
    }
}
