//! Provider-level setup state shared by CLI, web UI, and executors.

use serde_json::{Map, Value};

pub const ENABLED_FIELD: &str = "enabled";

pub fn provider_enabled(answers: &Value) -> bool {
    answers
        .as_object()
        .map(provider_enabled_from_map)
        .unwrap_or(true)
}

pub fn provider_enabled_from_map(answers: &Map<String, Value>) -> bool {
    answers
        .get(ENABLED_FIELD)
        .map(value_is_enabled)
        .unwrap_or(true)
}

pub fn value_is_enabled(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::String(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(
                normalized.as_str(),
                "false" | "0" | "no" | "off" | "disabled"
            )
        }
        Value::Null => true,
        Value::Number(value) => value.as_i64() != Some(0),
        Value::Array(_) | Value::Object(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::provider_enabled;
    use serde_json::json;

    #[test]
    fn provider_enabled_defaults_true_and_accepts_false_strings() {
        assert!(provider_enabled(&json!({})));
        assert!(!provider_enabled(&json!({"enabled": false})));
        assert!(!provider_enabled(&json!({"enabled": "off"})));
        assert!(!provider_enabled(&json!({"enabled": "disabled"})));
        assert!(provider_enabled(&json!({"enabled": "true"})));
    }
}
