//! Bridge between provider QA specs and greentic-qa's FormSpec engine.
//!
//! Providers return a simple list of `(id, i18n_key, required)` questions.
//! This module converts that into a full `qa_spec::FormSpec` so the setup
//! engine can drive wizard flows using greentic-qa's visibility, progress,
//! validation, and rendering.

use std::collections::{BTreeMap, HashMap};

use qa_spec::{
    Expr, FormSpec, I18nText, QuestionSpec, QuestionType, ResolvedI18nMap,
    spec::{FormPresentation, ProgressPolicy},
};
use serde_json::Value;

use crate::setup_to_formspec::{capitalize, infer_question_properties, strip_domain_prefix};

/// Convert provider QA spec JSON output + i18n translations into a `FormSpec`.
///
/// The provider's `qa-spec` output looks like:
/// ```json
/// {
///   "mode": "setup",
///   "title": {"key": "telegram.qa.setup.title"},
///   "questions": [
///     {"id": "enabled", "label": {"key": "telegram.qa.setup.enabled"}, "required": true}
///   ]
/// }
/// ```
pub fn provider_qa_to_form_spec(
    qa_output: &Value,
    i18n: &HashMap<String, String>,
    provider: &str,
) -> FormSpec {
    let mode = qa_output
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("setup");

    let title_key = qa_output
        .get("title")
        .and_then(|t| t.get("key").and_then(Value::as_str))
        .unwrap_or("");
    let title = i18n
        .get(title_key)
        .cloned()
        .unwrap_or_else(|| format!("{} setup", provider));

    let questions = qa_output
        .get("questions")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|q| convert_question(q, i18n, provider))
                .collect()
        })
        .unwrap_or_default();

    let display_name = capitalize(&strip_domain_prefix(provider));

    FormSpec {
        id: format!("{provider}-{mode}"),
        title,
        version: "1.0.0".to_string(),
        description: Some(format!("{display_name} provider configuration")),
        presentation: Some(FormPresentation {
            intro: Some(format!(
                "Configure {display_name} provider settings.\n\
                 Fields marked with * are required."
            )),
            theme: None,
            default_locale: Some("en".to_string()),
        }),
        progress_policy: Some(ProgressPolicy {
            skip_answered: false,
            autofill_defaults: false,
            treat_default_as_answered: false,
        }),
        secrets_policy: None,
        store: vec![],
        validations: vec![],
        includes: vec![],
        questions,
    }
}

fn convert_question(
    q: &Value,
    i18n: &HashMap<String, String>,
    _provider: &str,
) -> Option<QuestionSpec> {
    let id = q.get("id").and_then(Value::as_str)?.to_string();

    let label_key = q
        .get("label")
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.get("key").and_then(Value::as_str).map(String::from))
        })
        .unwrap_or_else(|| id.clone());

    let title = i18n.get(&label_key).cloned().unwrap_or_else(|| id.clone());
    let description =
        description_key_for(&label_key, &id).and_then(|desc_key| i18n.get(&desc_key).cloned());

    let required = q.get("required").and_then(Value::as_bool).unwrap_or(false);
    let (kind, secret, constraint) = infer_question_properties(&id);

    let default_value = q
        .get("default")
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .or_else(|| infer_default(&kind));

    let visible_if = parse_visible_if(q);

    Some(QuestionSpec {
        id,
        kind,
        title: title.clone(),
        title_i18n: Some(I18nText {
            key: label_key.clone(),
            args: None,
        }),
        description: description.clone(),
        description_i18n: description_key_for(
            &label_key,
            q.get("id").and_then(Value::as_str).unwrap_or(""),
        )
        .map(|key| I18nText { key, args: None }),
        required,
        choices: None,
        default_value,
        secret,
        visible_if,
        constraint,
        list: None,
        computed: None,
        policy: Default::default(),
        computed_overridable: false,
    })
}

/// Parse a `visible_if` expression from the provider QA question JSON.
///
/// Supports three formats:
/// - `{"field": "q1", "eq": "true"}` → `Eq(Answer(q1), Literal(true))`
/// - `{"op": "answer", "path": "q1"}` → `Answer(q1)` (truthy check)
/// - Full `Expr` JSON (serde-compatible)
fn parse_visible_if(q: &Value) -> Option<Expr> {
    let vis = q.get("visible_if")?;

    // Format 1: {"field": "q1", "eq": "value"}
    if let Some(field) = vis.get("field").and_then(Value::as_str) {
        if let Some(eq_val) = vis.get("eq").and_then(Value::as_str) {
            return Some(Expr::Eq {
                left: Box::new(Expr::Answer {
                    path: field.to_string(),
                }),
                right: Box::new(Expr::Literal {
                    value: Value::String(eq_val.to_string()),
                }),
            });
        }
        // No "eq" → truthy check on the field
        return Some(Expr::Answer {
            path: field.to_string(),
        });
    }

    // Format 2: {"op": "answer"|"var"|"is_set"|"not", ...}
    if vis.get("op").is_some()
        && let Ok(expr) = serde_json::from_value::<Expr>(vis.clone())
    {
        return Some(expr);
    }

    // Format 3: Full Expr serde-compatible JSON
    serde_json::from_value::<Expr>(vis.clone()).ok()
}

fn infer_default(kind: &QuestionType) -> Option<String> {
    match kind {
        QuestionType::Boolean => Some("true".to_string()),
        _ => None,
    }
}

/// Derive the i18n description key from a label key and question id.
///
/// `telegram.qa.setup.enabled` + `enabled` → `telegram.schema.config.enabled.description`
fn description_key_for(label_key: &str, question_id: &str) -> Option<String> {
    let prefix = label_key.split(".qa.").next()?;
    Some(format!("{prefix}.schema.config.{question_id}.description"))
}

/// Build a `ResolvedI18nMap` from the provider's i18n bundle.
///
/// greentic-qa expects keys like `"key"` or `"en:key"`.
/// We insert both forms for maximum compatibility.
pub fn build_resolved_i18n(i18n: &HashMap<String, String>) -> ResolvedI18nMap {
    let mut resolved = BTreeMap::new();
    for (key, value) in i18n {
        resolved.insert(key.clone(), value.clone());
        resolved.insert(format!("en:{key}"), value.clone());
    }
    resolved
}

/// Normalize a user's answer based on the question type.
///
/// For boolean questions, converts natural language (yes/no/y/n) to "true"/"false".
pub fn normalize_answer(answer: &str, kind: QuestionType) -> String {
    match kind {
        QuestionType::Boolean => match answer.to_ascii_lowercase().as_str() {
            "yes" | "y" | "true" | "1" | "on" => "true".to_string(),
            "no" | "n" | "false" | "0" | "off" => "false".to_string(),
            _ => answer.to_string(),
        },
        _ => answer.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_qa_output() -> Value {
        json!({
            "mode": "setup",
            "title": {"key": "telegram.qa.setup.title"},
            "questions": [
                {"id": "enabled", "label": {"key": "telegram.qa.setup.enabled"}, "required": true},
                {"id": "public_base_url", "label": {"key": "telegram.qa.setup.public_base_url"}, "required": true},
                {"id": "default_chat_id", "label": {"key": "telegram.qa.setup.default_chat_id"}, "required": false},
                {"id": "api_base_url", "label": {"key": "telegram.qa.setup.api_base_url"}, "required": true},
                {"id": "bot_token", "label": {"key": "telegram.qa.setup.bot_token"}, "required": false},
            ]
        })
    }

    fn sample_i18n() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("telegram.qa.setup.title".into(), "Setup".into());
        m.insert("telegram.qa.setup.enabled".into(), "Enable provider".into());
        m.insert(
            "telegram.qa.setup.public_base_url".into(),
            "Public base URL".into(),
        );
        m.insert(
            "telegram.qa.setup.default_chat_id".into(),
            "Default chat ID".into(),
        );
        m.insert(
            "telegram.qa.setup.api_base_url".into(),
            "API base URL".into(),
        );
        m.insert("telegram.qa.setup.bot_token".into(), "Bot token".into());
        m.insert(
            "telegram.schema.config.enabled.description".into(),
            "Enable this provider".into(),
        );
        m.insert(
            "telegram.schema.config.public_base_url.description".into(),
            "Public URL for webhook callbacks".into(),
        );
        m.insert(
            "telegram.schema.config.bot_token.description".into(),
            "Bot token for Telegram API calls".into(),
        );
        m
    }

    #[test]
    fn converts_provider_qa_to_form_spec() {
        let form =
            provider_qa_to_form_spec(&sample_qa_output(), &sample_i18n(), "messaging-telegram");
        assert_eq!(form.id, "messaging-telegram-setup");
        assert_eq!(form.title, "Setup");
        assert_eq!(form.questions.len(), 5);
    }

    #[test]
    fn infers_question_types() {
        let form =
            provider_qa_to_form_spec(&sample_qa_output(), &sample_i18n(), "messaging-telegram");
        assert_eq!(form.questions[0].kind, QuestionType::Boolean);
        assert_eq!(form.questions[1].kind, QuestionType::String);
        assert!(form.questions[1].constraint.is_some());
        assert!(form.questions[4].secret);
    }

    #[test]
    fn resolves_titles_from_i18n() {
        let form =
            provider_qa_to_form_spec(&sample_qa_output(), &sample_i18n(), "messaging-telegram");
        assert_eq!(form.questions[0].title, "Enable provider");
        assert_eq!(form.questions[4].title, "Bot token");
    }

    #[test]
    fn resolves_descriptions_from_i18n() {
        let form =
            provider_qa_to_form_spec(&sample_qa_output(), &sample_i18n(), "messaging-telegram");
        assert_eq!(
            form.questions[0].description.as_deref(),
            Some("Enable this provider")
        );
        assert_eq!(
            form.questions[4].description.as_deref(),
            Some("Bot token for Telegram API calls")
        );
    }

    #[test]
    fn normalizes_boolean_answers() {
        assert_eq!(normalize_answer("yes", QuestionType::Boolean), "true");
        assert_eq!(normalize_answer("No", QuestionType::Boolean), "false");
        assert_eq!(normalize_answer("y", QuestionType::Boolean), "true");
        assert_eq!(normalize_answer("hello", QuestionType::String), "hello");
    }

    #[test]
    fn parses_visible_if_field_eq() {
        let qa = json!({
            "mode": "setup",
            "title": {"key": "test.title"},
            "questions": [
                {"id": "enable_redis", "label": "Enable Redis", "required": false},
                {
                    "id": "redis_password",
                    "label": "Redis password",
                    "required": false,
                    "visible_if": {"field": "enable_redis", "eq": "true"}
                }
            ]
        });
        let form = provider_qa_to_form_spec(&qa, &HashMap::new(), "state-redis");
        assert!(form.questions[0].visible_if.is_none());
        assert!(form.questions[1].visible_if.is_some());
    }

    #[test]
    fn parses_visible_if_truthy_field() {
        let qa = json!({
            "mode": "setup",
            "title": {"key": "test.title"},
            "questions": [
                {"id": "advanced", "label": "Advanced mode", "required": false},
                {
                    "id": "debug_level",
                    "label": "Debug level",
                    "required": false,
                    "visible_if": {"field": "advanced"}
                }
            ]
        });
        let form = provider_qa_to_form_spec(&qa, &HashMap::new(), "test-provider");
        assert!(form.questions[1].visible_if.is_some());
    }

    #[test]
    fn builds_resolved_i18n_map() {
        let i18n = sample_i18n();
        let resolved = build_resolved_i18n(&i18n);
        assert_eq!(
            resolved
                .get("telegram.qa.setup.enabled")
                .map(String::as_str),
            Some("Enable provider")
        );
        assert_eq!(
            resolved
                .get("en:telegram.qa.setup.enabled")
                .map(String::as_str),
            Some("Enable provider")
        );
    }
}
