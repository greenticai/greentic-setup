//! Post-setup instructions for messaging providers.
//!
//! Some providers (e.g. Teams, WhatsApp) cannot be fully automated and require
//! the user to complete additional steps in external portals.

use serde_json::Value;

/// Print post-setup instructions for providers that need manual intervention.
///
/// Some providers (e.g. Teams, WhatsApp) cannot be fully automated and require
/// the user to complete additional steps in external portals.
pub fn print_post_setup_instructions(providers: &[(String, Value)], tenant: &str, team: &str) {
    let mut instructions: Vec<(&str, Vec<String>)> = Vec::new();

    for (provider_id, config) in providers {
        let provider_short = provider_id
            .strip_prefix("messaging-")
            .unwrap_or(provider_id);

        let public_base_url = config
            .get("public_base_url")
            .and_then(Value::as_str)
            .unwrap_or("<your-public-url>");

        match provider_short {
            "teams" => {
                let webhook_url = format!(
                    "{}/v1/messaging/ingress/{}/{}/{}",
                    public_base_url.trim_end_matches('/'),
                    provider_id,
                    tenant,
                    team,
                );
                instructions.push(("Microsoft Teams", vec![
                    "1. Go to Azure Portal → Bot Services → your bot".into(),
                    format!("2. Set Messaging Endpoint to: {webhook_url}"),
                    "3. Ensure the App ID and Password match your answers file".into(),
                    "4. Grant API permissions (delegated): Channel.ReadBasic.All, ChannelMessage.Send, Team.ReadBasic.All, ChatMessage.Send".into(),
                    "5. If using Graph API: complete OAuth flow to obtain a refresh token".into(),
                    "   → See: docs/guides/providers/guide-teams-setup.md".into(),
                ]));
            }
            "whatsapp" => {
                let webhook_url = format!(
                    "{}/v1/messaging/ingress/{}/{}/{}",
                    public_base_url.trim_end_matches('/'),
                    provider_id,
                    tenant,
                    team,
                );
                instructions.push((
                    "WhatsApp",
                    vec![
                        "1. Go to Meta Developer Portal → WhatsApp → Configuration".into(),
                        format!("2. Set Webhook URL to: {webhook_url}"),
                        "3. Set Verify Token to match your config (if configured)".into(),
                        "4. Subscribe to webhook fields: messages".into(),
                    ],
                ));
            }
            "webex" => {
                // Webex webhooks are auto-registered, but mention bot creation
                // Check both webex_bot_token (canonical from WEBEX_BOT_TOKEN) and bot_token (QA field)
                let webex_token = config.get("webex_bot_token");
                let bot_token = config.get("bot_token");
                eprintln!(
                    "  [debug] webex instruction check: webex_bot_token={:?} bot_token={:?}",
                    webex_token.map(|v| if v.as_str().map(|s| s.len()).unwrap_or(0) > 10 {
                        "***"
                    } else {
                        "empty"
                    }),
                    bot_token.map(|v| if v.as_str().map(|s| s.len()).unwrap_or(0) > 10 {
                        "***"
                    } else {
                        "empty"
                    })
                );
                let has_token = webex_token
                    .or(bot_token)
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                if !has_token {
                    instructions.push((
                        "Webex",
                        vec![
                            "1. Create a Webex Bot at: https://developer.webex.com/my-apps/new/bot"
                                .into(),
                            "2. Copy the bot access token into your answers file as 'webex_bot_token'"
                                .into(),
                            "3. Re-run setup to register webhooks automatically".into(),
                        ],
                    ));
                }
            }
            "slack" => {
                // Slack manifest is auto-updated, but mention app creation
                let has_app_id = config
                    .get("slack_app_id")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty());
                if !has_app_id {
                    instructions.push(("Slack", vec![
                        "1. Create a Slack App at: https://api.slack.com/apps".into(),
                        "2. Add 'slack_app_id' and 'slack_configuration_token' to your answers file".into(),
                        "3. Re-run setup to update the app manifest automatically".into(),
                    ]));
                }
            }
            _ => {}
        }
    }

    if instructions.is_empty() {
        return;
    }

    println!();
    println!("──────────────────────────────────────────────────────────");
    println!("  Manual steps required:");
    println!("──────────────────────────────────────────────────────────");
    for (provider_name, steps) in &instructions {
        println!();
        println!("  [{provider_name}]");
        for step in steps {
            println!("    {step}");
        }
    }
    println!();
    println!("──────────────────────────────────────────────────────────");
}
