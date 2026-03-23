//! QA-aware setup wizard that unifies WASM-based `qa-spec` and legacy
//! `setup.yaml` into a single FormSpec-driven flow.
//!
//! Provides both interactive CLI prompts and Adaptive Card rendering
//! for collecting provider configuration answers.

use std::path::Path;

use anyhow::{Result, anyhow};
use qa_spec::spec::form::ProgressPolicy;
use qa_spec::{FormSpec, VisibilityMode, build_render_payload, render_card, resolve_visibility};
use serde_json::{Map as JsonMap, Value};

use crate::setup_input::SetupInputAnswers;
use crate::setup_to_formspec;

// Re-exports for backward compatibility (these are the public API)
pub use crate::qa::prompts::{
    answer_satisfies_question, ask_form_spec_question, has_required_questions, matches_pattern,
    parse_typed_value, prompt_form_spec_answers, prompt_form_spec_answers_with_existing,
};
pub use crate::qa::shared_questions::{
    ProviderFormSpec, SHARED_QUESTION_IDS, SharedQuestionsResult, build_provider_form_specs,
    collect_shared_questions, merge_shared_with_provider_answers, prompt_shared_questions,
};

// Internal imports for use within this module (aliased to avoid conflicts with re-exports)
use crate::qa::prompts::{
    ask_form_spec_question as prompt_question, has_required_questions as check_required,
    matches_pattern as pattern_match, prompt_form_spec_answers as do_prompt_answers,
    prompt_form_spec_answers_with_existing as do_prompt_with_existing,
};
use crate::qa::shared_questions::merge_shared_with_provider_answers as merge_answers;

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
            let mut answers = crate::setup_input::ensure_object(value.clone())?;
            if let Some(ref spec) = form_spec {
                // Check for missing required fields and prompt if needed
                let missing = find_missing_required_fields(spec, &answers);
                if !missing.is_empty() {
                    let display = setup_to_formspec::strip_domain_prefix(provider_id);
                    println!("\n⚠️  Missing required fields for {display}. Please provide values:");
                    answers = prompt_for_missing_fields(spec, &answers, &missing)?;
                }
                validate_answers_against_form_spec(spec, &answers)?;
            }
            answers
        } else if check_required(form_spec.as_ref()) {
            return Err(anyhow!("setup input missing answers for {provider_id}"));
        } else {
            Value::Object(JsonMap::new())
        }
    } else if let Some(ref spec) = form_spec {
        if spec.questions.is_empty() {
            Value::Object(JsonMap::new())
        } else if interactive {
            do_prompt_answers(spec, provider_id, advanced)?
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
            && !pattern_match(s, pattern)
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

/// Run QA setup for a provider with pre-filled shared answers.
///
/// This is a convenience wrapper around `run_qa_setup` that merges shared
/// answers with provider-specific answers from `setup_input`.
///
/// When using `--answers` file (non-interactive mode), if any required fields
/// are missing or empty, the user will be prompted to fill them in.
pub fn run_qa_setup_with_shared(
    pack_path: &Path,
    provider_id: &str,
    setup_input: Option<&SetupInputAnswers>,
    interactive: bool,
    qa_form_spec: Option<FormSpec>,
    advanced: bool,
    shared_answers: &Value,
) -> Result<(Value, Option<FormSpec>)> {
    let form_spec =
        qa_form_spec.or_else(|| setup_to_formspec::pack_to_form_spec(pack_path, provider_id));

    // Merge shared answers with provider-specific answers from setup_input
    let merged_initial = merge_answers(
        shared_answers,
        setup_input.and_then(|i| i.answers_for_provider(provider_id)),
    );

    let answers = if let Some(ref spec) = form_spec {
        if spec.questions.is_empty() {
            Value::Object(JsonMap::new())
        } else if interactive {
            // Prompt with merged initial answers (shared + provider-specific)
            do_prompt_with_existing(spec, provider_id, advanced, &merged_initial)?
        } else {
            // Non-interactive: check for missing required fields
            let mut answers = crate::setup_input::ensure_object(merged_initial)?;
            let missing = find_missing_required_fields(spec, &answers);

            if !missing.is_empty() {
                // Prompt for missing required fields
                let display = setup_to_formspec::strip_domain_prefix(provider_id);
                println!("\n⚠️  Missing required fields for {display}. Please provide values:");
                answers = prompt_for_missing_fields(spec, &answers, &missing)?;
            }

            validate_answers_against_form_spec(spec, &answers)?;
            answers
        }
    } else {
        Value::Object(JsonMap::new())
    };

    Ok((answers, form_spec))
}

/// Find required fields that are missing or have empty values.
///
/// Returns a list of question IDs that are required, visible, and either:
/// - Missing from answers
/// - Have null value
/// - Have empty string value
fn find_missing_required_fields(spec: &FormSpec, answers: &Value) -> Vec<String> {
    let map = answers.as_object();
    let visibility = resolve_visibility(spec, answers, VisibilityMode::Visible);

    spec.questions
        .iter()
        .filter(|q| {
            // Must be required
            if !q.required {
                return false;
            }
            // Must be visible
            let visible = visibility.get(&q.id).copied().unwrap_or(true);
            if !visible {
                return false;
            }
            // Check if missing or empty
            match map.and_then(|m| m.get(&q.id)) {
                None => true,                                   // Missing
                Some(Value::Null) => true,                      // Null
                Some(Value::String(s)) if s.is_empty() => true, // Empty string
                _ => false,                                     // Has value
            }
        })
        .map(|q| q.id.clone())
        .collect()
}

/// Prompt for specific missing required fields.
///
/// Only prompts for the questions whose IDs are in `missing_ids`.
fn prompt_for_missing_fields(
    spec: &FormSpec,
    existing_answers: &Value,
    missing_ids: &[String],
) -> Result<Value> {
    let mut answers = existing_answers.as_object().cloned().unwrap_or_default();

    for question in &spec.questions {
        if !missing_ids.contains(&question.id) {
            continue;
        }

        // Re-evaluate visibility with answers collected so far
        if question.visible_if.is_some() {
            let current = Value::Object(answers.clone());
            let vis = resolve_visibility(spec, &current, VisibilityMode::Visible);
            if !vis.get(&question.id).copied().unwrap_or(true) {
                continue;
            }
        }

        if let Some(value) = prompt_question(question)? {
            answers.insert(question.id.clone(), value);
        }
    }

    Ok(Value::Object(answers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use qa_spec::{QuestionSpec, QuestionType};
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
    fn normal_mode_skips_optional_questions() {
        let spec = test_form_spec();
        let advanced = false;
        let visible: Vec<&str> = spec
            .questions
            .iter()
            .filter(|q| !q.id.is_empty() && (advanced || q.required))
            .map(|q| q.id.as_str())
            .collect();
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
        assert_eq!(visible, vec!["api_url", "token", "optional"]);
    }

    // ── Missing Required Fields Tests ──────────────────────────────────────────

    #[test]
    fn find_missing_required_fields_detects_missing() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "https://example.com"});

        let missing = find_missing_required_fields(&spec, &answers);

        assert_eq!(missing.len(), 1);
        assert!(missing.contains(&"token".to_string()));
    }

    #[test]
    fn find_missing_required_fields_detects_empty_string() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "https://example.com", "token": ""});

        let missing = find_missing_required_fields(&spec, &answers);

        assert_eq!(missing.len(), 1);
        assert!(missing.contains(&"token".to_string()));
    }

    #[test]
    fn find_missing_required_fields_detects_null() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "https://example.com", "token": null});

        let missing = find_missing_required_fields(&spec, &answers);

        assert_eq!(missing.len(), 1);
        assert!(missing.contains(&"token".to_string()));
    }

    #[test]
    fn find_missing_required_fields_returns_empty_when_all_filled() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "https://example.com", "token": "abc123"});

        let missing = find_missing_required_fields(&spec, &answers);

        assert!(missing.is_empty());
    }

    #[test]
    fn find_missing_required_fields_ignores_optional() {
        let spec = test_form_spec();
        let answers = json!({"api_url": "https://example.com", "token": "abc"});

        let missing = find_missing_required_fields(&spec, &answers);

        assert!(missing.is_empty());
        assert!(!missing.contains(&"optional".to_string()));
    }

    #[test]
    fn find_missing_required_fields_respects_visibility() {
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

        // trigger=false → dependent is invisible → should NOT be in missing list
        let answers = json!({"trigger": false});
        let missing = find_missing_required_fields(&spec, &answers);
        assert!(missing.is_empty());

        // trigger=true → dependent is visible → should BE in missing list
        let answers = json!({"trigger": true});
        let missing = find_missing_required_fields(&spec, &answers);
        assert_eq!(missing.len(), 1);
        assert!(missing.contains(&"dependent".to_string()));
    }
}
