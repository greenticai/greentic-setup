//! Load and validate user-provided setup answers from JSON/YAML files.
//!
//! Supports both per-provider keyed answers (where the top-level JSON object
//! maps provider IDs to their answers) and flat single-provider answers.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, anyhow};
use rpassword::prompt_password;
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value};
use zip::{ZipArchive, result::ZipError};

/// Answers loaded from a user-provided `--setup-input` file.
#[derive(Clone)]
pub struct SetupInputAnswers {
    raw: Value,
    provider_keys: BTreeSet<String>,
}

impl SetupInputAnswers {
    /// Creates a new helper with the raw file data and the set of known provider IDs.
    pub fn new(raw: Value, provider_keys: BTreeSet<String>) -> anyhow::Result<Self> {
        Ok(Self { raw, provider_keys })
    }

    /// Returns the answers that correspond to a provider/pack.
    ///
    /// If the raw value is keyed by provider ID, returns only that provider's
    /// answers.  Otherwise, returns the entire raw value (flat mode).
    pub fn answers_for_provider(&self, provider: &str) -> Option<&Value> {
        if let Some(map) = self.raw.as_object() {
            if let Some(value) = map.get(provider) {
                return Some(value);
            }
            if !self.provider_keys.is_empty()
                && map.keys().all(|key| self.provider_keys.contains(key))
            {
                return None;
            }
        }
        Some(&self.raw)
    }
}

/// Reads a JSON/YAML answers file.
pub fn load_setup_input(path: &Path) -> anyhow::Result<Value> {
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw)
        .or_else(|_| serde_yaml_bw::from_str(&raw))
        .with_context(|| format!("parse setup input {}", path.display()))
}

/// Represents a provider setup spec extracted from `assets/setup.yaml`.
#[derive(Debug, Deserialize)]
pub struct SetupSpec {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub questions: Vec<SetupQuestion>,
}

/// A single setup question definition.
#[derive(Debug, Deserialize)]
pub struct SetupQuestion {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub visible_if: Option<SetupVisibleIf>,
}

/// Conditional visibility for a setup question.
///
/// Example in setup.yaml:
/// ```yaml
/// visible_if:
///   field: public_base_url_mode
///   eq: static
/// ```
#[derive(Debug, Deserialize)]
pub struct SetupVisibleIf {
    pub field: String,
    #[serde(default)]
    pub eq: Option<String>,
}

fn default_kind() -> String {
    "string".to_string()
}

/// Load a `SetupSpec` from `assets/setup.yaml` inside a `.gtpack` archive.
///
/// Falls back to reading `setup.yaml` from the filesystem next to the pack
/// (sibling or `assets/` subdirectory) when the archive does not contain it.
pub fn load_setup_spec(pack_path: &Path) -> anyhow::Result<Option<SetupSpec>> {
    let file = File::open(pack_path)?;
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(ZipError::InvalidArchive(_)) | Err(ZipError::UnsupportedArchive(_)) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let contents = match read_setup_yaml(&mut archive)? {
        Some(value) => value,
        None => match read_setup_yaml_from_filesystem(pack_path)? {
            Some(value) => value,
            None => return Ok(None),
        },
    };
    let spec: SetupSpec =
        serde_yaml_bw::from_str(&contents).context("parse provider setup spec")?;
    Ok(Some(spec))
}

fn read_setup_yaml(archive: &mut ZipArchive<File>) -> anyhow::Result<Option<String>> {
    for entry in ["assets/setup.yaml", "setup.yaml"] {
        match archive.by_name(entry) {
            Ok(mut file) => {
                let mut contents = String::new();
                file.read_to_string(&mut contents)?;
                return Ok(Some(contents));
            }
            Err(ZipError::FileNotFound) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(None)
}

/// Fallback: look for `setup.yaml` on the filesystem near the `.gtpack` file.
///
/// Searches sibling paths relative to the pack file:
///   1. `<pack_dir>/assets/setup.yaml`
///   2. `<pack_dir>/setup.yaml`
///
/// Also searches based on pack filename (e.g. for `messaging-telegram.gtpack`):
///   3. `<pack_dir>/../../../packs/messaging-telegram/assets/setup.yaml`
///   4. `<pack_dir>/../../../packs/messaging-telegram/setup.yaml`
fn read_setup_yaml_from_filesystem(pack_path: &Path) -> anyhow::Result<Option<String>> {
    let pack_dir = pack_path.parent().unwrap_or(Path::new("."));
    let pack_stem = pack_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    let candidates = [
        pack_dir.join("assets/setup.yaml"),
        pack_dir.join("setup.yaml"),
    ];

    // Also try a source-layout path: packs/<pack_stem>/assets/setup.yaml
    let mut all_candidates: Vec<std::path::PathBuf> = candidates.to_vec();
    if !pack_stem.is_empty() {
        // Walk up to find a packs/ directory (common in greentic-messaging-providers layout)
        for ancestor in pack_dir.ancestors().skip(1).take(4) {
            let source_dir = ancestor.join("packs").join(pack_stem);
            if source_dir.is_dir() {
                all_candidates.push(source_dir.join("assets/setup.yaml"));
                all_candidates.push(source_dir.join("setup.yaml"));
                break;
            }
        }
    }

    for candidate in &all_candidates {
        if candidate.is_file() {
            let contents = fs::read_to_string(candidate)?;
            return Ok(Some(contents));
        }
    }
    Ok(None)
}

/// Collect setup answers for a provider pack.
///
/// Uses provided input answers if available, otherwise falls back to
/// interactive prompting (if `interactive` is true) or returns an error.
pub fn collect_setup_answers(
    pack_path: &Path,
    provider_id: &str,
    setup_input: Option<&SetupInputAnswers>,
    interactive: bool,
) -> anyhow::Result<Value> {
    let spec = load_setup_spec(pack_path)?;
    if let Some(input) = setup_input {
        if let Some(value) = input.answers_for_provider(provider_id) {
            let answers = ensure_object(value.clone())?;
            ensure_required_answers(spec.as_ref(), &answers)?;
            return Ok(answers);
        }
        if has_required_questions(spec.as_ref()) {
            return Err(anyhow!("setup input missing answers for {provider_id}"));
        }
        return Ok(Value::Object(JsonMap::new()));
    }
    if let Some(spec) = spec {
        if spec.questions.is_empty() {
            return Ok(Value::Object(JsonMap::new()));
        }
        if interactive {
            let answers = prompt_setup_answers(&spec, provider_id)?;
            ensure_required_answers(Some(&spec), &answers)?;
            return Ok(answers);
        }
        return Err(anyhow!(
            "setup answers required for {provider_id} but run is non-interactive"
        ));
    }
    Ok(Value::Object(JsonMap::new()))
}

fn has_required_questions(spec: Option<&SetupSpec>) -> bool {
    spec.map(|spec| spec.questions.iter().any(|q| q.required))
        .unwrap_or(false)
}

/// Validate that all required answers are present.
pub fn ensure_required_answers(spec: Option<&SetupSpec>, answers: &Value) -> anyhow::Result<()> {
    let map = answers
        .as_object()
        .ok_or_else(|| anyhow!("setup answers must be an object"))?;
    if let Some(spec) = spec {
        for question in spec.questions.iter().filter(|q| q.required) {
            match map.get(&question.name) {
                Some(value) if !value.is_null() => continue,
                _ => {
                    return Err(anyhow!(
                        "missing required setup answer for {}",
                        question.name
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Ensure a JSON value is an object.
pub fn ensure_object(value: Value) -> anyhow::Result<Value> {
    match value {
        Value::Object(_) => Ok(value),
        other => Err(anyhow!(
            "setup answers must be a JSON object, got {}",
            other
        )),
    }
}

/// Interactively prompt the user for setup answers.
pub fn prompt_setup_answers(spec: &SetupSpec, provider: &str) -> anyhow::Result<Value> {
    if spec.questions.is_empty() {
        return Ok(Value::Object(JsonMap::new()));
    }
    let title = spec.title.as_deref().unwrap_or(provider).to_string();
    println!("\nConfiguring {provider}: {title}");
    let mut answers = JsonMap::new();
    for question in &spec.questions {
        if question.name.trim().is_empty() {
            continue;
        }
        if let Some(value) = ask_setup_question(question)? {
            answers.insert(question.name.clone(), value);
        }
    }
    Ok(Value::Object(answers))
}

fn ask_setup_question(question: &SetupQuestion) -> anyhow::Result<Option<Value>> {
    if let Some(help) = question.help.as_ref()
        && !help.trim().is_empty()
    {
        println!("  {help}");
    }
    if !question.choices.is_empty() {
        println!("  Choices:");
        for (idx, choice) in question.choices.iter().enumerate() {
            println!("    {}) {}", idx + 1, choice);
        }
    }
    loop {
        let prompt = build_question_prompt(question);
        let input = read_question_input(&prompt, question.secret)?;
        let trimmed = input.trim();
        if trimmed.is_empty() {
            if let Some(default) = question.default.clone() {
                return Ok(Some(default));
            }
            if question.required {
                println!("  This field is required.");
                continue;
            }
            return Ok(None);
        }
        match parse_question_value(question, trimmed) {
            Ok(value) => return Ok(Some(value)),
            Err(err) => {
                println!("  {err}");
                continue;
            }
        }
    }
}

fn build_question_prompt(question: &SetupQuestion) -> String {
    let mut prompt = question
        .title
        .as_deref()
        .unwrap_or(&question.name)
        .to_string();
    if question.kind != "string" {
        prompt = format!("{prompt} [{}]", question.kind);
    }
    if let Some(default) = &question.default {
        prompt = format!("{prompt} [default: {}]", display_value(default));
    }
    prompt.push_str(": ");
    prompt
}

fn read_question_input(prompt: &str, secret: bool) -> anyhow::Result<String> {
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

fn parse_question_value(question: &SetupQuestion, input: &str) -> anyhow::Result<Value> {
    let kind = question.kind.to_lowercase();
    match kind.as_str() {
        "number" => serde_json::Number::from_str(input)
            .map(Value::Number)
            .map_err(|err| anyhow!("invalid number: {err}")),
        "choice" => {
            if question.choices.is_empty() {
                return Ok(Value::String(input.to_string()));
            }
            if let Ok(index) = input.parse::<usize>()
                && let Some(choice) = question.choices.get(index - 1)
            {
                return Ok(Value::String(choice.clone()));
            }
            for choice in &question.choices {
                if choice == input {
                    return Ok(Value::String(choice.clone()));
                }
            }
            Err(anyhow!("invalid choice '{input}'"))
        }
        "boolean" => match input.to_lowercase().as_str() {
            "true" | "t" | "yes" | "y" => Ok(Value::Bool(true)),
            "false" | "f" | "no" | "n" => Ok(Value::Bool(false)),
            _ => Err(anyhow!("invalid boolean value")),
        },
        _ => Ok(Value::String(input.to_string())),
    }
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(v) => v.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    fn create_test_pack(yaml: &str) -> anyhow::Result<(tempfile::TempDir, std::path::PathBuf)> {
        let temp_dir = tempfile::tempdir()?;
        let pack_path = temp_dir.path().join("messaging-test.gtpack");
        let file = File::create(&pack_path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("assets/setup.yaml", options)?;
        writer.write_all(yaml.as_bytes())?;
        writer.finish()?;
        Ok((temp_dir, pack_path))
    }

    #[test]
    fn parse_setup_yaml_questions() -> anyhow::Result<()> {
        let yaml =
            "provider_id: dummy\nquestions:\n  - name: public_base_url\n    required: true\n";
        let (_dir, pack_path) = create_test_pack(yaml)?;
        let spec = load_setup_spec(&pack_path)?.expect("expected spec");
        assert_eq!(spec.questions.len(), 1);
        assert_eq!(spec.questions[0].name, "public_base_url");
        assert!(spec.questions[0].required);
        Ok(())
    }

    #[test]
    fn collect_setup_answers_uses_input() -> anyhow::Result<()> {
        let yaml =
            "provider_id: telegram\nquestions:\n  - name: public_base_url\n    required: true\n";
        let (_dir, pack_path) = create_test_pack(yaml)?;
        let provider_keys = BTreeSet::from(["messaging-telegram".to_string()]);
        let raw = json!({ "messaging-telegram": { "public_base_url": "https://example.com" } });
        let answers = SetupInputAnswers::new(raw, provider_keys)?;
        let collected =
            collect_setup_answers(&pack_path, "messaging-telegram", Some(&answers), false)?;
        assert_eq!(
            collected.get("public_base_url"),
            Some(&Value::String("https://example.com".to_string()))
        );
        Ok(())
    }

    #[test]
    fn collect_setup_answers_missing_required_errors() -> anyhow::Result<()> {
        let yaml =
            "provider_id: slack\nquestions:\n  - name: slack_bot_token\n    required: true\n";
        let (_dir, pack_path) = create_test_pack(yaml)?;
        let provider_keys = BTreeSet::from(["messaging-slack".to_string()]);
        let raw = json!({ "messaging-slack": {} });
        let answers = SetupInputAnswers::new(raw, provider_keys)?;
        let error = collect_setup_answers(&pack_path, "messaging-slack", Some(&answers), false)
            .unwrap_err();
        assert!(error.to_string().contains("missing required setup answer"));
        Ok(())
    }
}
