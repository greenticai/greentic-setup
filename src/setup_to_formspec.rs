//! Converts legacy `setup.yaml` (`SetupSpec`) into `qa_spec::FormSpec`.
//!
//! This allows setup logic to drive provider configuration through a single
//! FormSpec-based wizard regardless of whether the provider ships a WASM
//! `qa-spec` op or a static `setup.yaml` file.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use qa_spec::spec::{Constraint, FormPresentation, ProgressPolicy};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
use zip::ZipArchive;

use crate::qa::bridge::provider_qa_to_form_spec;
use crate::setup_input::{SetupQuestion, SetupSpec, load_setup_spec};

/// Convert a legacy `SetupSpec` (from `assets/setup.yaml`) into a `FormSpec`.
pub fn setup_spec_to_form_spec(spec: &SetupSpec, provider_id: &str) -> FormSpec {
    let display_name = strip_domain_prefix(provider_id);
    let display_name = capitalize(&display_name);

    let title = spec
        .title
        .clone()
        .unwrap_or_else(|| format!("{display_name} setup"));

    let questions: Vec<QuestionSpec> = spec.questions.iter().map(convert_setup_question).collect();

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

fn convert_setup_question(q: &SetupQuestion) -> QuestionSpec {
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

    let default_value = q.default.as_ref().map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    });

    let visible_if = q.visible_if.as_ref().map(|v| {
        if let Some(ref eq_val) = v.eq {
            qa_spec::Expr::Eq {
                left: Box::new(qa_spec::Expr::Answer {
                    path: v.field.clone(),
                }),
                right: Box::new(qa_spec::Expr::Literal {
                    value: serde_json::Value::String(eq_val.clone()),
                }),
            }
        } else {
            qa_spec::Expr::Answer {
                path: v.field.clone(),
            }
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

/// Load a `FormSpec` from a pack's `setup.yaml`, if present.
///
/// Falls back to reading `qa/*.json` files from the pack when `setup.yaml`
/// is missing or has no questions.
pub fn pack_to_form_spec(pack_path: &Path, provider_id: &str) -> Option<FormSpec> {
    // Try legacy setup.yaml first
    if let Ok(Some(spec)) = load_setup_spec(pack_path)
        && !spec.questions.is_empty()
    {
        return Some(setup_spec_to_form_spec(&spec, provider_id));
    }

    // Fallback: try qa/*.json from inside the pack
    if let Some(form) = load_qa_form_spec_from_pack(pack_path, provider_id)
        && !form.questions.is_empty()
    {
        return Some(form);
    }

    None
}

/// Read `qa/*.json` files from inside a `.gtpack` ZIP archive and convert
/// the first valid QA spec into a `FormSpec` via the bridge.
fn load_qa_form_spec_from_pack(pack_path: &Path, provider_id: &str) -> Option<FormSpec> {
    let file = File::open(pack_path).ok()?;
    let mut archive = ZipArchive::new(file).ok()?;

    // Collect qa/*.json entry names
    let qa_entries: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let entry = archive.by_index(i).ok()?;
            let name = entry.name().to_string();
            if name.starts_with("qa/") && name.ends_with(".json") {
                Some(name)
            } else {
                None
            }
        })
        .collect();

    for entry_name in qa_entries {
        if let Ok(mut entry) = archive.by_name(&entry_name) {
            let mut contents = String::new();
            if entry.read_to_string(&mut contents).is_ok()
                && let Ok(qa_value) = serde_json::from_str::<serde_json::Value>(&contents)
            {
                let i18n = HashMap::new();
                let form = provider_qa_to_form_spec(&qa_value, &i18n, provider_id);
                if !form.questions.is_empty() {
                    return Some(form);
                }
            }
        }
    }

    None
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_setup_spec() -> SetupSpec {
        SetupSpec {
            title: Some("Telegram Setup".to_string()),
            description: None,
            questions: vec![
                SetupQuestion {
                    name: "enabled".to_string(),
                    kind: "boolean".to_string(),
                    required: true,
                    help: Some("Enable this provider".to_string()),
                    choices: vec![],
                    default: Some(json!(true)),
                    secret: false,
                    title: Some("Enable provider".to_string()),
                    visible_if: None,
                },
                SetupQuestion {
                    name: "public_base_url".to_string(),
                    kind: "string".to_string(),
                    required: true,
                    help: Some("Public URL for webhook callbacks".to_string()),
                    choices: vec![],
                    default: None,
                    secret: false,
                    title: None,
                    visible_if: None,
                },
                SetupQuestion {
                    name: "bot_token".to_string(),
                    kind: "string".to_string(),
                    required: true,
                    help: Some("Telegram bot token".to_string()),
                    choices: vec![],
                    default: None,
                    secret: true,
                    title: Some("Bot Token".to_string()),
                    visible_if: None,
                },
                SetupQuestion {
                    name: "log_level".to_string(),
                    kind: "choice".to_string(),
                    required: false,
                    help: None,
                    choices: vec!["debug".into(), "info".into(), "warn".into()],
                    default: Some(json!("info")),
                    secret: false,
                    title: Some("Log Level".to_string()),
                    visible_if: None,
                },
            ],
        }
    }

    #[test]
    fn converts_setup_spec_to_form_spec() {
        let form = setup_spec_to_form_spec(&sample_setup_spec(), "messaging-telegram");
        assert_eq!(form.id, "messaging-telegram-setup");
        assert_eq!(form.title, "Telegram Setup");
        assert_eq!(form.questions.len(), 4);
    }

    #[test]
    fn maps_question_types_correctly() {
        let form = setup_spec_to_form_spec(&sample_setup_spec(), "messaging-telegram");
        assert_eq!(form.questions[0].kind, QuestionType::Boolean);
        assert_eq!(form.questions[1].kind, QuestionType::String);
        assert_eq!(form.questions[3].kind, QuestionType::Enum);
    }

    #[test]
    fn detects_url_constraint() {
        let form = setup_spec_to_form_spec(&sample_setup_spec(), "messaging-telegram");
        let url_q = &form.questions[1];
        assert!(url_q.constraint.is_some());
        assert!(
            url_q
                .constraint
                .as_ref()
                .unwrap()
                .pattern
                .as_ref()
                .unwrap()
                .contains("https?")
        );
    }

    #[test]
    fn detects_secret_fields() {
        let form = setup_spec_to_form_spec(&sample_setup_spec(), "messaging-telegram");
        assert!(form.questions[2].secret);
        assert!(!form.questions[0].secret);
    }

    #[test]
    fn preserves_choices_and_defaults() {
        let form = setup_spec_to_form_spec(&sample_setup_spec(), "messaging-telegram");
        let log_q = &form.questions[3];
        assert_eq!(log_q.choices.as_ref().unwrap(), &["debug", "info", "warn"]);
        assert_eq!(log_q.default_value.as_deref(), Some("info"));
    }

    #[test]
    fn handles_empty_spec() {
        let spec = SetupSpec {
            title: None,
            description: None,
            questions: vec![],
        };
        let form = setup_spec_to_form_spec(&spec, "messaging-dummy");
        assert_eq!(form.id, "messaging-dummy-setup");
        assert_eq!(form.title, "Dummy setup");
        assert!(form.questions.is_empty());
    }

    #[test]
    fn pack_to_form_spec_falls_back_to_qa_json() {
        use std::io::Write;
        use zip::write::{FileOptions, ZipWriter};

        let qa_json = serde_json::json!({
            "mode": "setup",
            "title": {"key": "state-redis.qa.setup.title"},
            "questions": [
                {"id": "redis_url", "label": "Redis URL", "required": true},
                {
                    "id": "redis_password",
                    "label": "Redis password",
                    "required": false,
                    "visible_if": {"field": "redis_auth_enabled", "eq": "true"}
                }
            ]
        });

        // Create a gtpack with empty setup.yaml but valid qa/*.json
        let temp_dir = tempfile::tempdir().unwrap();
        let pack_path = temp_dir.path().join("state-redis.gtpack");
        let file = std::fs::File::create(&pack_path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // Empty setup.yaml (no questions)
        writer.start_file("assets/setup.yaml", options).unwrap();
        writer
            .write_all(b"title: State Redis\nquestions: []\n")
            .unwrap();

        // QA JSON with real questions
        writer
            .start_file("qa/state-redis-setup.json", options)
            .unwrap();
        writer
            .write_all(serde_json::to_string(&qa_json).unwrap().as_bytes())
            .unwrap();
        writer.finish().unwrap();

        let form = pack_to_form_spec(&pack_path, "state-redis").expect("should find QA JSON");
        assert_eq!(form.questions.len(), 2);
        assert_eq!(form.questions[0].id, "redis_url");
        assert!(form.questions[1].visible_if.is_some());
    }

    #[test]
    fn pack_to_form_spec_prefers_setup_yaml_with_questions() {
        use std::io::Write;
        use zip::write::{FileOptions, ZipWriter};

        // Create a gtpack with both setup.yaml questions and qa/*.json
        let temp_dir = tempfile::tempdir().unwrap();
        let pack_path = temp_dir.path().join("messaging-test.gtpack");
        let file = std::fs::File::create(&pack_path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);

        // setup.yaml with questions
        writer.start_file("assets/setup.yaml", options).unwrap();
        writer
            .write_all(
                b"title: Test\nquestions:\n  - name: enabled\n    kind: boolean\n    required: true\n",
            )
            .unwrap();

        // QA JSON with different questions
        writer.start_file("qa/test-setup.json", options).unwrap();
        writer
            .write_all(
                br#"{"mode":"setup","title":{"key":"t"},"questions":[{"id":"other","label":"Other"}]}"#,
            )
            .unwrap();
        writer.finish().unwrap();

        let form = pack_to_form_spec(&pack_path, "messaging-test").expect("should find setup.yaml");
        // Should use setup.yaml, not qa JSON
        assert_eq!(form.questions.len(), 1);
        assert_eq!(form.questions[0].id, "enabled");
    }
}
