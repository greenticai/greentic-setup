//! Legacy post-setup instruction printer.
//!
//! Provider-specific setup guidance should come from provider packs through
//! setup actions or pack metadata, not from hardcoded setup-side branches.

use serde_json::Value;

/// A single provider's manual setup instructions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderInstruction {
    pub provider_name: String,
    pub steps: Vec<String>,
}

/// Collect post-setup instructions for providers that need manual intervention.
///
/// This intentionally returns no hardcoded provider guidance. Packs should
/// declare setup actions and instruction metadata themselves.
pub fn collect_post_setup_instructions(
    providers: &[(String, Value)],
    tenant: &str,
    team: &str,
) -> Vec<ProviderInstruction> {
    let _ = (providers, tenant, team);
    Vec::new()
}

/// Print post-setup instructions for providers that need manual intervention.
pub fn print_post_setup_instructions(providers: &[(String, Value)], tenant: &str, team: &str) {
    let instructions = collect_post_setup_instructions(providers, tenant, team);

    if instructions.is_empty() {
        return;
    }

    println!();
    println!("──────────────────────────────────────────────────────────");
    println!("  Manual steps required:");
    println!("──────────────────────────────────────────────────────────");
    for instr in &instructions {
        println!();
        println!("  [{}]", instr.provider_name);
        for step in &instr.steps {
            println!("    {step}");
        }
    }
    println!();
    println!("──────────────────────────────────────────────────────────");
}

#[cfg(test)]
mod tests {
    use super::collect_post_setup_instructions;
    use serde_json::json;

    #[test]
    fn slack_does_not_emit_legacy_manual_app_creation_steps() {
        let providers = vec![(
            "messaging-slack".to_string(),
            json!({
                "public_base_url": "https://setup.trycloudflare.com",
                "slack_configuration_access_token": "xoxe-access",
                "slack_configuration_refresh_token": "xoxe-refresh"
            }),
        )];

        let instructions = collect_post_setup_instructions(&providers, "demo", "default");

        assert!(instructions.is_empty());
    }

    #[test]
    fn teams_does_not_emit_hardcoded_manual_steps() {
        let providers = vec![(
            "messaging-teams".to_string(),
            json!({
                "public_base_url": "https://setup.trycloudflare.com",
                "client_id": "public-client-id"
            }),
        )];

        let instructions = collect_post_setup_instructions(&providers, "demo", "default");

        assert!(instructions.is_empty());
    }
}
