//! Telegram webhook registration.
//!
//! Calls the Telegram Bot API `setWebhook` endpoint to register the
//! operator's ingress endpoint for incoming messages.

use serde_json::{Value, json};

use super::build_webhook_url;

/// Call Telegram Bot API `setWebhook` to register the webhook URL.
pub fn setup_telegram_webhook(
    config: &Value,
    public_base_url: &str,
    provider_id: &str,
    tenant: &str,
    team: &str,
) -> Option<Value> {
    // Try bot_token first, then fall back to telegram_bot_token
    // (secret-requirements.json uses TELEGRAM_BOT_TOKEN which becomes telegram_bot_token)
    let bot_token = config
        .get("bot_token")
        .or_else(|| config.get("telegram_bot_token"))
        .and_then(Value::as_str)?;
    if bot_token.is_empty() {
        return Some(json!({"ok": false, "error": "bot_token is empty"}));
    }

    let api_base = config
        .get("api_base_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.contains("telegram.org"))
        .unwrap_or("https://api.telegram.org");

    let webhook_url = build_webhook_url(public_base_url, provider_id, tenant, team);

    let url = format!("{api_base}/bot{bot_token}/setWebhook");
    let body = json!({
        "url": webhook_url,
        "allowed_updates": ["message", "callback_query", "edited_message"]
    });

    let token_preview = if bot_token.len() > 10 {
        format!(
            "{}...{}",
            &bot_token[..5],
            &bot_token[bot_token.len() - 4..]
        )
    } else {
        "***".to_string()
    };
    println!(
        "  [webhook] telegram setWebhook url={} token_preview={} api={}",
        webhook_url, token_preview, api_base
    );

    match ureq::post(&url)
        .header("Content-Type", "application/json")
        .send_json(&body)
    {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let raw_body = resp.body_mut().read_to_string().unwrap_or_default();
            println!(
                "  [webhook] telegram setWebhook response status={} body={}",
                status, raw_body
            );
            let resp_body: Value = serde_json::from_str(&raw_body).unwrap_or(Value::Null);
            let tg_ok = resp_body
                .get("ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let description = resp_body
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            Some(json!({
                "ok": tg_ok,
                "webhook_url": webhook_url,
                "description": description,
                "http_status": status,
                "telegram_response": resp_body,
            }))
        }
        Err(err) => Some(json!({
            "ok": false,
            "error": format!("request failed: {err}"),
            "webhook_url": webhook_url,
        })),
    }
}
