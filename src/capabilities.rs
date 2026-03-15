//! Capability validation and auto-upgrade for provider gtpacks.
//!
//! Provider gtpacks must contain a `greentic.ext.capabilities.v1` extension
//! in their `manifest.cbor` for the operator to discover and mount them.
//! Old gtpacks built before the capabilities extension was introduced will
//! silently fail at runtime.
//!
//! This module provides validation during `gtc setup` and auto-upgrade from
//! known source locations when a newer pack with capabilities is found.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Context;
use zip::ZipArchive;

use crate::discovery;

const EXT_CAPABILITIES_V1: &str = "greentic.ext.capabilities.v1";

fn canonicalize_or_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Result of validating and upgrading packs in a bundle.
pub struct UpgradeReport {
    pub checked: usize,
    pub upgraded: Vec<UpgradedPack>,
    pub warnings: Vec<PackWarning>,
}

pub struct UpgradedPack {
    pub provider_id: String,
    pub source_path: PathBuf,
}

pub struct PackWarning {
    pub provider_id: String,
    pub message: String,
}

/// Check whether a gtpack has the `greentic.ext.capabilities.v1` extension.
pub fn has_capabilities_extension(pack_path: &Path) -> bool {
    read_has_capabilities(pack_path).unwrap_or(false)
}

fn read_has_capabilities(pack_path: &Path) -> anyhow::Result<bool> {
    let file = std::fs::File::open(pack_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut entry = match archive.by_name("manifest.cbor") {
        Ok(e) => e,
        Err(_) => return Ok(false),
    };
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    // Search for the capabilities extension key in the raw CBOR bytes.
    // This avoids depending on the exact CBOR schema which may vary between
    // greentic-types versions. The string is unique enough to be reliable.
    Ok(bytes
        .windows(EXT_CAPABILITIES_V1.len())
        .any(|w| w == EXT_CAPABILITIES_V1.as_bytes()))
}

/// Search known source locations for a replacement gtpack that has capabilities.
///
/// Search order:
/// 1. Sibling bundles in the same parent directory
/// 2. `greentic-messaging-providers/target/packs/` in ancestor dirs
fn find_replacement_pack(pack_filename: &str, bundle_path: &Path, domain: &str) -> Option<PathBuf> {
    let bundle_abs = canonicalize_or_path(bundle_path);
    let parent = bundle_abs.parent()?;

    // 1. Sibling bundles: ../*/providers/{domain}/{filename}
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let candidate_bundle = canonicalize_or_path(&entry.path());
            if candidate_bundle == bundle_abs || !candidate_bundle.is_dir() {
                continue;
            }
            let candidate = candidate_bundle
                .join("providers")
                .join(domain)
                .join(pack_filename);
            if candidate.is_file() && has_capabilities_extension(&candidate) {
                return Some(candidate);
            }
        }
    }

    // 2. greentic-messaging-providers build output in ancestor dirs
    for ancestor in parent.ancestors().take(4) {
        let candidate = ancestor
            .join("greentic-messaging-providers")
            .join("target")
            .join("packs")
            .join(pack_filename);
        if candidate.is_file() && has_capabilities_extension(&candidate) {
            return Some(candidate);
        }
    }

    None
}

/// Validate all provider gtpacks in a bundle and auto-upgrade those missing capabilities.
pub fn validate_and_upgrade_packs(bundle_path: &Path) -> anyhow::Result<UpgradeReport> {
    let discovered = discovery::discover(bundle_path)
        .context("failed to discover providers for capability validation")?;

    let mut report = UpgradeReport {
        checked: 0,
        upgraded: Vec::new(),
        warnings: Vec::new(),
    };

    for provider in &discovered.providers {
        report.checked += 1;

        if has_capabilities_extension(&provider.pack_path) {
            continue;
        }

        let pack_filename = provider
            .pack_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if pack_filename.is_empty() {
            continue;
        }

        // Try to find a replacement
        if let Some(replacement) =
            find_replacement_pack(pack_filename, bundle_path, &provider.domain)
        {
            // Backup original
            let backup = provider.pack_path.with_extension("gtpack.bak");
            std::fs::copy(&provider.pack_path, &backup).with_context(|| {
                format!(
                    "failed to backup {} before upgrade",
                    provider.pack_path.display()
                )
            })?;

            // Copy replacement
            std::fs::copy(&replacement, &provider.pack_path).with_context(|| {
                format!(
                    "failed to copy replacement pack from {}",
                    replacement.display()
                )
            })?;

            println!(
                "  [upgrade] {}: replaced with {} (capabilities extension added)",
                provider.provider_id,
                replacement.display()
            );

            report.upgraded.push(UpgradedPack {
                provider_id: provider.provider_id.clone(),
                source_path: replacement,
            });
        } else {
            let msg = format!(
                "pack missing greentic.ext.capabilities.v1 — operator will not detect this provider. \
                 Replace with a newer build of {}",
                pack_filename,
            );
            println!("  [warn] {}: {}", provider.provider_id, msg);
            report.warnings.push(PackWarning {
                provider_id: provider.provider_id.clone(),
                message: msg,
            });
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    use serde_cbor::value::Value as CV;

    /// Write a minimal gtpack zip with a CBOR manifest.
    fn write_test_gtpack(path: &Path, with_capabilities: bool) {
        let mut map = BTreeMap::new();
        map.insert(
            CV::Text("schema_version".into()),
            CV::Text("pack-v1".into()),
        );
        map.insert(CV::Text("pack_id".into()), CV::Text("test-provider".into()));
        map.insert(CV::Text("version".into()), CV::Text("0.1.0".into()));
        map.insert(CV::Text("kind".into()), CV::Text("provider".into()));
        map.insert(CV::Text("publisher".into()), CV::Text("test".into()));

        if with_capabilities {
            let mut ext_inner = BTreeMap::new();
            ext_inner.insert(
                CV::Text("kind".into()),
                CV::Text(EXT_CAPABILITIES_V1.into()),
            );
            ext_inner.insert(CV::Text("version".into()), CV::Text("1.0.0".into()));

            let mut exts = BTreeMap::new();
            exts.insert(CV::Text(EXT_CAPABILITIES_V1.into()), CV::Map(ext_inner));
            map.insert(CV::Text("extensions".into()), CV::Map(exts));
        }

        let manifest = CV::Map(map);
        let bytes = serde_cbor::to_vec(&manifest).expect("encode cbor");
        let file = File::create(path).expect("create file");
        let mut zip = ZipWriter::new(file);
        zip.start_file("manifest.cbor", FileOptions::<()>::default())
            .expect("start file");
        zip.write_all(&bytes).expect("write manifest");
        zip.finish().expect("finish zip");
    }

    #[test]
    fn has_capabilities_returns_true_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("test.gtpack");
        write_test_gtpack(&pack, true);
        assert!(has_capabilities_extension(&pack));
    }

    #[test]
    fn has_capabilities_returns_false_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("test.gtpack");
        write_test_gtpack(&pack, false);
        assert!(!has_capabilities_extension(&pack));
    }

    #[test]
    fn has_capabilities_returns_false_for_nonexistent() {
        assert!(!has_capabilities_extension(Path::new(
            "/nonexistent.gtpack"
        )));
    }

    #[test]
    fn find_replacement_from_sibling_bundle() {
        let root = tempfile::tempdir().unwrap();

        // Bundle A: no capabilities
        let bundle_a = root.path().join("bundle-a");
        let providers_a = bundle_a.join("providers").join("messaging");
        std::fs::create_dir_all(&providers_a).unwrap();
        write_test_gtpack(&providers_a.join("messaging-test.gtpack"), false);

        // Bundle B: has capabilities
        let bundle_b = root.path().join("bundle-b");
        let providers_b = bundle_b.join("providers").join("messaging");
        std::fs::create_dir_all(&providers_b).unwrap();
        write_test_gtpack(&providers_b.join("messaging-test.gtpack"), true);

        let result = find_replacement_pack("messaging-test.gtpack", &bundle_a, "messaging");
        assert!(result.is_some());
        assert!(
            canonicalize_or_path(&result.unwrap()).starts_with(canonicalize_or_path(&bundle_b))
        );
    }

    #[test]
    fn find_replacement_returns_none_when_no_better_pack() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("bundle");
        std::fs::create_dir_all(bundle.join("providers").join("messaging")).unwrap();
        write_test_gtpack(
            &bundle
                .join("providers")
                .join("messaging")
                .join("test.gtpack"),
            false,
        );

        let result = find_replacement_pack("test.gtpack", &bundle, "messaging");
        assert!(result.is_none());
    }
}
