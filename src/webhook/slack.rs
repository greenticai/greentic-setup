//! Slack app manifest management.
//!
//! Calls Slack `apps.manifest.export` + `apps.manifest.update` to set event
//! subscription and interactivity URLs in the app manifest automatically.

use serde_json::{Value, json};

use super::build_webhook_url;

/// Call Slack `apps.manifest.export` + `apps.manifest.update` to set event
/// subscription and interactivity URLs in the app manifest automatically.
pub fn setup_slack_manifest(
    config: &Value,
    public_base_url: &str,
    provider_id: &str,
    tenant: &str,
    team: &str,
) -> Option<Value> {
    let app_id = config
        .get("slack_app_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let config_token = config
        .get("slack_configuration_token")
        .and_then(Value::as_str)
        .unwrap_or("");

    if app_id.is_empty() || config_token.is_empty() {
        println!(
            "  [webhook] slack manifest: skipping — slack_app_id or slack_configuration_token not provided"
        );
        return None;
    }

    let webhook_url = build_webhook_url(public_base_url, provider_id, tenant, team);

    println!(
        "  [webhook] slack manifest: exporting manifest for app_id={} webhook_url={}",
        app_id, webhook_url
    );

    // 1. Export current manifest
    let mut manifest = match export_manifest(app_id, config_token) {
        Ok(m) => m,
        Err(err_json) => {
            return Some(json!({
                "ok": false,
                "error": err_json,
                "webhook_url": webhook_url,
            }));
        }
    };

    // 2. Update manifest URLs in-place
    update_manifest_urls(&mut manifest, &webhook_url);

    println!(
        "  [webhook] slack manifest: updating manifest for app_id={}",
        app_id
    );

    // 3. Push updated manifest
    push_manifest(app_id, config_token, &manifest, &webhook_url)
}

/// Export the current Slack app manifest via `apps.manifest.export`.
fn export_manifest(app_id: &str, config_token: &str) -> Result<Value, String> {
    let resp = ureq::post("https://slack.com/api/apps.manifest.export")
        .header("Authorization", &format!("Bearer {config_token}"))
        .header("Content-Type", "application/json")
        .send_json(json!({ "app_id": app_id }));

    match resp {
        Ok(mut resp) => {
            let raw = resp.body_mut().read_to_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let ok = parsed.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if !ok {
                let err = parsed
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                println!("  [webhook] slack apps.manifest.export failed: {err}");
                return Err(format!("apps.manifest.export failed: {err}"));
            }
            parsed.get("manifest").cloned().ok_or_else(|| {
                println!(
                    "  [webhook] slack apps.manifest.export: response missing 'manifest' field"
                );
                "export response missing manifest field".to_string()
            })
        }
        Err(err) => Err(format!("apps.manifest.export request failed: {err}")),
    }
}

/// Update event_subscriptions and interactivity URLs in the manifest.
pub fn update_manifest_urls(manifest: &mut Value, webhook_url: &str) {
    if let Some(settings) = manifest.get_mut("settings").and_then(Value::as_object_mut) {
        if let Some(es) = settings
            .get_mut("event_subscriptions")
            .and_then(Value::as_object_mut)
        {
            es.insert(
                "request_url".to_string(),
                Value::String(webhook_url.to_string()),
            );
        } else {
            settings.insert(
                "event_subscriptions".to_string(),
                json!({ "request_url": webhook_url }),
            );
        }
        if let Some(ir) = settings
            .get_mut("interactivity")
            .and_then(Value::as_object_mut)
        {
            ir.insert(
                "request_url".to_string(),
                Value::String(webhook_url.to_string()),
            );
            ir.insert("is_enabled".to_string(), Value::Bool(true));
        } else {
            settings.insert(
                "interactivity".to_string(),
                json!({ "is_enabled": true, "request_url": webhook_url }),
            );
        }
    } else if let Some(obj) = manifest.as_object_mut() {
        obj.insert(
            "settings".to_string(),
            json!({
                "event_subscriptions": { "request_url": webhook_url },
                "interactivity": { "is_enabled": true, "request_url": webhook_url }
            }),
        );
    }
}

/// Push the updated manifest via `apps.manifest.update`.
fn push_manifest(
    app_id: &str,
    config_token: &str,
    manifest: &Value,
    webhook_url: &str,
) -> Option<Value> {
    let resp = ureq::post("https://slack.com/api/apps.manifest.update")
        .header("Authorization", &format!("Bearer {config_token}"))
        .header("Content-Type", "application/json")
        .send_json(json!({
            "app_id": app_id,
            "manifest": manifest,
        }));

    match resp {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let raw = resp.body_mut().read_to_string().unwrap_or_default();
            let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let ok = parsed.get("ok").and_then(Value::as_bool).unwrap_or(false);

            println!(
                "  [webhook] slack apps.manifest.update response status={} ok={}",
                status, ok
            );

            Some(json!({
                "ok": ok,
                "webhook_url": webhook_url,
                "http_status": status,
                "slack_response": parsed,
            }))
        }
        Err(err) => Some(json!({
            "ok": false,
            "error": format!("apps.manifest.update request failed: {err}"),
            "webhook_url": webhook_url,
        })),
    }
}
