//! Conversion from legacy `setup.yaml` (`SetupSpec`) into `qa_spec::FormSpec`.

use qa_spec::spec::{FormPresentation, ProgressPolicy};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};

use crate::setup_input::{SetupQuestion, SetupSpec, SetupVisibleIf};
use crate::setup_to_formspec::inference::{
    capitalize, extract_default_from_help, infer_default_for_id, infer_question_properties,
    strip_domain_prefix,
};

/// Convert a legacy `SetupSpec` (from `assets/setup.yaml`) into a `FormSpec`.
pub fn setup_spec_to_form_spec(spec: &SetupSpec, provider_id: &str) -> FormSpec {
    let display_name = strip_domain_prefix(provider_id);
    let display_name = capitalize(&display_name);

    let title = spec
        .title
        .clone()
        .unwrap_or_else(|| format!("{display_name} setup"));

    let questions: Vec<QuestionSpec> = spec
        .questions
        .iter()
        .map(|q| convert_setup_question(q, provider_id))
        .collect();

    FormSpec {
        id: format!("{provider_id}-setup"),
        title,
        version: "1.0.0".to_string(),
        description: spec.description.clone(),
        presentation: Some(FormPresentation {
            intro: None,
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

/// Convert a single setup question to a FormSpec question.
fn convert_setup_question(q: &SetupQuestion, provider_id: &str) -> QuestionSpec {
    let kind = match q.kind.as_str() {
        "boolean" => QuestionType::Boolean,
        "number" => QuestionType::Number,
        "choice" | "enum" => QuestionType::Enum,
        _ => QuestionType::String,
    };

    let (inferred_kind, inferred_secret, inferred_constraint) = infer_question_properties(&q.name);

    // Explicit kind from setup.yaml takes priority unless it's the default "string".
    let final_kind = if q.kind == "string" {
        inferred_kind
    } else {
        kind
    };
    let secret = q.secret || inferred_secret;
    let constraint = if final_kind == QuestionType::String {
        inferred_constraint
    } else {
        None
    };

    let choices = if q.choices.is_empty() {
        None
    } else {
        Some(q.choices.clone())
    };

    let default_value = q
        .default
        .as_ref()
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            other => other.to_string(),
        })
        .or_else(|| {
            // Fallback: try to extract default from help text
            q.help.as_ref().and_then(|h| extract_default_from_help(h))
        })
        .or_else(|| {
            // Fallback: try to infer default from well-known question IDs
            infer_default_for_id(&q.name, provider_id)
        });

    let visible_if = q.visible_if.as_ref().and_then(|v| match v {
        SetupVisibleIf::Struct { field, eq } => {
            if let Some(eq_val) = eq {
                Some(qa_spec::Expr::Eq {
                    left: Box::new(qa_spec::Expr::Answer {
                        path: field.clone(),
                    }),
                    right: Box::new(qa_spec::Expr::Literal {
                        value: serde_json::Value::String(eq_val.clone()),
                    }),
                })
            } else {
                Some(qa_spec::Expr::Answer {
                    path: field.clone(),
                })
            }
        }
        SetupVisibleIf::Expr(_expr) => {
            // String expressions are not currently converted to FormSpec Expr.
            // Return None to skip visibility handling for these.
            None
        }
    });

    QuestionSpec {
        id: q.name.clone(),
        kind: final_kind,
        title: q.title.clone().unwrap_or_else(|| q.name.clone()),
        title_i18n: None,
        description: q.help.clone(),
        description_i18n: None,
        required: q.required,
        choices,
        default_value,
        secret,
        visible_if,
        constraint,
        list: None,
        computed: None,
        policy: Default::default(),
        computed_overridable: false,
    }
}
