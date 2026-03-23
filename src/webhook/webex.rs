//! Webex webhook management.
//!
//! Registers (or updates) Webex webhooks so incoming messages AND card actions
//! are forwarded to the operator's ingress endpoint.

use serde_json::{Value, json};

use super::build_webhook_url;

/// Register (or update) Webex webhooks so incoming messages AND card actions
/// are forwarded to the operator's ingress endpoint.
///
/// Two webhooks are managed:
/// - `messages.created` — new text/file messages
/// - `attachmentActions.created` — Adaptive Card button clicks
///
/// Flow: list existing webhooks → find matching ones by name → create or update.
pub fn setup_webex_webhook(
    config: &Value,
    public_base_url: &str,
    provider_id: &str,
    tenant: &str,
    team: &str,
) -> Option<Value> {
    let bot_token = config
        .get("bot_token")
        .or_else(|| config.get("webex_bot_token"))
        .and_then(Value::as_str)
        .unwrap_or("");

    if bot_token.is_empty() {
        println!("  [webhook] webex webhook: skipping — bot_token not provided");
        return None;
    }

    let api_base = config
        .get("api_base_url")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("https://webexapis.com/v1");

    let webhook_url = build_webhook_url(public_base_url, provider_id, tenant, team);
    let base_name = format!("greentic:{}:{}:webex", tenant, team);

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
        "  [webhook] webex webhook: target_url={} name={} token_preview={}",
        webhook_url, base_name, token_preview
    );

    // 1. List existing webhooks
    let existing = match list_webhooks(api_base, bot_token) {
        Ok(hooks) => hooks,
        Err(err) => {
            return Some(json!({
                "ok": false,
                "error": err,
                "webhook_url": webhook_url,
            }));
        }
    };

    // 2. Reconcile both webhook types
    let subscriptions: &[(&str, &str, &str)] = &[
        ("messages", "created", &base_name),
        (
            "attachmentActions",
            "created",
            &format!("{base_name}:cards"),
        ),
    ];

    let mut results = Vec::new();
    let mut all_ok = true;

    for &(resource, event, name) in subscriptions {
        let result = reconcile_one(
            api_base,
            bot_token,
            &existing,
            name,
            &webhook_url,
            resource,
            event,
        );
        if let Some(ref r) = result
            && !r.get("ok").and_then(Value::as_bool).unwrap_or(false)
        {
            all_ok = false;
        }
        results.push(json!({
            "resource": resource,
            "event": event,
            "name": name,
            "result": result,
        }));
    }

    Some(json!({
        "ok": all_ok,
        "webhook_url": webhook_url,
        "webhooks": results,
    }))
}

/// Reconcile a single Webex webhook: find by name → create or update.
fn reconcile_one(
    api_base: &str,
    token: &str,
    existing: &[Value],
    name: &str,
    target_url: &str,
    resource: &str,
    event: &str,
) -> Option<Value> {
    let matching = existing
        .iter()
        .find(|hook| hook.get("name").and_then(Value::as_str) == Some(name));

    if let Some(hook) = matching {
        let hook_id = hook.get("id").and_then(Value::as_str).unwrap_or("");
        let current_url = hook.get("targetUrl").and_then(Value::as_str).unwrap_or("");

        if current_url == target_url {
            println!(
                "  [webhook] webex webhook: already up-to-date name={} id={}",
                name, hook_id
            );
            return Some(json!({
                "ok": true,
                "webhook_id": hook_id,
                "action": "noop",
            }));
        }

        println!(
            "  [webhook] webex webhook: updating name={} id={} old_url={}",
            name, hook_id, current_url
        );
        update_webhook(api_base, token, hook_id, name, target_url)
    } else {
        println!(
            "  [webhook] webex webhook: creating name={} resource={} event={}",
            name, resource, event
        );
        create_webhook_with_resource(api_base, token, name, target_url, resource, event)
    }
}

/// List all webhooks registered for the bot.
fn list_webhooks(api_base: &str, token: &str) -> Result<Vec<Value>, String> {
    let url = format!("{}/webhooks", api_base.trim_end_matches('/'));
    match ureq::get(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .call()
    {
        Ok(mut resp) => {
            let raw = resp.body_mut().read_to_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            Ok(parsed
                .get("items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default())
        }
        Err(err) => Err(format!("GET /webhooks failed: {err}")),
    }
}

/// Create a new Webex webhook for the given resource/event.
fn create_webhook_with_resource(
    api_base: &str,
    token: &str,
    name: &str,
    target_url: &str,
    resource: &str,
    event: &str,
) -> Option<Value> {
    let url = format!("{}/webhooks", api_base.trim_end_matches('/'));
    let body = json!({
        "name": name,
        "targetUrl": target_url,
        "resource": resource,
        "event": event,
    });

    match ureq::post(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send_json(&body)
    {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let raw = resp.body_mut().read_to_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let hook_id = parsed
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            println!(
                "  [webhook] webex webhook: created id={} status={}",
                hook_id, status
            );

            Some(json!({
                "ok": (200..300).contains(&status),
                "webhook_url": target_url,
                "webhook_id": hook_id,
                "action": "create",
                "http_status": status,
                "webex_response": parsed,
            }))
        }
        Err(err) => Some(json!({
            "ok": false,
            "error": format!("POST /webhooks failed: {err}"),
            "webhook_url": target_url,
        })),
    }
}

/// Update an existing Webex webhook's target URL.
fn update_webhook(
    api_base: &str,
    token: &str,
    webhook_id: &str,
    name: &str,
    target_url: &str,
) -> Option<Value> {
    let url = format!("{}/webhooks/{}", api_base.trim_end_matches('/'), webhook_id);
    let body = json!({
        "name": name,
        "targetUrl": target_url,
    });

    match ureq::put(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send_json(&body)
    {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let raw = resp.body_mut().read_to_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

            println!(
                "  [webhook] webex webhook: updated id={} status={}",
                webhook_id, status
            );

            Some(json!({
                "ok": (200..300).contains(&status),
                "webhook_url": target_url,
                "webhook_id": webhook_id,
                "action": "update",
                "http_status": status,
                "webex_response": parsed,
            }))
        }
        Err(err) => Some(json!({
            "ok": false,
            "error": format!("PUT /webhooks/{} failed: {err}", webhook_id),
            "webhook_url": target_url,
        })),
    }
}
