//! CLI i18n support for greentic-setup.
//!
//! Provides locale-aware message translation for CLI output.

use std::collections::BTreeMap;
use std::env;

/// CLI internationalization support.
pub struct CliI18n {
    catalog: BTreeMap<String, String>,
    fallback: BTreeMap<String, String>,
}

impl CliI18n {
    /// Create a new CliI18n instance from a requested locale.
    ///
    /// If no locale is specified, the system locale (LC_ALL, LANG) is used.
    pub fn from_request(requested: Option<&str>) -> Result<Self, String> {
        let resolved = resolve_locale(requested);
        let fallback = load_catalog("en")?;
        let catalog = load_catalog(&resolved).unwrap_or_else(|_| fallback.clone());
        Ok(Self { catalog, fallback })
    }

    /// Translate a key to the current locale.
    pub fn t(&self, key: &str) -> String {
        if let Some(v) = self.catalog.get(key) {
            return v.clone();
        }
        if let Some(v) = self.fallback.get(key) {
            return v.clone();
        }
        key.to_string()
    }

    /// Translate a key with format arguments.
    ///
    /// Arguments replace `{}` placeholders in order.
    pub fn tf(&self, key: &str, args: &[&str]) -> String {
        format_template(&self.t(key), args)
    }
}

fn resolve_locale(requested: Option<&str>) -> String {
    if let Some(locale) = requested.and_then(normalize_locale) {
        return locale;
    }
    if let Some(locale) = env::var("LC_ALL")
        .ok()
        .as_deref()
        .and_then(normalize_locale)
    {
        return locale;
    }
    if let Some(locale) = env::var("LANG").ok().as_deref().and_then(normalize_locale) {
        return locale;
    }
    "en".to_string()
}

fn normalize_locale(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let pre_dot = trimmed.split('.').next().unwrap_or(trimmed);
    let normalized = pre_dot.replace('_', "-");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized)
}

fn load_catalog(locale: &str) -> Result<BTreeMap<String, String>, String> {
    let raw = match locale {
        "ar" => include_str!("../i18n/ar.json"),
        "ar-AE" => include_str!("../i18n/ar-AE.json"),
        "ar-DZ" => include_str!("../i18n/ar-DZ.json"),
        "ar-EG" => include_str!("../i18n/ar-EG.json"),
        "ar-IQ" => include_str!("../i18n/ar-IQ.json"),
        "ar-MA" => include_str!("../i18n/ar-MA.json"),
        "ar-SA" => include_str!("../i18n/ar-SA.json"),
        "ar-SD" => include_str!("../i18n/ar-SD.json"),
        "ar-SY" => include_str!("../i18n/ar-SY.json"),
        "ar-TN" => include_str!("../i18n/ar-TN.json"),
        "ay" => include_str!("../i18n/ay.json"),
        "bg" => include_str!("../i18n/bg.json"),
        "bn" => include_str!("../i18n/bn.json"),
        "cs" => include_str!("../i18n/cs.json"),
        "da" => include_str!("../i18n/da.json"),
        "de" => include_str!("../i18n/de.json"),
        "el" => include_str!("../i18n/el.json"),
        "en" => include_str!("../i18n/en.json"),
        "en-GB" => include_str!("../i18n/en-GB.json"),
        "es" => include_str!("../i18n/es.json"),
        "et" => include_str!("../i18n/et.json"),
        "fa" => include_str!("../i18n/fa.json"),
        "fi" => include_str!("../i18n/fi.json"),
        "fr" => include_str!("../i18n/fr.json"),
        "gn" => include_str!("../i18n/gn.json"),
        "gu" => include_str!("../i18n/gu.json"),
        "hi" => include_str!("../i18n/hi.json"),
        "hr" => include_str!("../i18n/hr.json"),
        "ht" => include_str!("../i18n/ht.json"),
        "hu" => include_str!("../i18n/hu.json"),
        "id" => include_str!("../i18n/id.json"),
        "it" => include_str!("../i18n/it.json"),
        "ja" => include_str!("../i18n/ja.json"),
        "km" => include_str!("../i18n/km.json"),
        "kn" => include_str!("../i18n/kn.json"),
        "ko" => include_str!("../i18n/ko.json"),
        "lo" => include_str!("../i18n/lo.json"),
        "lt" => include_str!("../i18n/lt.json"),
        "lv" => include_str!("../i18n/lv.json"),
        "ml" => include_str!("../i18n/ml.json"),
        "mr" => include_str!("../i18n/mr.json"),
        "ms" => include_str!("../i18n/ms.json"),
        "my" => include_str!("../i18n/my.json"),
        "nah" => include_str!("../i18n/nah.json"),
        "ne" => include_str!("../i18n/ne.json"),
        "nl" => include_str!("../i18n/nl.json"),
        "no" => include_str!("../i18n/no.json"),
        "pa" => include_str!("../i18n/pa.json"),
        "pl" => include_str!("../i18n/pl.json"),
        "pt" => include_str!("../i18n/pt.json"),
        "qu" => include_str!("../i18n/qu.json"),
        "ro" => include_str!("../i18n/ro.json"),
        "ru" => include_str!("../i18n/ru.json"),
        "si" => include_str!("../i18n/si.json"),
        "sk" => include_str!("../i18n/sk.json"),
        "sr" => include_str!("../i18n/sr.json"),
        "sv" => include_str!("../i18n/sv.json"),
        "ta" => include_str!("../i18n/ta.json"),
        "te" => include_str!("../i18n/te.json"),
        "th" => include_str!("../i18n/th.json"),
        "tl" => include_str!("../i18n/tl.json"),
        "tr" => include_str!("../i18n/tr.json"),
        "uk" => include_str!("../i18n/uk.json"),
        "ur" => include_str!("../i18n/ur.json"),
        "vi" => include_str!("../i18n/vi.json"),
        "zh" => include_str!("../i18n/zh.json"),
        _ => return Err(format!("unsupported locale `{locale}`")),
    };
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| format!("invalid locale JSON `{locale}`: {err}"))?;
    let obj = value
        .as_object()
        .ok_or_else(|| format!("locale catalog `{locale}` must be an object"))?;
    let mut map = BTreeMap::new();
    for (k, v) in obj {
        let s = v
            .as_str()
            .ok_or_else(|| format!("locale catalog `{locale}` key `{k}` must be a string"))?;
        map.insert(k.to_string(), s.to_string());
    }
    Ok(map)
}

fn format_template(template: &str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut idx = 0usize;
    let mut i = 0usize;
    while let Some(pos) = template[i..].find("{}") {
        let abs = i + pos;
        out.push_str(&template[i..abs]);
        if idx < args.len() {
            out.push_str(args[idx]);
            idx += 1;
        } else {
            out.push_str("{}");
        }
        i = abs + 2;
    }
    out.push_str(&template[i..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_english_catalog() {
        let catalog = load_catalog("en").expect("should load English catalog");
        assert!(catalog.contains_key("cli.bundle.init.creating"));
    }

    #[test]
    fn test_format_template() {
        assert_eq!(format_template("Hello {}", &["World"]), "Hello World");
        assert_eq!(
            format_template("{} + {} = {}", &["1", "2", "3"]),
            "1 + 2 = 3"
        );
        assert_eq!(format_template("No args", &[]), "No args");
    }

    #[test]
    fn test_cli_i18n_translation() {
        let i18n = CliI18n::from_request(Some("en")).expect("should create i18n");
        let msg = i18n.tf("cli.bundle.init.creating", &["/path/to/bundle"]);
        assert!(msg.contains("/path/to/bundle"));
    }
}
