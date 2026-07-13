//! Provider kind → pack mapping.
//!
//! Maps a user-facing provider kind (`telegram`, `slack`, `webex`, `teams`) to
//! the corresponding pack name on the OCI registry and the `provider_type`
//! string the deployer's messaging-endpoint engine expects.

/// A provider pack descriptor: how a kind maps to a concrete pack and
/// engine-level provider type.
#[derive(Debug, Clone)]
pub struct ProviderPackInfo {
    /// User-facing kind (the CLI positional, e.g. `telegram`).
    pub kind: &'static str,
    /// OCI pack name (the last segment of the GHCR path, e.g.
    /// `messaging-telegram`).
    pub pack_name: &'static str,
    /// `provider_type` value for the deployer's endpoint engine (determines
    /// webhook-secret auto-minting and engine transforms).
    pub provider_type: &'static str,
    /// Default `provider_id` (instance identity) when the user does not
    /// override it. Matches the `provider_id:` field in each pack's
    /// `setup.yaml`.
    pub default_provider_id: &'static str,
}

/// OCI registry base for provider packs.
pub const OCI_REGISTRY_BASE: &str = "ghcr.io/greenticai/packs/messaging";

/// Default OCI tag for provider packs. Uses the mutable `:stable` tag that the
/// publish workflow pushes on every release, so it cannot go stale the way a
/// hand-maintained version constant would.
pub const PACK_TAG: &str = "stable";

/// All known provider kinds.
const REGISTRY: &[ProviderPackInfo] = &[
    ProviderPackInfo {
        kind: "telegram",
        pack_name: "messaging-telegram",
        provider_type: "telegram",
        default_provider_id: "telegram",
    },
    ProviderPackInfo {
        kind: "slack",
        pack_name: "messaging-slack",
        provider_type: "slack",
        default_provider_id: "slack",
    },
    ProviderPackInfo {
        kind: "webex",
        pack_name: "messaging-webex",
        provider_type: "webex",
        default_provider_id: "webex",
    },
    ProviderPackInfo {
        kind: "teams",
        pack_name: "messaging-teams-graph",
        provider_type: "teams",
        default_provider_id: "teams",
    },
];

/// Look up a provider kind. Returns `None` for unknown kinds.
pub fn lookup(kind: &str) -> Option<&'static ProviderPackInfo> {
    REGISTRY.iter().find(|info| info.kind == kind)
}

/// All known kind strings (for CLI help / error messages).
pub fn known_kinds() -> Vec<&'static str> {
    REGISTRY.iter().map(|info| info.kind).collect()
}

/// Build the full OCI reference for a provider pack.
///
/// When `tag_override` is `Some`, uses that tag verbatim (the `--pack-version`
/// escape hatch). Otherwise falls back to [`PACK_TAG`] (`"stable"`).
pub fn oci_reference(info: &ProviderPackInfo, tag_override: Option<&str>) -> String {
    let tag = tag_override.unwrap_or(PACK_TAG);
    format!(
        "oci://{}/{pack}:{tag}",
        OCI_REGISTRY_BASE,
        pack = info.pack_name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_kinds() {
        for kind in &["telegram", "slack", "webex", "teams"] {
            let info = lookup(kind).unwrap_or_else(|| panic!("kind `{kind}` not in registry"));
            assert!(!info.pack_name.is_empty());
            assert!(!info.provider_type.is_empty());
        }
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("whatsapp").is_none());
        assert!(lookup("").is_none());
    }

    #[test]
    fn oci_reference_uses_stable_tag_by_default() {
        let info = lookup("telegram").unwrap();
        let reference = oci_reference(info, None);
        assert_eq!(
            reference,
            "oci://ghcr.io/greenticai/packs/messaging/messaging-telegram:stable"
        );
    }

    /// Guard against somebody silently re-pinning a semver version into the
    /// default OCI reference. The default tag must NOT match `X.Y.Z`.
    #[test]
    fn oci_reference_default_contains_no_hardcoded_semver() {
        let re = regex::Regex::new(r"[0-9]+\.[0-9]+\.[0-9]+").unwrap();
        for kind in &["telegram", "slack", "webex", "teams"] {
            let info = lookup(kind).unwrap();
            let reference = oci_reference(info, None);
            assert!(
                !re.is_match(&reference),
                "default OCI reference for `{kind}` contains a hardcoded semver: {reference}"
            );
        }
    }

    #[test]
    fn oci_reference_with_version_override() {
        let info = lookup("slack").unwrap();
        let reference = oci_reference(info, Some("0.5.6"));
        assert_eq!(
            reference,
            "oci://ghcr.io/greenticai/packs/messaging/messaging-slack:0.5.6"
        );
    }

    #[test]
    fn known_kinds_returns_all_four() {
        let kinds = known_kinds();
        assert_eq!(kinds.len(), 4);
        assert!(kinds.contains(&"telegram"));
        assert!(kinds.contains(&"slack"));
        assert!(kinds.contains(&"webex"));
        assert!(kinds.contains(&"teams"));
    }
}
