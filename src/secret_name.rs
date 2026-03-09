//! Normalize secret names to a store-friendly canonical form.
//!
//! Conventions:
//! - Lowercase all ASCII letters
//! - Keep `a-z`, `0-9`, `_`
//! - Map `-`, `.`, ` `, `/` → `_`
//! - Collapse repeated underscores, trim leading/trailing `_`

/// Convert a raw secret name (e.g. `TELEGRAM_BOT_TOKEN`) into canonical form
/// (`telegram_bot_token`).
pub fn canonical_secret_name(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut prev_underscore = false;

    for ch in raw.chars() {
        if let Some(normalized) = normalize_char(ch) {
            if normalized == '_' {
                if prev_underscore {
                    continue;
                }
                prev_underscore = true;
            } else {
                prev_underscore = false;
            }
            result.push(normalized);
        }
    }

    let trimmed = result.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "secret".to_string()
    } else {
        trimmed
    }
}

/// Apply [`canonical_secret_name`] to each segment of a slash-delimited key path.
pub fn canonical_secret_key_path(raw: &str) -> String {
    raw.split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(canonical_secret_name)
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_char(ch: char) -> Option<char> {
    match ch {
        'A'..='Z' => Some(ch.to_ascii_lowercase()),
        'a'..='z' | '0'..='9' | '_' => Some(ch),
        '-' | '.' | ' ' | '/' => Some('_'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_normalizes() {
        assert_eq!(
            canonical_secret_name("TELEGRAM_BOT_TOKEN"),
            "telegram_bot_token"
        );
    }

    #[test]
    fn collapses_underscores() {
        assert_eq!(canonical_secret_name("a__b___c"), "a_b_c");
    }

    #[test]
    fn trims_edge_underscores() {
        assert_eq!(canonical_secret_name("__key__"), "key");
    }

    #[test]
    fn maps_dashes_and_dots() {
        assert_eq!(canonical_secret_name("my-key.name"), "my_key_name");
    }

    #[test]
    fn empty_becomes_secret() {
        assert_eq!(canonical_secret_name(""), "secret");
    }

    #[test]
    fn key_path_normalizes_segments() {
        assert_eq!(canonical_secret_key_path("A/B/C"), "a/b/c");
    }
}
