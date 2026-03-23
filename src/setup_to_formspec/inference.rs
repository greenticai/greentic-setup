//! Property inference and default value helpers for setup questions.

use qa_spec::QuestionType;
use qa_spec::spec::Constraint;

/// Infer `QuestionType`, secret flag, and optional constraint from a question ID.
///
/// Convention-based:
/// - `"enabled"` → Boolean
/// - `*_url` → String with URL pattern constraint
/// - `*_token` / `*secret*` / `*password*` → String, secret
pub fn infer_question_properties(id: &str) -> (QuestionType, bool, Option<Constraint>) {
    match id {
        "enabled" => (QuestionType::Boolean, false, None),
        id if id.ends_with("_url") || id == "public_base_url" || id == "api_base_url" => (
            QuestionType::String,
            false,
            Some(Constraint {
                pattern: Some(r"^https?://\S+".to_string()),
                min: None,
                max: None,
                min_len: None,
                max_len: None,
            }),
        ),
        id if id.ends_with("_token") || id.contains("secret") || id.contains("password") => {
            (QuestionType::String, true, None)
        }
        _ => (QuestionType::String, false, None),
    }
}

/// Infer a default value for well-known question IDs.
///
/// Returns `Some(default)` for known fields with standard defaults:
/// - `api_base_url` for Slack → `https://slack.com/api`
/// - `api_base_url` for Telegram → `https://api.telegram.org`
/// - `enabled` → `true`
pub fn infer_default_for_id(id: &str, provider_id: &str) -> Option<String> {
    match id {
        "api_base_url" => {
            if provider_id.contains("slack") {
                Some("https://slack.com/api".to_string())
            } else if provider_id.contains("telegram") {
                Some("https://api.telegram.org".to_string())
            } else {
                None
            }
        }
        "enabled" => Some("true".to_string()),
        _ => None,
    }
}

/// Strip common domain prefixes from a provider ID for display.
pub fn strip_domain_prefix(provider_id: &str) -> String {
    provider_id
        .strip_prefix("messaging-")
        .or_else(|| provider_id.strip_prefix("events-"))
        .or_else(|| provider_id.strip_prefix("oauth-"))
        .unwrap_or(provider_id)
        .to_string()
}

/// Capitalize the first character of a string.
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

/// Extract default value from help text.
///
/// Matches patterns like:
/// - "(default: https://slack.com/api)"
/// - "[default: true]"
/// - "Default: some_value"
pub fn extract_default_from_help(help: &str) -> Option<String> {
    use regex::Regex;

    // Pattern: (default: VALUE) or [default: VALUE]
    let re = Regex::new(r"(?i)[\(\[]?\s*default:\s*([^\)\]\n,]+)\s*[\)\]]?").ok()?;
    if let Some(caps) = re.captures(help) {
        let value = caps.get(1)?.as_str().trim();
        // Clean up the value - remove trailing punctuation
        let cleaned = value.trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace());
        if !cleaned.is_empty() {
            return Some(cleaned.to_string());
        }
    }

    None
}
