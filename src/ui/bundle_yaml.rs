//! Read/write helpers for `bundle.yaml` (or `greentic.demo.yaml` in older bundles).
//!
//! Bundles may use either filename. Both contain the same schema.
//! This module abstracts over the filename variant and provides typed
//! access to the `extension_providers` and `capabilities` sections.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ── Bundle YAML schema (partial) ─────────────────────────────────────────────

/// Subset of bundle.yaml that this module reads and writes.
///
/// Unknown fields are preserved via `flatten` so round-trip writes don't
/// lose information that other tools manage.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BundleYaml {
    #[serde(default)]
    pub extension_providers: Vec<String>,

    #[serde(default)]
    pub capabilities: Vec<String>,

    // All other fields passed through unchanged.
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

// ── Path resolution ───────────────────────────────────────────────────────────

/// Returns the path to the bundle yaml file if it exists, checking both
/// `bundle.yaml` and `greentic.demo.yaml`.
///
/// Returns `None` if neither file exists.
pub fn find_bundle_yaml(bundle_root: &Path) -> Option<PathBuf> {
    for name in &["bundle.yaml", "greentic.demo.yaml"] {
        let candidate = bundle_root.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Returns the preferred write path for the bundle yaml.
///
/// If `bundle.yaml` exists, returns it. Otherwise checks `greentic.demo.yaml`.
/// Falls back to `bundle.yaml` if neither exists (creating a new one).
pub fn write_path(bundle_root: &Path) -> PathBuf {
    find_bundle_yaml(bundle_root).unwrap_or_else(|| bundle_root.join("bundle.yaml"))
}

// ── Read / Write ──────────────────────────────────────────────────────────────

/// Load the bundle yaml from `bundle_root`.
///
/// Returns an empty `BundleYaml` if no file exists (first-run).
/// Returns an error if the file exists but cannot be parsed.
pub fn load(bundle_root: &Path) -> Result<BundleYaml> {
    let path = match find_bundle_yaml(bundle_root) {
        Some(p) => p,
        None => return Ok(BundleYaml::default()),
    };

    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let doc: BundleYaml = serde_yaml_bw::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    Ok(doc)
}

/// Write the bundle yaml back to `bundle_root`.
///
/// Uses the same file that `load` would read (or creates `bundle.yaml` for
/// new bundles). The write is atomic: we serialize to a string first and
/// only write to disk if serialization succeeds.
pub fn save(bundle_root: &Path, doc: &BundleYaml) -> Result<()> {
    let path = write_path(bundle_root);

    // Ensure parent directory exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let contents = serde_yaml_bw::to_string(doc)
        .with_context(|| "failed to serialize bundle.yaml")?;

    std::fs::write(&path, &contents)
        .with_context(|| format!("failed to write {}", path.display()))?;

    Ok(())
}

// ── Provider helpers ──────────────────────────────────────────────────────────

/// Add an OCI ref to `extension_providers` if not already present.
///
/// Returns `true` if the ref was added, `false` if it was already there.
pub fn add_extension_provider(doc: &mut BundleYaml, oci_ref: &str) -> bool {
    if doc.extension_providers.iter().any(|r| r == oci_ref) {
        return false;
    }
    doc.extension_providers.push(oci_ref.to_string());
    true
}

/// Remove an OCI ref from `extension_providers`.
///
/// Returns `true` if the ref was found and removed, `false` if not present.
pub fn remove_extension_provider(doc: &mut BundleYaml, oci_ref: &str) -> bool {
    let before = doc.extension_providers.len();
    doc.extension_providers.retain(|r| r != oci_ref);
    doc.extension_providers.len() < before
}

// ── Capability helpers ────────────────────────────────────────────────────────

/// Set the enabled state of a capability in the `capabilities` list.
///
/// When `enabled = true`, adds the capability id if not present.
/// When `enabled = false`, removes it if present.
/// Returns `true` if the document was modified.
pub fn set_capability(doc: &mut BundleYaml, id: &str, enabled: bool) -> bool {
    if enabled {
        if doc.capabilities.iter().any(|c| c == id) {
            return false;
        }
        doc.capabilities.push(id.to_string());
        true
    } else {
        let before = doc.capabilities.len();
        doc.capabilities.retain(|c| c != id);
        doc.capabilities.len() < before
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_yaml(root: &Path, name: &str, content: &str) {
        std::fs::write(root.join(name), content).unwrap();
    }

    #[test]
    fn find_bundle_yaml_prefers_bundle_yaml() {
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), "bundle.yaml", "capabilities: []\n");
        write_yaml(dir.path(), "greentic.demo.yaml", "capabilities: []\n");
        let found = find_bundle_yaml(dir.path()).unwrap();
        assert!(found.ends_with("bundle.yaml"));
    }

    #[test]
    fn find_bundle_yaml_falls_back_to_demo_yaml() {
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), "greentic.demo.yaml", "capabilities: []\n");
        let found = find_bundle_yaml(dir.path()).unwrap();
        assert!(found.ends_with("greentic.demo.yaml"));
    }

    #[test]
    fn find_bundle_yaml_returns_none_when_absent() {
        let dir = tempdir().unwrap();
        assert!(find_bundle_yaml(dir.path()).is_none());
    }

    #[test]
    fn load_returns_default_when_no_file() {
        let dir = tempdir().unwrap();
        let doc = load(dir.path()).unwrap();
        assert!(doc.extension_providers.is_empty());
        assert!(doc.capabilities.is_empty());
    }

    #[test]
    fn load_parses_extension_providers_and_capabilities() {
        let dir = tempdir().unwrap();
        write_yaml(
            dir.path(),
            "bundle.yaml",
            r#"
extension_providers:
  - oci://ghcr.io/greenticai/packs/messaging-slack:latest
capabilities:
  - greentic.cap.bundle_assets.read.v1
"#,
        );
        let doc = load(dir.path()).unwrap();
        assert_eq!(doc.extension_providers.len(), 1);
        assert_eq!(
            doc.extension_providers[0],
            "oci://ghcr.io/greenticai/packs/messaging-slack:latest"
        );
        assert_eq!(doc.capabilities.len(), 1);
        assert_eq!(doc.capabilities[0], "greentic.cap.bundle_assets.read.v1");
    }

    #[test]
    fn save_round_trips_through_load() {
        let dir = tempdir().unwrap();
        let mut doc = BundleYaml::default();
        doc.extension_providers
            .push("oci://ghcr.io/foo:latest".into());
        doc.capabilities.push("greentic.cap.test.v1".into());

        save(dir.path(), &doc).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.extension_providers, doc.extension_providers);
        assert_eq!(loaded.capabilities, doc.capabilities);
    }

    #[test]
    fn add_extension_provider_appends_new_ref() {
        let mut doc = BundleYaml::default();
        let added = add_extension_provider(&mut doc, "oci://foo:latest");
        assert!(added);
        assert_eq!(doc.extension_providers, vec!["oci://foo:latest"]);
    }

    #[test]
    fn add_extension_provider_is_idempotent() {
        let mut doc = BundleYaml::default();
        add_extension_provider(&mut doc, "oci://foo:latest");
        let added_again = add_extension_provider(&mut doc, "oci://foo:latest");
        assert!(!added_again);
        assert_eq!(doc.extension_providers.len(), 1);
    }

    #[test]
    fn remove_extension_provider_removes_existing() {
        let mut doc = BundleYaml::default();
        doc.extension_providers.push("oci://foo:latest".into());
        let removed = remove_extension_provider(&mut doc, "oci://foo:latest");
        assert!(removed);
        assert!(doc.extension_providers.is_empty());
    }

    #[test]
    fn remove_extension_provider_noop_for_missing() {
        let mut doc = BundleYaml::default();
        let removed = remove_extension_provider(&mut doc, "oci://not-here:latest");
        assert!(!removed);
    }

    #[test]
    fn set_capability_enables_and_disables() {
        let mut doc = BundleYaml::default();
        let modified = set_capability(&mut doc, "greentic.cap.test.v1", true);
        assert!(modified);
        assert_eq!(doc.capabilities, vec!["greentic.cap.test.v1"]);

        let modified_again = set_capability(&mut doc, "greentic.cap.test.v1", true);
        assert!(!modified_again); // idempotent

        let disabled = set_capability(&mut doc, "greentic.cap.test.v1", false);
        assert!(disabled);
        assert!(doc.capabilities.is_empty());
    }
}
