//! Webhook registration for messaging providers during setup.
//!
//! Calls provider-specific APIs (e.g. Telegram `setWebhook`, Slack manifest
//! update, Webex webhook management) to register the operator's ingress
//! endpoint so that external services can deliver messages to the running
//! instance.
//!
//! Ported from `greentic-operator/src/onboard/webhook_setup.rs` so that
//! `gtc setup` can handle webhook registration without the operator.

mod instructions;
mod slack;
mod telegram;
mod webex;

use serde_json::{Value, json};

// Re-export public functions from submodules
pub use instructions::{
    ProviderInstruction, collect_post_setup_instructions, print_post_setup_instructions,
};
pub use slack::update_manifest_urls as slack_update_manifest_urls;

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
/// Supports: Telegram, Slack, Webex.
/// Returns `Some(result)` with status JSON, or `None` if the provider
/// doesn't need webhook registration.
pub fn register_webhook(
    provider_id: &str,
    config: &Value,
    tenant: &str,
    team: Option<&str>,
) -> Option<Value> {
    if let Some(result) = registration_result_from_declared_ops(config) {
        return Some(result);
    }

    let public_base_url = config.get("public_base_url").and_then(Value::as_str)?;
    if public_base_url.is_empty() || !public_base_url.starts_with("https://") {
        return None;
    }

    let team = team.unwrap_or("default");

    let provider_short = provider_id
        .strip_prefix("messaging-")
        .unwrap_or(provider_id);

    match provider_short {
        "telegram" => {
            telegram::setup_telegram_webhook(config, public_base_url, provider_id, tenant, team)
        }
        "slack" => slack::setup_slack_manifest(config, public_base_url, provider_id, tenant, team),
        "webex" => webex::setup_webex_webhook(config, public_base_url, provider_id, tenant, team),
        _ => None,
    }
}

/// Build the webhook URL for a provider.
pub(crate) fn build_webhook_url(
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
        assert_eq!(
            result["webhook_ops"][0]["url"],
            "https://example.com/webhook"
        );
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
    fn register_webhook_skips_unknown_provider() {
        let config = json!({"public_base_url": "https://example.com", "bot_token": "x"});
        assert!(register_webhook("messaging-unknown", &config, "demo", None).is_none());
    }

    #[test]
    fn register_webhook_skips_without_public_url() {
        let config = json!({"bot_token": "x"});
        assert!(register_webhook("messaging-telegram", &config, "demo", None).is_none());
    }

    #[test]
    fn register_webhook_skips_http_url() {
        let config = json!({"public_base_url": "http://example.com", "bot_token": "x"});
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

    #[test]
    fn slack_skips_without_credentials() {
        let config = json!({"public_base_url": "https://example.com"});
        assert!(register_webhook("messaging-slack", &config, "demo", None).is_none());
    }

    #[test]
    fn webex_skips_without_token() {
        let config = json!({"public_base_url": "https://example.com"});
        assert!(register_webhook("messaging-webex", &config, "demo", None).is_none());
    }

    #[test]
    fn slack_update_manifest_urls_creates_settings() {
        let mut manifest = json!({});
        slack_update_manifest_urls(&mut manifest, "https://example.com/webhook");
        let settings = manifest.get("settings").unwrap();
        assert_eq!(
            settings["event_subscriptions"]["request_url"],
            "https://example.com/webhook"
        );
        assert_eq!(
            settings["interactivity"]["request_url"],
            "https://example.com/webhook"
        );
        assert_eq!(settings["interactivity"]["is_enabled"], true);
    }

    #[test]
    fn slack_update_manifest_urls_updates_existing() {
        let mut manifest = json!({
            "settings": {
                "event_subscriptions": { "request_url": "https://old.com" },
                "interactivity": { "request_url": "https://old.com", "is_enabled": false }
            }
        });
        slack_update_manifest_urls(&mut manifest, "https://new.com/webhook");
        let settings = manifest.get("settings").unwrap();
        assert_eq!(
            settings["event_subscriptions"]["request_url"],
            "https://new.com/webhook"
        );
        assert_eq!(
            settings["interactivity"]["request_url"],
            "https://new.com/webhook"
        );
        assert_eq!(settings["interactivity"]["is_enabled"], true);
    }
}
