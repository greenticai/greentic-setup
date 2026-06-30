//! Guards for the env-wizard locale catalogs (`i18n/*.json`).
//!
//! The terminal env wizard localizes its prompts by stable question id
//! (`env_wizard.q.<id>.title`, …) and by generic prompt-loop chrome keys
//! (`setup.qa.prompt.*`, `setup.qa.list.*`). These tests pin three invariants:
//!
//! 1. The English question strings in `en.json` match the deployer's
//!    `manifest_form_spec_for_env` verbatim — `en.json` is the feedstock the
//!    translator generates the other locales from, so it must not drift from
//!    the canonical source.
//! 2. The hand-authored Dutch catalog (`nl.json`) carries every new key.
//! 3. Placeholders (`{}`), backticks and newlines are preserved across
//!    locales, so a translation can't silently break `{}` substitution or a
//!    multi-line layout.

use std::collections::BTreeMap;

use greentic_deployer::cli::env_manifest::manifest_form_spec_for_env;
use qa_spec::QuestionSpec;
use serde_json::Value;

const EN: &str = include_str!("../i18n/en.json");
const NL: &str = include_str!("../i18n/nl.json");

fn catalog(raw: &str) -> BTreeMap<String, String> {
    let value: Value = serde_json::from_str(raw).expect("catalog is valid JSON");
    value
        .as_object()
        .expect("catalog is an object")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().expect("string value").to_string()))
        .collect()
}

/// The question/form strings the env wizard localizes, keyed exactly as
/// [`greentic_setup`]'s `localize_spec` derives them.
fn expected_question_strings() -> BTreeMap<String, String> {
    fn walk(q: &QuestionSpec, out: &mut BTreeMap<String, String>) {
        out.insert(format!("env_wizard.q.{}.title", q.id), q.title.clone());
        if let Some(desc) = &q.description {
            out.insert(format!("env_wizard.q.{}.desc", q.id), desc.clone());
        }
        if let Some(list) = &q.list {
            if let Some(label) = &list.item_label {
                out.insert(
                    format!("env_wizard.list.{}.item_label", q.id),
                    label.clone(),
                );
            }
            for field in &list.fields {
                walk(field, out);
            }
        }
    }

    let spec = manifest_form_spec_for_env("local");
    let mut out = BTreeMap::new();
    out.insert("env_wizard.form.title".to_string(), spec.title.clone());
    if let Some(desc) = &spec.description {
        out.insert("env_wizard.form.desc".to_string(), desc.clone());
    }
    for q in &spec.questions {
        walk(q, &mut out);
    }
    out
}

#[test]
fn en_catalog_matches_deployer_question_strings() {
    let en = catalog(EN);
    let mut missing = Vec::new();
    let mut drifted = Vec::new();
    for (key, expected) in expected_question_strings() {
        match en.get(&key) {
            None => missing.push(key),
            Some(actual) if actual != &expected => {
                drifted.push(format!("{key}: en.json={actual:?} spec={expected:?}"));
            }
            Some(_) => {}
        }
    }
    assert!(
        missing.is_empty() && drifted.is_empty(),
        "en.json out of sync with manifest_form_spec_for_env — \
         re-run the catalog update + retranslate.\nmissing: {missing:?}\ndrifted: {drifted:#?}"
    );
}

/// Every new env-wizard key in `en.json` must exist in the hand-authored
/// `nl.json` (the locale this PR ships by hand); a gap means a Dutch user
/// silently falls back to English for that string.
#[test]
fn nl_catalog_covers_all_new_keys() {
    let en = catalog(EN);
    let nl = catalog(NL);
    let prefixes = ["env_wizard.", "setup.qa.prompt.", "setup.qa.list."];
    let missing: Vec<_> = en
        .keys()
        .filter(|k| prefixes.iter().any(|p| k.starts_with(p)))
        .filter(|k| !nl.contains_key(*k))
        .cloned()
        .collect();
    assert!(missing.is_empty(), "nl.json missing keys: {missing:?}");
}

/// A translation must preserve `{}` substitution slots, backtick spans, and
/// newline-driven layout, or runtime formatting/rendering breaks.
#[test]
fn nl_preserves_placeholders_backticks_and_newlines() {
    let en = catalog(EN);
    let nl = catalog(NL);
    let prefixes = ["env_wizard.", "setup.qa.prompt.", "setup.qa.list."];
    let mut problems = Vec::new();
    for (key, e) in &en {
        if !prefixes.iter().any(|p| key.starts_with(p)) {
            continue;
        }
        let Some(n) = nl.get(key) else { continue };
        for (label, ch) in [("{}", "{}"), ("backtick", "`"), ("newline", "\n")] {
            if e.matches(ch).count() != n.matches(ch).count() {
                problems.push(format!(
                    "{key}: {label} count en={} nl={}",
                    e.matches(ch).count(),
                    n.matches(ch).count()
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}
