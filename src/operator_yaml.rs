//! Minimal mutator for the operator's greentic.yaml.
//!
//! ONLY scoped to the webchat.notifier section. Do not generalize this;
//! a future operator-config-management framework should replace this fn.

use std::path::Path;

use anyhow::{Context, Result};
use serde_yaml_bw::{Mapping as YamlMapping, Value as YamlValue};

fn yaml_str(s: &str) -> YamlValue {
    YamlValue::String(s.into(), None)
}

/// Set `webchat.notifier.backend = redis` in the operator's greentic.yaml.
///
/// If the file doesn't exist, creates one containing only the webchat section.
/// If it exists, parses, mutates, and atomically writes back.
pub fn enable_redis_notifier_in_greentic_yaml(operator_root: &Path) -> Result<()> {
    let path = operator_root.join("greentic.yaml");

    let mut root: YamlValue = if path.exists() {
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        serde_yaml_bw::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?
    } else {
        YamlValue::Mapping(YamlMapping::new())
    };

    // Ensure root is a mapping (yaml may be empty/null on first run).
    if !matches!(root, YamlValue::Mapping(_)) {
        root = YamlValue::Mapping(YamlMapping::new());
    }
    let map = root.as_mapping_mut().expect("ensured mapping above");

    // Ensure webchat: { ... }
    let webchat = map
        .entry(yaml_str("webchat"))
        .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
    if !matches!(webchat, YamlValue::Mapping(_)) {
        *webchat = YamlValue::Mapping(YamlMapping::new());
    }
    let webchat_map = webchat.as_mapping_mut().expect("ensured mapping above");

    // Ensure webchat.notifier: { ... }
    let notifier = webchat_map
        .entry(yaml_str("notifier"))
        .or_insert_with(|| YamlValue::Mapping(YamlMapping::new()));
    if !matches!(notifier, YamlValue::Mapping(_)) {
        *notifier = YamlValue::Mapping(YamlMapping::new());
    }
    let notifier_map = notifier.as_mapping_mut().expect("ensured mapping above");

    // Set backend = redis.
    notifier_map.insert(yaml_str("backend"), yaml_str("redis"));

    let serialized = serde_yaml_bw::to_string(&root).context("serialize updated greentic.yaml")?;

    // Atomic write via temp-file + rename.
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, serialized.as_bytes())
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_section_into_empty_file() {
        let dir = tempdir().unwrap();
        enable_redis_notifier_in_greentic_yaml(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("greentic.yaml")).unwrap();
        assert!(content.contains("webchat:"), "got: {content}");
        assert!(content.contains("notifier:"), "got: {content}");
        assert!(content.contains("backend: redis"), "got: {content}");
    }

    #[test]
    fn preserves_existing_unrelated_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("greentic.yaml");
        std::fs::write(&path, "binaries:\n  some_bin: /usr/bin/foo\n").unwrap();

        enable_redis_notifier_in_greentic_yaml(dir.path()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("binaries:"), "got: {content}");
        assert!(content.contains("some_bin: /usr/bin/foo"), "got: {content}");
        assert!(content.contains("backend: redis"), "got: {content}");
    }

    #[test]
    fn idempotent() {
        let dir = tempdir().unwrap();
        enable_redis_notifier_in_greentic_yaml(dir.path()).unwrap();
        enable_redis_notifier_in_greentic_yaml(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("greentic.yaml")).unwrap();
        assert!(content.contains("backend: redis"), "got: {content}");
        // Should not have duplicate sections.
        assert_eq!(content.matches("webchat:").count(), 1, "got: {content}");
    }
}
