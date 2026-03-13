//! QA-aware setup wizard that unifies WASM-based `qa-spec` and legacy
//! `setup.yaml` into a single FormSpec-driven flow.
//!
//! Provides both interactive CLI prompts and Adaptive Card rendering
//! for collecting provider configuration answers.

use std::io::{self, Write as _};
use std::path::Path;

use anyhow::{Result, anyhow};
use qa_spec::spec::form::ProgressPolicy;
use qa_spec::{
    FormSpec, QuestionSpec, QuestionType, VisibilityMode, build_render_payload, render_card,
    resolve_visibility,
};
use rpassword::prompt_password;
use serde_json::{Map as JsonMap, Value};

use crate::qa::bridge;
use crate::setup_input::SetupInputAnswers;
use crate::setup_to_formspec;

/// Run the QA setup wizard for a provider pack.
///
/// Builds a `FormSpec` from `setup.yaml` (or uses a pre-built one from a
/// component `qa-spec` invocation), then collects and validates answers.
///
/// Returns `(answers, form_spec)` where `form_spec` is `Some` if one was found.
pub fn run_qa_setup(
    pack_path: &Path,
    provider_id: &str,
    setup_input: Option<&SetupInputAnswers>,
    interactive: bool,
    qa_form_spec: Option<FormSpec>,
    advanced: bool,
) -> Result<(Value, Option<FormSpec>)> {
    let form_spec =
        qa_form_spec.or_else(|| setup_to_formspec::pack_to_form_spec(pack_path, provider_id));

    let answers = if let Some(input) = setup_input {
        if let Some(value) = input.answers_for_provider(provider_id) {
            let answers = crate::setup_input::ensure_object(value.clone())?;
            if let Some(ref spec) = form_spec {
                validate_answers_against_form_spec(spec, &answers)?;
            }
            answers
        } else if has_required_questions(form_spec.as_ref()) {
            return Err(anyhow!("setup input missing answers for {provider_id}"));
        } else {
            Value::Object(JsonMap::new())
        }
    } else if let Some(ref spec) = form_spec {
        if spec.questions.is_empty() {
            Value::Object(JsonMap::new())
        } else if interactive {
            prompt_form_spec_answers(spec, provider_id, advanced)?
        } else {
            return Err(anyhow!(
                "setup answers required for {provider_id} but run is non-interactive"
            ));
        }
    } else {
        Value::Object(JsonMap::new())
    };

    Ok((answers, form_spec))
}

/// Render a QA setup step as an Adaptive Card v1.3.
///
/// Returns `(card_json, next_question_id)` where `next_question_id` is `None`
/// when all visible questions have been answered.
pub fn render_qa_card(form_spec: &FormSpec, answers: &Value) -> (Value, Option<String>) {
    let mut spec = form_spec.clone();
    spec.progress_policy = Some(
        spec.progress_policy
            .map(|mut p| {
                p.skip_answered = true;
                p
            })
            .unwrap_or(ProgressPolicy {
                skip_answered: true,
                ..ProgressPolicy::default()
            }),
    );

    let ctx = serde_json::json!({});
    let payload = build_render_payload(&spec, &ctx, answers);
    let next_id = payload.next_question_id.clone();
    let mut card = render_card(&payload);

    // Ensure Action.Submit has an `id` field for the REPL's @click.
    if let Some(actions) = card.get_mut("actions").and_then(Value::as_array_mut) {
        for action in actions.iter_mut() {
            if action.get("id").is_none() {
                action["id"] = Value::String("submit".into());
            }
        }
    }

    (card, next_id)
}

/// Validate answers against a FormSpec, checking required fields and constraints.
///
/// Questions with `visible_if` expressions that evaluate to `false` are skipped.
pub fn validate_answers_against_form_spec(spec: &FormSpec, answers: &Value) -> Result<()> {
    let map = answers
        .as_object()
        .ok_or_else(|| anyhow!("setup answers must be an object"))?;

    let visibility = resolve_visibility(spec, answers, VisibilityMode::Visible);

    for question in &spec.questions {
        let visible = visibility.get(&question.id).copied().unwrap_or(true);
        if !visible {
            continue;
        }

        if question.required {
            match map.get(&question.id) {
                Some(value) if !value.is_null() => {}
                _ => {
                    return Err(anyhow!(
                        "missing required setup answer for '{}'{}",
                        question.id,
                        question
                            .description
                            .as_ref()
                            .map(|d| format!(" ({d})"))
                            .unwrap_or_default()
                    ));
                }
            }
        }

        if let Some(value) = map.get(&question.id)
            && let Some(s) = value.as_str()
            && let Some(ref constraint) = question.constraint
            && let Some(ref pattern) = constraint.pattern
            && !matches_pattern(s, pattern)
        {
            return Err(anyhow!(
                "answer for '{}' does not match pattern: {}",
                question.id,
                pattern
            ));
        }
    }

    Ok(())
}

/// Compute the visibility map for a FormSpec given the current answers.
///
/// Returns a map of `question_id → visible`. Questions without `visible_if`
/// default to visible.
pub fn compute_visibility(spec: &FormSpec, answers: &Value) -> qa_spec::VisibilityMap {
    resolve_visibility(spec, answers, VisibilityMode::Visible)
}

/// Interactively prompt the user using FormSpec questions.
///
/// Evaluates `visible_if` expressions after each answer so that conditional
/// questions are shown/hidden dynamically as answers are collected.
pub fn prompt_form_spec_answers(
    spec: &FormSpec,
    provider_id: &str,
    advanced: bool,
) -> Result<Value> {
    let display = setup_to_formspec::strip_domain_prefix(provider_id);
    let mode_label = if advanced { " (advanced)" } else { "" };
    println!("\nConfiguring {display}: {}{mode_label}", spec.title);
    if let Some(ref pres) = spec.presentation
        && let Some(ref intro) = pres.intro
    {
        println!("{intro}");
    }

    let mut answers = JsonMap::new();
    for question in &spec.questions {
        if question.id.is_empty() {
            continue;
        }
        // In normal mode, skip optional questions.
        if !advanced && !question.required {
            continue;
        }
        // Re-evaluate visibility with answers collected so far.
        if question.visible_if.is_some() {
            let current = Value::Object(answers.clone());
            let vis = resolve_visibility(spec, &current, VisibilityMode::Visible);
            if !vis.get(&question.id).copied().unwrap_or(true) {
                continue;
            }
        }
        if let Some(value) = ask_form_spec_question(question)? {
            answers.insert(question.id.clone(), value);
        }
    }
    Ok(Value::Object(answers))
}

fn ask_form_spec_question(question: &QuestionSpec) -> Result<Option<Value>> {
    // Print question header
    let marker = if question.required {
        " (required)"
    } else {
        " (optional)"
    };
    println!();
    println!("  {}{marker}", question.title);

    // Print description as contextual help
    if let Some(ref desc) = question.description
        && !desc.is_empty()
    {
        println!("  {desc}");
    }

    if let Some(ref choices) = question.choices {
        println!();
        for (idx, choice) in choices.iter().enumerate() {
            println!("    {}) {choice}", idx + 1);
        }
    }

    loop {
        let prompt = build_form_spec_prompt(question);
        let input = read_input(&prompt, question.secret)?;
        let trimmed = input.trim();

        if trimmed.is_empty() {
            if let Some(ref default) = question.default_value {
                return Ok(Some(parse_typed_value(question.kind, default)));
            }
            if question.required {
                println!("  This field is required.");
                continue;
            }
            return Ok(None);
        }

        let normalized = bridge::normalize_answer(trimmed, question.kind);

        if let Some(ref constraint) = question.constraint
            && let Some(ref pattern) = constraint.pattern
            && !matches_pattern(&normalized, pattern)
        {
            println!("  Invalid format. Expected pattern: {pattern}");
            continue;
        }

        if let Some(ref choices) = question.choices
            && !choices.is_empty()
        {
            if let Ok(idx) = normalized.parse::<usize>()
                && let Some(choice) = choices.get(idx - 1)
            {
                return Ok(Some(Value::String(choice.clone())));
            }
            if !choices.contains(&normalized) {
                println!("  Invalid choice. Options: {}", choices.join(", "));
                continue;
            }
        }

        return Ok(Some(parse_typed_value(question.kind, &normalized)));
    }
}

fn build_form_spec_prompt(question: &QuestionSpec) -> String {
    let mut prompt = String::from("  > ");
    match question.kind {
        QuestionType::Boolean => prompt.push_str("[yes/no] "),
        QuestionType::Number | QuestionType::Integer => prompt.push_str("[number] "),
        QuestionType::Enum => prompt.push_str("[choice] "),
        _ => {}
    }
    if let Some(ref default) = question.default_value
        && !default.is_empty()
    {
        prompt.push_str(&format!("(default: {default}) "));
    }
    prompt
}

fn read_input(prompt: &str, secret: bool) -> Result<String> {
    if secret {
        prompt_password(prompt).map_err(|err| anyhow!("read secret: {err}"))
    } else {
        print!("{prompt}");
        io::stdout().flush()?;
        let mut buffer = String::new();
        io::stdin().read_line(&mut buffer)?;
        Ok(buffer)
    }
}

/// Simple pattern matching for common constraint patterns.
///
/// Supports the URL pattern `^https?://\S+` used by setup specs.
pub fn matches_pattern(value: &str, pattern: &str) -> bool {
    if pattern == r"^https?://\S+" {
        (value.starts_with("http://") || value.starts_with("https://"))
            && value.len() > 8
            && !value.contains(char::is_whitespace)
    } else {
        // Unknown pattern — accept (validation is best-effort).
        true
    }
}

/// Parse a string input into the appropriate JSON value type.
pub fn parse_typed_value(kind: QuestionType, input: &str) -> Value {
    match kind {
        QuestionType::Boolean => match input.to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "1" | "on" => Value::Bool(true),
            "false" | "no" | "n" | "0" | "off" => Value::Bool(false),
            _ => Value::String(input.to_string()),
        },
        QuestionType::Number | QuestionType::Integer => {
            if let Ok(n) = input.parse::<i64>() {
                Value::Number(n.into())
            } else if let Ok(n) = input.parse::<f64>() {
                serde_json::Number::from_f64(n)
                    .map(Value::Number)
                    .unwrap_or_else(|| Value::String(input.to_string()))
            } else {
                Value::String(input.to_string())
            }
        }
        _ => Value::String(input.to_string()),
    }
}

fn has_required_questions(spec: Option<&FormSpec>) -> bool {
    spec.map(|s| s.questions.iter().any(|q| q.required))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn test_form_spec() -> FormSpec {
        FormSpec {
            id: "test-setup".into(),
            title: "Test Setup".into(),
            version: "1.0.0".into(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions: vec![
                QuestionSpec {
                    id: "api_url".into(),
                    kind: QuestionType::String,
                    title: "API URL".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: false,
                    visible_if: None,
                    constraint: Some(qa_spec::spec::Constraint {
                        pattern: Some(r"^https?://\S+".into()),
                        min: None,
                        max: None,
                        min_len: None,
                        max_len: None,
                    }),
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
                QuestionSpec {
                    id: "token".into(),
                    kind: QuestionType::String,
                    title: "Token".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: true,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
                QuestionSpec {
                    id: "optional".into(),
                    kind: QuestionType::String,
                    title: "Optional Field".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: false,
                    choices: None,
                    default_value: Some("default_val".into()),
                    secret: false,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
            ],
        }
    }

    #[test]
    fn validates_required_answers() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "https://example.com", "token": "abc"});
        assert!(validate_answers_against_form_spec(&spec, &answers).is_ok());
    }

    #[test]
    fn rejects_missing_required() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "https://example.com"});
        let err = validate_answers_against_form_spec(&spec, &answers).unwrap_err();
        assert!(err.to_string().contains("token"));
    }

    #[test]
    fn rejects_invalid_url_pattern() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "not-a-url", "token": "abc"});
        let err = validate_answers_against_form_spec(&spec, &answers).unwrap_err();
        assert!(err.to_string().contains("pattern"));
    }

    #[test]
    fn parse_typed_values() {
        assert_eq!(
            parse_typed_value(QuestionType::Boolean, "true"),
            Value::Bool(true)
        );
        assert_eq!(
            parse_typed_value(QuestionType::Boolean, "no"),
            Value::Bool(false)
        );
        assert_eq!(parse_typed_value(QuestionType::Number, "42"), json!(42));
        assert_eq!(
            parse_typed_value(QuestionType::String, "hello"),
            Value::String("hello".into())
        );
    }

    #[test]
    fn skips_invisible_required_in_validation() {
        use qa_spec::Expr;

        let spec = FormSpec {
            id: "vis-test".into(),
            title: "Visibility Test".into(),
            version: "1.0.0".into(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions: vec![
                QuestionSpec {
                    id: "trigger".into(),
                    kind: QuestionType::Boolean,
                    title: "Enable feature".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: false,
                    visible_if: None,
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
                QuestionSpec {
                    id: "dependent".into(),
                    kind: QuestionType::String,
                    title: "Dependent field".into(),
                    title_i18n: None,
                    description: None,
                    description_i18n: None,
                    required: true,
                    choices: None,
                    default_value: None,
                    secret: false,
                    visible_if: Some(Expr::Answer {
                        path: "trigger".to_string(),
                    }),
                    constraint: None,
                    list: None,
                    computed: None,
                    policy: Default::default(),
                    computed_overridable: false,
                },
            ],
        };

        // trigger=false → dependent is invisible → should pass without "dependent"
        let answers = json!({"trigger": false});
        assert!(validate_answers_against_form_spec(&spec, &answers).is_ok());

        // trigger=true → dependent is visible → should fail without "dependent"
        let answers = json!({"trigger": true});
        let err = validate_answers_against_form_spec(&spec, &answers);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("dependent"));
    }

    #[test]
    fn compute_visibility_returns_map() {
        use qa_spec::Expr;

        let spec = FormSpec {
            id: "vis-test".into(),
            title: "Test".into(),
            version: "1.0.0".into(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions: vec![QuestionSpec {
                id: "conditional".into(),
                kind: QuestionType::String,
                title: "Cond".into(),
                title_i18n: None,
                description: None,
                description_i18n: None,
                required: false,
                choices: None,
                default_value: None,
                secret: false,
                visible_if: Some(Expr::Answer {
                    path: "flag".to_string(),
                }),
                constraint: None,
                list: None,
                computed: None,
                policy: Default::default(),
                computed_overridable: false,
            }],
        };

        let vis = compute_visibility(&spec, &json!({"flag": true}));
        assert_eq!(vis.get("conditional"), Some(&true));

        let vis = compute_visibility(&spec, &json!({"flag": false}));
        assert_eq!(vis.get("conditional"), Some(&false));
    }

    #[test]
    fn matches_url_pattern() {
        assert!(matches_pattern("https://example.com", r"^https?://\S+"));
        assert!(matches_pattern("http://localhost:8080", r"^https?://\S+"));
        assert!(!matches_pattern("not-a-url", r"^https?://\S+"));
        assert!(!matches_pattern("https://", r"^https?://\S+")); // too short
    }

    #[test]
    fn normal_mode_skips_optional_questions() {
        let spec = test_form_spec();
        let advanced = false;
        let visible: Vec<&str> = spec
            .questions
            .iter()
            .filter(|q| !q.id.is_empty() && (advanced || q.required))
            .map(|q| q.id.as_str())
            .collect();
        // Normal mode: only required questions shown
        assert_eq!(visible, vec!["api_url", "token"]);
        assert!(!visible.contains(&"optional"));
    }

    #[test]
    fn advanced_mode_shows_all_questions() {
        let spec = test_form_spec();
        let advanced = true;
        let visible: Vec<&str> = spec
            .questions
            .iter()
            .filter(|q| !q.id.is_empty() && (advanced || q.required))
            .map(|q| q.id.as_str())
            .collect();
        // Advanced mode: all questions shown
        assert_eq!(visible, vec!["api_url", "token", "optional"]);
    }
}
