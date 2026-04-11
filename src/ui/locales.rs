//! Embedded i18n catalogs for the dashboard UI.
//!
//! At compile time we embed every `i18n/*.json` locale file into the
//! binary via `include_dir!`. The dashboard can then serve any locale
//! catalog from memory without touching the filesystem, keeping the
//! setup server offline-friendly.
//!
//! Only `ui.*` keys are exposed to the SPA — `cli.*` keys are filtered
//! out because they are for the terminal flow and their values may
//! contain the hyphenated crate name that must not appear in dashboard
//! copy.

use include_dir::{Dir, include_dir};
use serde_json::{Map, Value};

static CATALOGS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/i18n");

/// Return the BCP-47 locale codes of every embedded catalog, sorted.
///
/// Each code is the filename without the `.json` extension — e.g. the
/// file `i18n/id.json` yields `"id"`, and `i18n/zh-Hant.json` yields
/// `"zh-Hant"`.
pub fn available_codes() -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = CATALOGS
        .files()
        .filter_map(|f| {
            let path = f.path();
            let name = path.file_name()?.to_str()?;
            name.strip_suffix(".json")
        })
        .collect();
    codes.sort_unstable();
    codes
}

/// Return the `ui.*`-filtered catalog for the given locale code.
///
/// Falls back to `en` if the requested code is not embedded. Returns
/// `None` only if even `en.json` is missing (should never happen given
/// the `include_dir!` manifest is resolved at compile time).
pub fn catalog_for(code: &str) -> Option<Value> {
    let parsed = parse_embedded(code).or_else(|| parse_embedded("en"))?;
    Some(filter_ui_keys(parsed))
}

fn parse_embedded(code: &str) -> Option<Value> {
    let filename = format!("{code}.json");
    let file = CATALOGS.get_file(&filename)?;
    let text = file.contents_utf8()?;
    serde_json::from_str(text).ok()
}

fn filter_ui_keys(value: Value) -> Value {
    match value {
        Value::Object(obj) => {
            let filtered: Map<String, Value> = obj
                .into_iter()
                .filter(|(k, _)| k.starts_with("ui."))
                .collect();
            Value::Object(filtered)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_codes_contains_en_as_source_of_truth() {
        let codes = available_codes();
        assert!(codes.contains(&"en"), "en.json must be embedded");
    }

    #[test]
    fn available_codes_has_many_locales() {
        let codes = available_codes();
        assert!(
            codes.len() >= 10,
            "expected many locale catalogs, got {}",
            codes.len()
        );
    }

    #[test]
    fn available_codes_is_sorted() {
        let codes = available_codes();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted);
    }

    #[test]
    fn catalog_for_en_contains_ui_keys() {
        let catalog = catalog_for("en").expect("en catalog");
        let obj = catalog.as_object().expect("object");
        assert!(
            obj.contains_key("ui.brand.name"),
            "en catalog missing ui.brand.name"
        );
    }

    #[test]
    fn catalog_for_en_excludes_cli_keys() {
        let catalog = catalog_for("en").expect("en catalog");
        let obj = catalog.as_object().expect("object");
        for key in obj.keys() {
            assert!(
                key.starts_with("ui."),
                "unexpected non-ui key in catalog: {key}"
            );
        }
    }

    #[test]
    fn catalog_for_unknown_locale_falls_back_to_en() {
        let catalog = catalog_for("xx-ZZ-nonsense").expect("fallback catalog");
        let obj = catalog.as_object().expect("object");
        assert!(
            obj.contains_key("ui.brand.name"),
            "fallback catalog should still contain english keys"
        );
    }

    #[test]
    fn catalog_for_id_contains_ui_keys() {
        // We have id.json embedded — verify it loads.
        let catalog = catalog_for("id").expect("id catalog");
        let obj = catalog.as_object().expect("object");
        assert!(obj.contains_key("ui.brand.name"));
    }
}
