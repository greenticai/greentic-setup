//! Shared Questions Support for multi-provider setup.
//!
//! When setting up multiple providers, some questions (like `public_base_url`)
//! appear in all providers. Instead of asking the same question repeatedly,
//! we identify shared questions and prompt for them once upfront.

use anyhow::Result;
use qa_spec::{FormSpec, QuestionSpec};
use serde_json::{Map as JsonMap, Value};
use std::collections::HashMap;

use crate::qa::prompts::ask_form_spec_question;
use crate::setup_to_formspec;

/// Well-known question IDs that are commonly shared across providers.
///
/// These questions will be prompted once at the beginning of a multi-provider
/// setup wizard, and their answers will be applied to all providers.
pub const SHARED_QUESTION_IDS: &[&str] = &[
    "public_base_url",
    // NOTE: api_base_url is NOT shared - each provider has different API endpoints
    // (e.g., slack.com, telegram.org, webexapis.com)
];

/// Information about a provider and its FormSpec for multi-provider setup.
#[derive(Clone)]
pub struct ProviderFormSpec {
    /// Provider identifier (e.g., "messaging-telegram")
    pub provider_id: String,
    /// The FormSpec for this provider
    pub form_spec: FormSpec,
}

/// Result of collecting shared questions across multiple providers.
#[derive(Clone, Default)]
pub struct SharedQuestionsResult {
    /// Questions that appear in multiple providers (deduplicated).
    /// Each question is taken from the first provider that defines it.
    pub shared_questions: Vec<QuestionSpec>,
    /// Provider IDs that contain each shared question ID.
    pub question_providers: HashMap<String, Vec<String>>,
}

/// Collect questions that are shared across multiple providers.
///
/// A question is considered "shared" if:
/// 1. Its ID is in `SHARED_QUESTION_IDS`, OR
/// 2. It appears in 2+ providers with the same ID
///
/// Returns deduplicated questions (taking the first occurrence) along with
/// which providers contain each question.
pub fn collect_shared_questions(providers: &[ProviderFormSpec]) -> SharedQuestionsResult {
    if providers.len() <= 1 {
        return SharedQuestionsResult::default();
    }

    // Count occurrences of each question ID across providers
    let mut question_count: HashMap<String, Vec<String>> = HashMap::new();
    let mut first_question: HashMap<String, QuestionSpec> = HashMap::new();

    for provider in providers {
        for question in &provider.form_spec.questions {
            if question.id.is_empty() {
                continue;
            }
            question_count
                .entry(question.id.clone())
                .or_default()
                .push(provider.provider_id.clone());

            // Keep the first occurrence of each question
            first_question
                .entry(question.id.clone())
                .or_insert_with(|| question.clone());
        }
    }

    // Find shared questions (must appear in 2+ providers to be truly shared)
    // SHARED_QUESTION_IDS are hints for what questions are commonly shared,
    // but we only share them if they actually appear in multiple providers.
    //
    // IMPORTANT: Exclude secrets and provider-specific fields from sharing.
    // Each provider needs unique values for these fields.
    let mut shared_questions = Vec::new();
    let mut question_providers = HashMap::new();

    // Questions that should NEVER be shared even if they appear in multiple providers
    const NEVER_SHARE_IDS: &[&str] = &[
        "api_base_url",   // Different API endpoints per provider
        "bot_token",      // Provider-specific secrets
        "access_token",   // Provider-specific secrets
        "token",          // Provider-specific secrets
        "app_id",         // Provider-specific IDs
        "app_secret",     // Provider-specific secrets
        "client_id",      // Provider-specific IDs
        "client_secret",  // Provider-specific secrets
        "webhook_secret", // Provider-specific secrets
        "signing_secret", // Provider-specific secrets
    ];

    for (question_id, provider_ids) in &question_count {
        let appears_multiple = provider_ids.len() >= 2;

        // Only share questions that actually appear in 2+ providers
        if appears_multiple && let Some(question) = first_question.get(question_id) {
            // Skip secrets - they should never be shared across providers
            if question.secret {
                continue;
            }

            // Skip provider-specific fields that happen to have the same ID
            if NEVER_SHARE_IDS.contains(&question_id.as_str()) {
                continue;
            }

            shared_questions.push(question.clone());
            question_providers.insert(question_id.clone(), provider_ids.clone());
        }
    }

    // Sort by question ID for deterministic ordering
    shared_questions.sort_by(|a, b| a.id.cmp(&b.id));

    SharedQuestionsResult {
        shared_questions,
        question_providers,
    }
}

/// Prompt for shared questions that apply to multiple providers.
///
/// Takes existing answers from loaded setup file and only prompts for
/// questions that don't already have a valid (non-empty) value.
pub fn prompt_shared_questions(
    shared: &SharedQuestionsResult,
    advanced: bool,
    existing_answers: &Value,
) -> Result<Value> {
    if shared.shared_questions.is_empty() {
        return Ok(Value::Object(JsonMap::new()));
    }

    let existing_map = existing_answers.as_object();

    // Check if all shared questions already have valid answers
    let questions_needing_prompt: Vec<_> = shared
        .shared_questions
        .iter()
        .filter(|q| {
            // Skip optional questions in normal mode
            if !advanced && !q.required {
                return false;
            }
            // Check if this question already has a non-empty value
            if let Some(map) = existing_map
                && let Some(value) = map.get(&q.id)
            {
                // Skip if value is non-null and non-empty string
                if !value.is_null() {
                    if let Some(s) = value.as_str() {
                        return s.is_empty(); // Need prompt if empty string
                    }
                    return false; // Has value, skip
                }
            }
            true // Need prompt
        })
        .collect();

    // If no questions need prompting, return existing answers
    if questions_needing_prompt.is_empty() {
        let mut answers = JsonMap::new();
        if let Some(map) = existing_map {
            for question in &shared.shared_questions {
                if let Some(value) = map.get(&question.id) {
                    answers.insert(question.id.clone(), value.clone());
                }
            }
        }
        return Ok(Value::Object(answers));
    }

    println!("\n── Shared Configuration ──");
    println!("The following settings apply to all providers:\n");

    let mut answers = JsonMap::new();

    // Copy existing values first
    if let Some(map) = existing_map {
        for question in &shared.shared_questions {
            if let Some(value) = map.get(&question.id)
                && !value.is_null()
                && !(value.is_string() && value.as_str() == Some(""))
            {
                answers.insert(question.id.clone(), value.clone());
            }
        }
    }

    for question in &shared.shared_questions {
        // Skip if we already have a valid answer
        if answers.contains_key(&question.id) {
            continue;
        }

        // Skip optional questions in normal mode
        if !advanced && !question.required {
            continue;
        }

        // Show which providers use this question
        if let Some(provider_ids) = shared.question_providers.get(&question.id) {
            let providers_str = provider_ids
                .iter()
                .map(|id| setup_to_formspec::strip_domain_prefix(id))
                .collect::<Vec<_>>()
                .join(", ");
            println!("  Used by: {providers_str}");
        }

        if let Some(value) = ask_form_spec_question(question)? {
            answers.insert(question.id.clone(), value);
        }
    }

    println!();
    Ok(Value::Object(answers))
}

/// Merge shared answers with provider-specific answers.
///
/// Shared answers take precedence for non-empty values, but provider-specific
/// answers can override if the shared value is empty.
pub fn merge_shared_with_provider_answers(
    shared: &Value,
    provider_specific: Option<&Value>,
) -> Value {
    let mut merged = JsonMap::new();

    // Start with shared answers
    if let Some(shared_map) = shared.as_object() {
        for (key, value) in shared_map {
            // Only include non-empty values
            if !(value.is_null() || value.is_string() && value.as_str() == Some("")) {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    // Add provider-specific answers (don't override non-empty shared values)
    if let Some(provider_map) = provider_specific.and_then(Value::as_object) {
        for (key, value) in provider_map {
            // Only add if not already present with a non-empty value
            if !merged.contains_key(key) {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    Value::Object(merged)
}

/// Build FormSpecs for multiple providers from their pack paths.
///
/// Convenience function to prepare input for `collect_shared_questions`.
pub fn build_provider_form_specs(
    providers: &[(std::path::PathBuf, String)], // (pack_path, provider_id)
) -> Vec<ProviderFormSpec> {
    providers
        .iter()
        .filter_map(|(pack_path, provider_id)| {
            setup_to_formspec::pack_to_form_spec(pack_path, provider_id).map(|form_spec| {
                ProviderFormSpec {
                    provider_id: provider_id.clone(),
                    form_spec,
                }
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qa_spec::QuestionType;

    fn make_provider_form_spec(provider_id: &str, question_ids: &[&str]) -> ProviderFormSpec {
        let questions = question_ids
            .iter()
            .map(|id| QuestionSpec {
                id: id.to_string(),
                kind: QuestionType::String,
                title: format!("{} Question", id),
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
            })
            .collect();

        ProviderFormSpec {
            provider_id: provider_id.to_string(),
            form_spec: FormSpec {
                id: format!("{}-setup", provider_id),
                title: format!("{} Setup", provider_id),
                version: "1.0.0".into(),
                description: None,
                presentation: None,
                progress_policy: None,
                secrets_policy: None,
                store: vec![],
                validations: vec![],
                includes: vec![],
                questions,
            },
        }
    }

    #[test]
    fn collect_shared_questions_finds_common_questions() {
        let providers = vec![
            make_provider_form_spec("messaging-telegram", &["public_base_url", "bot_token"]),
            make_provider_form_spec("messaging-slack", &["public_base_url", "slack_token"]),
            make_provider_form_spec("messaging-teams", &["public_base_url", "teams_app_id"]),
        ];

        let result = collect_shared_questions(&providers);

        // public_base_url appears in all 3 providers
        assert_eq!(result.shared_questions.len(), 1);
        assert_eq!(result.shared_questions[0].id, "public_base_url");

        // Check provider mapping
        let providers_for_url = result.question_providers.get("public_base_url").unwrap();
        assert_eq!(providers_for_url.len(), 3);
        assert!(providers_for_url.contains(&"messaging-telegram".to_string()));
        assert!(providers_for_url.contains(&"messaging-slack".to_string()));
        assert!(providers_for_url.contains(&"messaging-teams".to_string()));
    }

    #[test]
    fn collect_shared_questions_excludes_single_provider_questions() {
        let providers = vec![
            make_provider_form_spec("messaging-telegram", &["public_base_url", "bot_token"]),
            make_provider_form_spec("messaging-slack", &["slack_token"]), // no public_base_url
        ];

        let result = collect_shared_questions(&providers);
        assert!(result.shared_questions.is_empty());
    }

    #[test]
    fn collect_shared_questions_returns_empty_for_single_provider() {
        let providers = vec![make_provider_form_spec(
            "messaging-telegram",
            &["public_base_url", "bot_token"],
        )];

        let result = collect_shared_questions(&providers);
        assert!(result.shared_questions.is_empty());
    }

    #[test]
    fn collect_shared_questions_finds_non_wellknown_duplicates() {
        let providers = vec![
            make_provider_form_spec("provider-a", &["custom_field", "field_a"]),
            make_provider_form_spec("provider-b", &["custom_field", "field_b"]),
        ];

        let result = collect_shared_questions(&providers);
        assert_eq!(result.shared_questions.len(), 1);
        assert_eq!(result.shared_questions[0].id, "custom_field");
    }

    #[test]
    fn collect_shared_questions_deduplicates() {
        let providers = vec![
            make_provider_form_spec("provider-a", &["public_base_url"]),
            make_provider_form_spec("provider-b", &["public_base_url"]),
            make_provider_form_spec("provider-c", &["public_base_url"]),
        ];

        let result = collect_shared_questions(&providers);
        assert_eq!(result.shared_questions.len(), 1);
    }
}
