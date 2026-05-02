//! Conversion from legacy `setup.yaml` (`SetupSpec`) into `qa_spec::FormSpec`.

use qa_spec::spec::question::ListSpec;
use qa_spec::spec::{FormPresentation, ProgressPolicy};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};

use crate::setup_input::{SetupQuestion, SetupSpec, SetupTableColumn, SetupVisibleIf};
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
    // Table kind handles repeating-row data (e.g. nav_links) and bridges to
    // qa-spec's existing `QuestionType::List` + `ListSpec.fields`. Each
    // column becomes a nested QuestionSpec the wizard prompts per row.
    if q.kind == "table" {
        return convert_table_question(q, provider_id);
    }

    let kind = match q.kind.as_str() {
        "boolean" => QuestionType::Boolean,
        "number" => QuestionType::Number,
        "choice" | "enum" => QuestionType::Enum,
        _ => QuestionType::String,
    };

    let (inferred_kind, inferred_secret, inferred_constraint) = infer_question_properties(&q.name);

    // Explicit kind from setup.yaml takes priority unless it's the default "string".
    let mut final_kind = if q.kind == "string" {
        inferred_kind
    } else {
        kind
    };

    // Choices come from explicit q.choices first; fall back to parsing the
    // placeholder when it uses the `a | b | c` convention. Lets a question
    // declared as `kind: string` with `placeholder: "default | 3aigent"`
    // render as a select dropdown without maintaining a separate `choices:`
    // array. Detection requires `|` flanked by spaces so genuine `|`-bearing
    // placeholders (e.g. regex hints) aren't mistaken for an enum.
    let mut choices_opt = if q.choices.is_empty() {
        None
    } else {
        Some(q.choices.clone())
    };
    if choices_opt.is_none()
        && final_kind == QuestionType::String
        && let Some(parsed) = q.placeholder.as_deref().and_then(parse_placeholder_choices)
    {
        choices_opt = Some(parsed);
        final_kind = QuestionType::Enum;
    }
    let choices = choices_opt;

    let secret = q.secret || inferred_secret;
    let constraint = if final_kind == QuestionType::String {
        inferred_constraint
    } else {
        None
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

/// Build the QuestionSpec for a `kind: table` question. The runtime kind is
/// `QuestionType::List`; per-row schema lives in `list.fields`.
fn convert_table_question(q: &SetupQuestion, _provider_id: &str) -> QuestionSpec {
    let fields: Vec<QuestionSpec> = q.columns.iter().map(convert_table_column).collect();

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
        SetupVisibleIf::Expr(_) => None,
    });

    QuestionSpec {
        id: q.name.clone(),
        kind: QuestionType::List,
        title: q.title.clone().unwrap_or_else(|| q.name.clone()),
        title_i18n: None,
        description: q.help.clone(),
        description_i18n: None,
        required: q.required,
        choices: None,
        default_value: None,
        secret: false,
        visible_if,
        constraint: None,
        list: Some(ListSpec {
            min_items: q.min_rows.map(usize::from),
            max_items: q.max_rows.map(usize::from),
            fields,
        }),
        computed: None,
        policy: Default::default(),
        computed_overridable: false,
    }
}

/// Build a per-row QuestionSpec for a single table column. Nested tables are
/// not supported — `kind: table` on a column collapses to `string`.
fn convert_table_column(col: &SetupTableColumn) -> QuestionSpec {
    let kind = match col.kind.as_str() {
        "boolean" => QuestionType::Boolean,
        "number" => QuestionType::Number,
        "choice" | "enum" => QuestionType::Enum,
        _ => QuestionType::String,
    };
    let choices = if col.choices.is_empty() {
        None
    } else {
        Some(col.choices.clone())
    };
    let default_value = col.default.as_ref().map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    });
    QuestionSpec {
        id: col.key.clone(),
        kind,
        title: col.title.clone().unwrap_or_else(|| col.key.clone()),
        title_i18n: None,
        description: col.help.clone(),
        description_i18n: None,
        required: col.required,
        choices,
        default_value,
        secret: false,
        visible_if: None,
        constraint: None,
        list: None,
        computed: None,
        policy: Default::default(),
        computed_overridable: false,
    }
}

/// Parse a placeholder string of the form `"a | b | c"` into a list of choices.
///
/// Returns `Some(choices)` only when the placeholder is *clearly* enumerating
/// alternatives — i.e. the `|` is flanked by spaces and there are at least two
/// non-empty trimmed segments. This avoids false-positives on placeholders that
/// happen to contain `|` for other reasons (e.g. regex hints like `(yes|no)`).
fn parse_placeholder_choices(placeholder: &str) -> Option<Vec<String>> {
    if !placeholder.contains(" | ") {
        return None;
    }
    let parts: Vec<String> = placeholder
        .split('|')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() < 2 {
        return None;
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::convert_setup_question;
    use super::parse_placeholder_choices;
    use crate::setup_input::{SetupQuestion, SetupTableColumn};
    use qa_spec::QuestionType;

    #[test]
    fn table_kind_bridges_to_list_with_per_column_fields() {
        let q = SetupQuestion {
            name: "nav_links".into(),
            kind: "table".into(),
            title: Some("Top-menu nav links".into()),
            help: Some("Click + Add to add a row".into()),
            required: false,
            min_rows: Some(0),
            max_rows: Some(8),
            columns: vec![
                SetupTableColumn {
                    key: "label".into(),
                    title: Some("Label".into()),
                    kind: "string".into(),
                    required: true,
                    placeholder: Some("Help".into()),
                    ..Default::default()
                },
                SetupTableColumn {
                    key: "url".into(),
                    title: Some("URL".into()),
                    kind: "string".into(),
                    required: true,
                    placeholder: Some("/help".into()),
                    ..Default::default()
                },
                SetupTableColumn {
                    key: "external".into(),
                    title: Some("Open in new tab".into()),
                    kind: "boolean".into(),
                    required: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let spec = convert_setup_question(&q, "messaging-webchat-gui");
        assert_eq!(spec.id, "nav_links");
        assert_eq!(spec.kind, QuestionType::List);
        assert_eq!(spec.title, "Top-menu nav links");
        let list = spec.list.expect("list spec must be set for table kind");
        assert_eq!(list.min_items, Some(0));
        assert_eq!(list.max_items, Some(8));
        assert_eq!(list.fields.len(), 3);

        // First column: required string
        assert_eq!(list.fields[0].id, "label");
        assert_eq!(list.fields[0].kind, QuestionType::String);
        assert!(list.fields[0].required);

        // Third column: boolean
        assert_eq!(list.fields[2].id, "external");
        assert_eq!(list.fields[2].kind, QuestionType::Boolean);
        assert!(!list.fields[2].required);
    }

    #[test]
    fn non_table_kind_keeps_existing_string_path() {
        let q = SetupQuestion {
            name: "skin".into(),
            kind: "string".into(),
            title: Some("Skin".into()),
            ..Default::default()
        };
        let spec = convert_setup_question(&q, "messaging-webchat-gui");
        assert_eq!(spec.kind, QuestionType::String);
        assert!(spec.list.is_none());
    }

    #[test]
    fn parses_two_choices_with_spaces() {
        assert_eq!(
            parse_placeholder_choices("default | 3aigent"),
            Some(vec!["default".into(), "3aigent".into()])
        );
    }

    #[test]
    fn parses_three_choices() {
        assert_eq!(
            parse_placeholder_choices("debug | info | warn"),
            Some(vec!["debug".into(), "info".into(), "warn".into()])
        );
    }

    #[test]
    fn rejects_placeholder_without_spaces_around_pipe() {
        // Regex-style alternatives shouldn't be misinterpreted as choices.
        assert_eq!(parse_placeholder_choices("(yes|no)"), None);
        assert_eq!(parse_placeholder_choices("a|b|c"), None);
    }

    #[test]
    fn rejects_single_segment() {
        assert_eq!(parse_placeholder_choices("just a hint"), None);
    }

    #[test]
    fn rejects_empty_segments() {
        // " | " alone (no real values) becomes zero non-empty parts.
        assert_eq!(parse_placeholder_choices(" | "), None);
    }

    #[test]
    fn trims_whitespace_around_segments() {
        assert_eq!(
            parse_placeholder_choices("  default   |   3aigent  "),
            Some(vec!["default".into(), "3aigent".into()])
        );
    }
}
