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

/// Current pack version published to GHCR.
pub const PACK_VERSION: &str = "0.5.1";

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
pub fn oci_reference(info: &ProviderPackInfo) -> String {
    format!(
        "oci://{}/{pack}:{version}",
        OCI_REGISTRY_BASE,
        pack = info.pack_name,
        version = PACK_VERSION,
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
    fn oci_reference_format() {
        let info = lookup("telegram").unwrap();
        let reference = oci_reference(info);
        assert!(
            reference.starts_with("oci://ghcr.io/greenticai/packs/messaging/messaging-telegram:")
        );
        assert!(reference.ends_with(PACK_VERSION));
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
