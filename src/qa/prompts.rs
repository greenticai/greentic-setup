//! Interactive CLI prompts for QA setup wizard.
//!
//! Handles user input collection, validation, and formatting for FormSpec questions.

use std::borrow::Cow;
use std::io::{self, Write as _};

use anyhow::{Result, anyhow};
use qa_spec::spec::question::ListSpec;
use qa_spec::{FormSpec, QuestionSpec, QuestionType, VisibilityMode, resolve_visibility};
use rpassword::prompt_password;
use serde_json::{Map as JsonMap, Value};

use crate::cli_i18n::{CliI18n, format_template};
use crate::qa::bridge;
use crate::setup_to_formspec;

/// Translate a chrome (prompt-loop scaffolding) string. When an `i18n` handle
/// is supplied the localized catalog value is used, falling back to the inline
/// English `default`; when it is `None` the `default` is returned verbatim, so
/// callers that opt out (the provider-setup flow) keep the English prompt loop
/// byte-for-byte. Question titles/descriptions are localized upstream on the
/// spec, not here — this is only for the loop's own scaffolding text.
fn ct(i18n: Option<&CliI18n>, key: &str, default: &str) -> String {
    match i18n {
        Some(i) => i.t_or(key, default),
        None => default.to_string(),
    }
}

/// [`ct`] with `{}` placeholders substituted from `args`.
fn ctf(i18n: Option<&CliI18n>, key: &str, default: &str, args: &[&str]) -> String {
    match i18n {
        Some(i) => i.tf_or(key, default, args),
        None => format_template(default, args),
    }
}

/// The localized ` (required)` / ` (optional)` suffix appended to a question
/// title in the prompt header.
fn required_marker(question: &QuestionSpec, i18n: Option<&CliI18n>) -> String {
    if question.required {
        ct(i18n, "setup.qa.prompt.required_marker", " (required)")
    } else {
        ct(i18n, "setup.qa.prompt.optional_marker", " (optional)")
    }
}

/// Interactively prompt the user using FormSpec questions.
///
/// Evaluates `visible_if` expressions after each answer so that conditional
/// questions are shown/hidden dynamically as answers are collected.
pub fn prompt_form_spec_answers(
    spec: &FormSpec,
    provider_id: &str,
    advanced: bool,
    i18n: Option<&CliI18n>,
) -> Result<Value> {
    prompt_form_spec_answers_with_existing(
        spec,
        provider_id,
        advanced,
        &Value::Object(JsonMap::new()),
        i18n,
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
    i18n: Option<&CliI18n>,
) -> Result<Value> {
    let display = setup_to_formspec::strip_domain_prefix(provider_id);
    let mode_label = if advanced {
        ct(i18n, "setup.qa.prompt.mode_advanced", " (advanced)")
    } else {
        String::new()
    };
    let head = ctf(
        i18n,
        "setup.qa.prompt.configuring",
        "Configuring {}: {}",
        &[display.as_ref(), spec.title.as_str()],
    );
    println!("\n{head}{mode_label}");
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
        // The public URL question is never surfaced: its answer is always
        // overwritten by the runtime, so we persist a placeholder instead of
        // asking. See `qa::shared_questions::PUBLIC_URL_FILLER`.
        if crate::qa::shared_questions::is_public_url_question(&question.id) {
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
        if let Some(value) = ask_form_spec_question(question, i18n)? {
            answers.insert(question.id.clone(), value);
        }
    }
    crate::qa::shared_questions::fill_public_url_placeholders(&spec.questions, &mut answers);
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
pub fn ask_form_spec_question(
    question: &QuestionSpec,
    i18n: Option<&CliI18n>,
) -> Result<Option<Value>> {
    // Table / repeating-row questions (kind: List) get a dedicated row loop
    // — see `ask_list_question` for the prompt protocol. Falls through to
    // the scalar path if the question is List-typed but missing its
    // `list` schema (defensive: shouldn't happen with a well-formed spec).
    if question.kind == QuestionType::List
        && let Some(ref list) = question.list
    {
        return ask_list_question(question, list, i18n);
    }

    // Print question header
    let marker = required_marker(question, i18n);
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
        let prompt = build_form_spec_prompt(question, i18n);
        let input = read_input(&prompt, question.secret)?;
        let trimmed = input.trim();

        if trimmed.is_empty() {
            if let Some(ref default) = question.default_value {
                return Ok(Some(parse_typed_value(question.kind, default)));
            }
            if question.required {
                println!(
                    "  {}",
                    ct(
                        i18n,
                        "setup.qa.prompt.field_required",
                        "This field is required."
                    )
                );
                continue;
            }
            return Ok(None);
        }

        let normalized = bridge::normalize_answer(trimmed, question.kind);

        if let Some(ref constraint) = question.constraint
            && let Some(ref pattern) = constraint.pattern
            && !matches_pattern(&normalized, pattern)
        {
            println!(
                "  {}",
                ctf(
                    i18n,
                    "setup.qa.prompt.invalid_format",
                    "Invalid format. Expected pattern: {}",
                    &[pattern],
                )
            );
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
                println!(
                    "  {}",
                    ctf(
                        i18n,
                        "setup.qa.prompt.invalid_choice",
                        "Invalid choice. Options: {}",
                        &[&choices.join(", ")],
                    )
                );
                continue;
            }
        }

        return Ok(Some(parse_typed_value(question.kind, &normalized)));
    }
}

/// Resolve a row column's effective question for prompting.
///
/// When a column carries a `computed` expression *and* is
/// `computed_overridable`, evaluate the expression against the row collected
/// so far and surface the result as the column's `default_value` — so the
/// operator sees a derived suggestion (e.g. route tenant defaulting to the
/// bundle id) and accepts it with Enter or types their own. Columns without
/// an overridable computed expression, and columns whose expression yields
/// nothing for the current row, are returned untouched so their own
/// `default_value` (if any) still applies.
fn resolve_row_column_default<'a>(
    column: &'a QuestionSpec,
    row_so_far: &JsonMap<String, Value>,
) -> Cow<'a, QuestionSpec> {
    if !column.computed_overridable {
        return Cow::Borrowed(column);
    }
    let Some(expr) = column.computed.as_ref() else {
        return Cow::Borrowed(column);
    };
    let ctx = qa_spec::build_expression_context(&Value::Object(row_so_far.clone()));
    let derived = match expr.evaluate_value(&ctx) {
        Some(Value::String(text)) if !text.trim().is_empty() => text,
        Some(Value::String(_)) | None => return Cow::Borrowed(column),
        Some(other) => other.to_string(),
    };
    let mut resolved = column.clone();
    resolved.default_value = Some(derived);
    Cow::Owned(resolved)
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
fn ask_list_question(
    question: &QuestionSpec,
    list: &ListSpec,
    i18n: Option<&CliI18n>,
) -> Result<Option<Value>> {
    let marker = required_marker(question, i18n);
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
            println!(
                "  {}",
                ctf(
                    i18n,
                    "setup.qa.list.max_reached",
                    "(max {} rows reached)",
                    &[&cap.to_string()]
                )
            );
            break;
        }

        // Ask whether to add another row. `item_label` lets the spec name
        // the row noun (e.g. "bundle" ⇒ "Add bundle?"); falls back to "row".
        // The `[y/N]` token and the literal `y` reply stay canonical — the
        // reply parser only understands those, not localized words.
        let label = list.item_label.as_deref().unwrap_or("row");
        let prompt = if rows.is_empty() {
            format!(
                "  > {}",
                ctf(i18n, "setup.qa.list.add_first", "Add {}? [y/N] ", &[label])
            )
        } else {
            format!(
                "  > {}",
                ctf(
                    i18n,
                    "setup.qa.list.add_more",
                    "Add another {}? [y/N] ",
                    &[label]
                )
            )
        };
        let input = read_input(&prompt, false)?;
        let trimmed = input.trim().to_ascii_lowercase();
        let yes = matches!(trimmed.as_str(), "y" | "yes" | "1" | "true");
        if !yes {
            if rows.len() < min {
                println!(
                    "  {}",
                    ctf(
                        i18n,
                        "setup.qa.list.min_required",
                        "At least {} row(s) required — got {}. Type 'y' to add another.",
                        &[&min.to_string(), &rows.len().to_string()],
                    )
                );
                continue;
            }
            break;
        }

        // Prompt each column for the new row.
        println!(
            "  {}",
            ctf(
                i18n,
                "setup.qa.list.row_header",
                "Row #{}:",
                &[&(rows.len() + 1).to_string()]
            )
        );
        let mut row_obj = JsonMap::new();
        for column in &list.fields {
            // A column may derive its default from earlier columns in the
            // same row (e.g. route_tenant ⇐ bundle_id). Resolve it against
            // the row collected so far before prompting.
            let column = resolve_row_column_default(column, &row_obj);
            if let Some(value) = ask_form_spec_question(&column, i18n)? {
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
            println!(
                "  {}",
                ct(
                    i18n,
                    "setup.qa.list.row_dropped",
                    "(row dropped — required columns were empty)",
                )
            );
            continue;
        }

        rows.push(Value::Object(row_obj));
    }

    if rows.is_empty() {
        if question.required {
            println!(
                "  {}",
                ct(
                    i18n,
                    "setup.qa.list.field_required_row",
                    "This field is required — at least one row needed.",
                )
            );
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
///
/// The type-hint tokens (`[yes/no]`, `[number]`, `[choice]`) stay canonical:
/// the answer parser only understands those literals, so localizing them would
/// invite input that no longer validates. Only the `(default: …)` prose is
/// localized.
fn build_form_spec_prompt(question: &QuestionSpec, i18n: Option<&CliI18n>) -> String {
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
        prompt.push_str(&ctf(
            i18n,
            "setup.qa.prompt.default",
            "(default: {}) ",
            &[default],
        ));
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

    fn column(value: Value) -> QuestionSpec {
        serde_json::from_value(value).expect("question spec")
    }

    fn row(pairs: &[(&str, &str)]) -> JsonMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect()
    }

    #[test]
    fn row_column_default_derives_overridable_computed_from_siblings() {
        let legal = row(&[("bundle_id", "legal")]);

        // Copy of a sibling answer (route_tenant ⇐ bundle_id).
        let tenant = column(json!({
            "id": "route_tenant", "type": "string", "title": "Route tenant",
            "computed": {"op": "var", "path": "bundle_id"},
            "computed_overridable": true,
        }));
        assert_eq!(
            resolve_row_column_default(&tenant, &legal)
                .default_value
                .as_deref(),
            Some("legal")
        );

        // String concat (route_path_prefixes ⇐ "/" + bundle_id).
        let prefixes = column(json!({
            "id": "route_path_prefixes", "type": "string", "title": "Route path prefixes",
            "computed": {"op": "concat", "parts": [
                {"op": "literal", "value": "/"},
                {"op": "var", "path": "bundle_id"}
            ]},
            "computed_overridable": true,
        }));
        assert_eq!(
            resolve_row_column_default(&prefixes, &legal)
                .default_value
                .as_deref(),
            Some("/legal")
        );
    }

    #[test]
    fn row_column_default_left_untouched_when_inapplicable() {
        let legal = row(&[("bundle_id", "legal")]);

        // Non-overridable computed → wizard does not surface a default.
        let forced = column(json!({
            "id": "x", "type": "string", "title": "X",
            "computed": {"op": "var", "path": "bundle_id"},
            "computed_overridable": false,
        }));
        assert!(matches!(
            resolve_row_column_default(&forced, &legal),
            Cow::Borrowed(_)
        ));

        // Overridable computed but the source is absent → no default.
        let tenant = column(json!({
            "id": "route_tenant", "type": "string", "title": "Route tenant",
            "computed": {"op": "var", "path": "bundle_id"},
            "computed_overridable": true,
        }));
        assert_eq!(
            resolve_row_column_default(&tenant, &JsonMap::new()).default_value,
            None
        );

        // No computed expression → the column's own static default survives.
        let team = column(json!({
            "id": "route_team", "type": "string", "title": "Route team",
            "default_value": "default",
        }));
        assert_eq!(
            resolve_row_column_default(&team, &legal)
                .default_value
                .as_deref(),
            Some("default")
        );
    }
}
