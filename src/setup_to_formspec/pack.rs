//! Pack loading functions for FormSpec extraction.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use qa_spec::FormSpec;
use zip::ZipArchive;

use crate::qa::bridge::provider_qa_to_form_spec;
use crate::setup_input::load_setup_spec;
use crate::setup_to_formspec::convert::setup_spec_to_form_spec;

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
