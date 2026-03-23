//! Environment variable placeholder handling.
//!
//! Functions for collecting, resolving, and applying environment variable
//! placeholders in setup answers (e.g., `${PUBLIC_BASE_URL}`).

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write as _};

use anyhow::{Result, bail};

use crate::engine::LoadedAnswers;

/// Represents an environment variable placeholder found in answers.
#[derive(Debug, Clone)]
pub struct EnvVarPlaceholder {
    /// The placeholder string (e.g., "${PUBLIC_BASE_URL}")
    pub placeholder: String,
    /// The environment variable name (e.g., "PUBLIC_BASE_URL")
    pub var_name: String,
    /// The resolved value from environment, if available
    pub resolved_value: Option<String>,
    /// Which providers use this placeholder
    pub used_by: Vec<String>,
}

/// Collect all environment variable placeholders from loaded answers.
pub fn collect_env_var_placeholders(loaded: &LoadedAnswers) -> Vec<EnvVarPlaceholder> {
    let mut placeholders: BTreeMap<String, EnvVarPlaceholder> = BTreeMap::new();

    // Check platform_setup.static_routes.public_base_url
    if let Some(ref routes) = loaded.platform_setup.static_routes
        && let Some(ref value) = routes.public_base_url
        && let Some(var_name) = extract_env_var_name(value)
    {
        let entry = placeholders
            .entry(var_name.clone())
            .or_insert_with(|| EnvVarPlaceholder {
                placeholder: value.to_string(),
                var_name: var_name.clone(),
                resolved_value: std::env::var(&var_name).ok(),
                used_by: Vec::new(),
            });
        entry.used_by.push("platform_setup".to_string());
    }

    // Check each provider's answers
    for (provider_id, answers) in &loaded.setup_answers {
        if let Some(obj) = answers.as_object() {
            for (key, value) in obj {
                if let Some(s) = value.as_str()
                    && let Some(var_name) = extract_env_var_name(s)
                {
                    let entry =
                        placeholders
                            .entry(var_name.clone())
                            .or_insert_with(|| EnvVarPlaceholder {
                                placeholder: s.to_string(),
                                var_name: var_name.clone(),
                                resolved_value: std::env::var(&var_name).ok(),
                                used_by: Vec::new(),
                            });
                    let provider_key = format!("{provider_id}.{key}");
                    if !entry.used_by.contains(&provider_key) {
                        entry.used_by.push(provider_key);
                    }
                }
            }
        }
    }

    placeholders.into_values().collect()
}

/// Extract environment variable name from a placeholder like "${VAR_NAME}".
fn extract_env_var_name(value: &str) -> Option<String> {
    if value.starts_with("${") && value.ends_with('}') {
        Some(value[2..value.len() - 1].to_string())
    } else {
        None
    }
}

/// Display environment variable placeholders and prompt for missing values.
///
/// Returns a map of env var name -> resolved value (either from env or user input).
/// Returns `Err` if user cancels.
pub fn confirm_env_var_placeholders(
    placeholders: &[EnvVarPlaceholder],
) -> Result<HashMap<String, String>> {
    use rpassword::prompt_password;

    let mut resolved: HashMap<String, String> = HashMap::new();

    if placeholders.is_empty() {
        return Ok(resolved);
    }

    println!();
    println!("── Environment Variables ──");
    println!("The following environment variables will be used:\n");

    let mut missing: Vec<&EnvVarPlaceholder> = Vec::new();

    for placeholder in placeholders {
        match &placeholder.resolved_value {
            Some(value) => {
                // Mask sensitive values (tokens, passwords, secrets)
                let display_value = if is_sensitive_var(&placeholder.var_name) {
                    mask_value(value)
                } else {
                    value.clone()
                };
                println!(
                    "  ${:<30} \x1b[32m✓\x1b[0m {}",
                    placeholder.var_name, display_value
                );
                resolved.insert(placeholder.var_name.clone(), value.clone());
            }
            None => {
                println!("  ${:<30} \x1b[31m✗ NOT SET\x1b[0m", placeholder.var_name);
                missing.push(placeholder);
            }
        };
    }

    println!();

    // Prompt for missing values
    if !missing.is_empty() {
        println!("Enter values for missing environment variables:");
        println!("(Press Enter to skip and keep placeholder, or 'q' to cancel)\n");

        for placeholder in missing {
            let is_sensitive = is_sensitive_var(&placeholder.var_name);
            let prompt = format!("  ${}: ", placeholder.var_name);

            let input = if is_sensitive {
                // Use secure password input for sensitive values
                print!("{}", prompt);
                io::stdout().flush()?;
                prompt_password("").unwrap_or_default()
            } else {
                print!("{}", prompt);
                io::stdout().flush()?;
                let mut buf = String::new();
                io::stdin().read_line(&mut buf)?;
                buf.trim().to_string()
            };

            if input.eq_ignore_ascii_case("q") {
                bail!("Setup cancelled by user");
            }

            if !input.is_empty() {
                resolved.insert(placeholder.var_name.clone(), input);
            }
        }

        println!();
    }

    Ok(resolved)
}

/// Apply resolved environment variable values to loaded answers.
///
/// Replaces `${VAR_NAME}` placeholders with actual values from the resolved map.
pub fn apply_resolved_env_vars(loaded: &mut LoadedAnswers, resolved: &HashMap<String, String>) {
    // Apply to platform_setup.static_routes.public_base_url
    if let Some(ref mut routes) = loaded.platform_setup.static_routes
        && let Some(ref mut value) = routes.public_base_url
        && let Some(var_name) = extract_env_var_name(value)
        && let Some(resolved_value) = resolved.get(&var_name)
    {
        *value = resolved_value.clone();
    }

    // Apply to each provider's answers
    for (_provider_id, answers) in loaded.setup_answers.iter_mut() {
        if let Some(obj) = answers.as_object_mut() {
            for (_key, value) in obj.iter_mut() {
                if let Some(s) = value.as_str()
                    && let Some(var_name) = extract_env_var_name(s)
                    && let Some(resolved_value) = resolved.get(&var_name)
                {
                    *value = serde_json::Value::String(resolved_value.clone());
                }
            }
        }
    }
}

/// Check if a variable name suggests sensitive data.
fn is_sensitive_var(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("token")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("key")
        || lower.contains("credential")
}

/// Mask a sensitive value, showing only first and last 4 characters.
fn mask_value(value: &str) -> String {
    if value.len() <= 12 {
        "*".repeat(value.len())
    } else {
        format!("{}...{}", &value[..4], &value[value.len() - 4..])
    }
}
