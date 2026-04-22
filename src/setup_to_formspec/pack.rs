//! Pack loading functions for FormSpec extraction.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use qa_spec::spec::FormPresentation;
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
use zip::ZipArchive;

use crate::qa::bridge::provider_qa_to_form_spec;
use crate::setup_input::load_setup_spec;
use crate::setup_to_formspec::convert::setup_spec_to_form_spec;
use crate::setup_to_formspec::inference::{capitalize, strip_domain_prefix};

/// Load a `FormSpec` from a pack's `setup.yaml`, if present.
///
/// Falls back to reading `qa/*.json` files from the pack when `setup.yaml`
/// is missing or has no questions.
pub fn pack_to_form_spec(pack_path: &Path, provider_id: &str) -> Option<FormSpec> {
    let mut form_spec = None;

    // Try legacy setup.yaml first
    if let Ok(Some(spec)) = load_setup_spec(pack_path)
        && !spec.questions.is_empty()
    {
        form_spec = Some(setup_spec_to_form_spec(&spec, provider_id));
    }

    // Fallback: try qa/*.json from inside the pack
    if form_spec.is_none() {
        form_spec = load_qa_form_spec_from_pack(pack_path, provider_id);
    }

    augment_with_secret_requirements(form_spec, pack_path, provider_id)
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

fn augment_with_secret_requirements(
    form_spec: Option<FormSpec>,
    pack_path: &Path,
    provider_id: &str,
) -> Option<FormSpec> {
    let secret_requirements = match crate::secrets::load_secret_requirements_from_pack(pack_path) {
        Ok(reqs) => reqs,
        Err(_) => return form_spec,
    };
    if secret_requirements.is_empty() {
        return form_spec;
    }

    let mut form = form_spec.unwrap_or_else(|| empty_form_spec(provider_id));
    let existing_ids: Vec<String> = form
        .questions
        .iter()
        .map(|question| crate::secret_name::canonical_secret_name(&question.id))
        .collect();

    for secret_req in secret_requirements {
        let canonical = crate::secret_name::canonical_secret_name(&secret_req.key);
        let already_covered = existing_ids.iter().any(|existing| {
            canonical == *existing
                || canonical.ends_with(existing)
                || existing.ends_with(&canonical)
        });
        if already_covered {
            continue;
        }

        form.questions.push(QuestionSpec {
            id: canonical.clone(),
            kind: QuestionType::String,
            title: humanize_secret_key(&canonical),
            title_i18n: None,
            description: secret_req
                .description
                .clone()
                .or_else(|| Some(format!("Required secret for {provider_id}"))),
            description_i18n: None,
            required: secret_req.required,
            choices: None,
            default_value: None,
            secret: true,
            visible_if: None,
            constraint: None,
            list: None,
            computed: None,
            policy: Default::default(),
            computed_overridable: false,
        });
    }

    if form.questions.is_empty() {
        None
    } else {
        Some(form)
    }
}

fn empty_form_spec(provider_id: &str) -> FormSpec {
    let display_name = capitalize(&strip_domain_prefix(provider_id));
    FormSpec {
        id: format!("{provider_id}-setup"),
        title: format!("{display_name} setup"),
        version: "1.0.0".to_string(),
        description: None,
        presentation: Some(FormPresentation {
            intro: None,
            theme: None,
            default_locale: Some("en".to_string()),
        }),
        progress_policy: None,
        secrets_policy: None,
        store: vec![],
        validations: vec![],
        includes: vec![],
        questions: vec![],
    }
}

fn humanize_secret_key(value: &str) -> String {
    value
        .split('_')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => {
                    format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
