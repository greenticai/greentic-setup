//! Normalize secret names to a store-friendly canonical form.
//!
//! Conventions:
//! - Lowercase all ASCII letters
//! - Keep `a-z`, `0-9`, `_`
//! - Map `-`, `.`, ` `, `/` → `_`
//! - Collapse repeated underscores, trim leading/trailing `_`

/// Convert a raw secret name (e.g. `TELEGRAM_BOT_TOKEN`) into canonical form
/// (`telegram_bot_token`).
///
/// The normalization itself lives in `greentic-secrets`
/// ([`greentic_secrets_lib::canonical_secret_name`]) — the single
/// ecosystem-wide definition shared by start/setup/deployer, so a producer and
/// a reader can never derive a name differently.
pub fn canonical_secret_name(raw: &str) -> String {
    greentic_secrets_lib::canonical_secret_name(raw)
}

/// Apply [`canonical_secret_name`] to each segment of a slash-delimited key path.
pub fn canonical_secret_key_path(raw: &str) -> String {
    raw.split('/')
        .filter(|segment| !segment.trim().is_empty())
        .map(canonical_secret_name)
        .collect::<Vec<_>>()
        .join("/")
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
