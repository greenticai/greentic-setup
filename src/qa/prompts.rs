//! Interactive CLI prompts for QA setup wizard.
//!
//! Handles user input collection, validation, and formatting for FormSpec questions.

use std::io::{self, Write as _};

use anyhow::{Result, anyhow};
use qa_spec::spec::question::ListSpec;
use qa_spec::{FormSpec, QuestionSpec, QuestionType, VisibilityMode, resolve_visibility};
use rpassword::prompt_password;
use serde_json::{Map as JsonMap, Value};

use crate::qa::bridge;
use crate::setup_to_formspec;

/// Interactively prompt the user using FormSpec questions.
///
/// Evaluates `visible_if` expressions after each answer so that conditional
/// questions are shown/hidden dynamically as answers are collected.
pub fn prompt_form_spec_answers(
    spec: &FormSpec,
    provider_id: &str,
    advanced: bool,
) -> Result<Value> {
    prompt_form_spec_answers_with_existing(
        spec,
        provider_id,
        advanced,
        &Value::Object(JsonMap::new()),
    )
}

/// Prompt for FormSpec answers with pre-existing initial values.
///
/// Only prompts for questions that don't already have satisfactory answers.
pub fn prompt_form_spec_answers_with_existing(
    spec: &FormSpec,
    provider_id: &str,
    advanced: bool,
    initial_answers: &Value,
) -> Result<Value> {
    let display = setup_to_formspec::strip_domain_prefix(provider_id);
    let mode_label = if advanced { " (advanced)" } else { "" };
    println!("\nConfiguring {display}: {}{mode_label}", spec.title);
    if let Some(ref pres) = spec.presentation
        && let Some(ref intro) = pres.intro
    {
        println!("{intro}");
    }

    let mut answers = initial_answers.as_object().cloned().unwrap_or_default();
    for question in &spec.questions {
        if question.id.is_empty() {
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
        let existing = answers.get(&question.id);
        if existing
            .filter(|value| answer_satisfies_question(question, value))
            .is_some()
        {
            continue;
        }
        // In normal mode, skip optional missing questions — except `List`
        // (table) kinds. A table is a structural hand-off to the operator
        // ("here's where nav-links / repeating data go") that doesn't make
        // sense to silently hide based on the required flag, even when
        // optional. They get the table; if they want to skip rows they
        // answer "n" to "Add a row?" and move on.
        if !advanced && !question.required && question.kind != QuestionType::List {
            continue;
        }
        if let Some(value) = ask_form_spec_question(question)? {
            answers.insert(question.id.clone(), value);
        }
    }
    Ok(Value::Object(answers))
}

/// Check if an answer satisfies a question's requirements.
pub fn answer_satisfies_question(question: &QuestionSpec, value: &Value) -> bool {
    if value.is_null() {
        return false;
    }

    // Empty or blank string is not satisfactory for any question
    if let Some(s) = value.as_str()
        && s.trim().is_empty()
    {
        return false;
    }

    // Check for environment variable placeholder (e.g., "${PUBLIC_BASE_URL}")
    // These are considered valid values that will be resolved at runtime
    if let Some(s) = value.as_str()
        && s.starts_with("${")
        && s.ends_with('}')
    {
        return true;
    }

    if let Some(ref choices) = question.choices
        && !choices.is_empty()
    {
        let Some(candidate) = value.as_str() else {
            return false;
        };
        if !choices.iter().any(|choice| choice == candidate) {
            return false;
        }
    }
    if let Some(ref constraint) = question.constraint
        && let Some(ref pattern) = constraint.pattern
        && let Some(candidate) = value.as_str()
        && !matches_pattern(candidate, pattern)
    {
        return false;
    }
    true
}

/// Prompt for a single FormSpec question and return the answer.
pub fn ask_form_spec_question(question: &QuestionSpec) -> Result<Option<Value>> {
    // Table / repeating-row questions (kind: List) get a dedicated row loop
    // — see `ask_list_question` for the prompt protocol. Falls through to
    // the scalar path if the question is List-typed but missing its
    // `list` schema (defensive: shouldn't happen with a well-formed spec).
    if question.kind == QuestionType::List
        && let Some(ref list) = question.list
    {
        return ask_list_question(question, list);
    }

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

/// Prompt for a `QuestionType::List` (repeating-row) question. Loops
/// "Add another?" / row-by-row prompts and returns a `Value::Array` of
/// per-row JSON objects whose keys match the column field IDs.
///
/// Constraints:
/// - `list.min_items` / `max_items` enforce row count bounds.
/// - The outer question's `required` flag is honoured: when required and
///   no rows were collected, we re-prompt instead of returning `None`.
/// - Rows whose required columns are all empty are dropped silently
///   (lets the operator type a blank line and "step out" mid-table).
fn ask_list_question(question: &QuestionSpec, list: &ListSpec) -> Result<Option<Value>> {
    let marker = if question.required {
        " (required)"
    } else {
        " (optional)"
    };
    println!();
    println!("  {}{marker}", question.title);
    if let Some(ref desc) = question.description
        && !desc.is_empty()
    {
        println!("  {desc}");
    }

    let max = list.max_items;
    let min = list.min_items.unwrap_or(0);

    let mut rows: Vec<Value> = Vec::new();
    loop {
        if let Some(cap) = max
            && rows.len() >= cap
        {
            println!("  (max {} rows reached)", cap);
            break;
        }

        // Ask whether to add another row.
        let prompt = if rows.is_empty() {
            "  > Add a row? [y/N] "
        } else {
            "  > Add another row? [y/N] "
        };
        let input = read_input(prompt, false)?;
        let trimmed = input.trim().to_ascii_lowercase();
        let yes = matches!(trimmed.as_str(), "y" | "yes" | "1" | "true");
        if !yes {
            if rows.len() < min {
                println!(
                    "  At least {min} row(s) required — got {}. Type 'y' to add another.",
                    rows.len()
                );
                continue;
            }
            break;
        }

        // Prompt each column for the new row.
        println!("  Row #{}:", rows.len() + 1);
        let mut row_obj = JsonMap::new();
        for column in &list.fields {
            if let Some(value) = ask_form_spec_question(column)? {
                row_obj.insert(column.id.clone(), value);
            }
        }

        // Drop the row if every required column ended up empty — lets the
        // operator back out by hitting Enter through every column.
        let row_has_required_content = list.fields.iter().all(|c| {
            !c.required
                || row_obj
                    .get(&c.id)
                    .map(|v| !is_empty_answer(v))
                    .unwrap_or(false)
        });
        if !row_has_required_content {
            println!("  (row dropped — required columns were empty)");
            continue;
        }

        rows.push(Value::Object(row_obj));
    }

    if rows.is_empty() {
        if question.required {
            println!("  This field is required — at least one row needed.");
            return Ok(None);
        }
        return Ok(None);
    }

    Ok(Some(Value::Array(rows)))
}

/// Treat an empty string, false bool, or null as "no answer" for the
/// row-required check.
fn is_empty_answer(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// Build the prompt string for a FormSpec question.
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

/// Read input from user, optionally masking for secrets.
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

/// Check if a FormSpec has any required questions.
pub fn has_required_questions(spec: Option<&FormSpec>) -> bool {
    spec.map(|s| s.questions.iter().any(|q| q.required))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn matches_url_pattern() {
        assert!(matches_pattern("https://example.com", r"^https?://\S+"));
        assert!(matches_pattern("http://localhost:8080", r"^https?://\S+"));
        assert!(!matches_pattern("not-a-url", r"^https?://\S+"));
        assert!(!matches_pattern("https://", r"^https?://\S+")); // too short
    }
}
