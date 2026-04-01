//! Converts legacy `setup.yaml` (`SetupSpec`) into `qa_spec::FormSpec`.
//!
//! This allows setup logic to drive provider configuration through a single
//! FormSpec-based wizard regardless of whether the provider ships a WASM
//! `qa-spec` op or a static `setup.yaml` file.

mod convert;
mod inference;
mod pack;

// Re-export public API
pub use convert::setup_spec_to_form_spec;
pub use inference::{
    capitalize, extract_default_from_help, infer_default_for_id, infer_question_properties,
    strip_domain_prefix,
};
pub use pack::pack_to_form_spec;

#[cfg(test)]
mod tests {
    use qa_spec::QuestionType;
    use serde_json::json;

    use super::*;
    use crate::setup_input::{SetupQuestion, SetupSpec};

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
                    ..Default::default()
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
                    ..Default::default()
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
                    ..Default::default()
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
                    ..Default::default()
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

    #[test]
    fn extract_default_from_help_slack_format() {
        // Exact format from Slack's setup.yaml
        let help = "Slack API base URL (default: https://slack.com/api)";
        let result = extract_default_from_help(help);
        assert_eq!(result, Some("https://slack.com/api".to_string()));
    }

    #[test]
    fn extract_default_from_help_various_formats() {
        // Parenthesized
        assert_eq!(
            extract_default_from_help("Some help (default: value)"),
            Some("value".to_string())
        );
        // Bracketed
        assert_eq!(
            extract_default_from_help("Some help [default: value]"),
            Some("value".to_string())
        );
        // Case insensitive
        assert_eq!(
            extract_default_from_help("(Default: VALUE)"),
            Some("VALUE".to_string())
        );
        // With trailing punctuation
        assert_eq!(
            extract_default_from_help("Help (default: value.)"),
            Some("value".to_string())
        );
        // No default
        assert_eq!(extract_default_from_help("Just some help text"), None);
    }

    #[test]
    fn converts_help_default_to_question_default_value() {
        let spec = SetupSpec {
            title: None,
            description: None,
            questions: vec![SetupQuestion {
                name: "api_base_url".to_string(),
                kind: "string".to_string(),
                required: true,
                help: Some("Slack API base URL (default: https://slack.com/api)".to_string()),
                choices: vec![],
                default: None, // No explicit default
                secret: false,
                title: Some("API base URL".to_string()),
                visible_if: None,
                ..Default::default()
            }],
        };

        let form = setup_spec_to_form_spec(&spec, "messaging-slack");
        assert_eq!(form.questions.len(), 1);
        // Should extract default from help text
        assert_eq!(
            form.questions[0].default_value,
            Some("https://slack.com/api".to_string())
        );
    }
}
