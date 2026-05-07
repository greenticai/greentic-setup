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

// ---------------------------------------------------------------------------
// Dependency capability validation
// ---------------------------------------------------------------------------

/// Report of dependency capability validation across all packs in the bundle.
pub struct DependencyReport {
    pub satisfied: Vec<SatisfiedCapability>,
    pub missing: Vec<MissingCapability>,
}

pub struct SatisfiedCapability {
    pub capability: String,
    pub required_by: String,
    pub provided_by: String,
}

pub struct MissingCapability {
    pub capability: String,
    pub required_by: String,
}

/// Validate that all pack dependencies have their required_capabilities
/// satisfied by other packs in the bundle.
pub fn validate_dependency_capabilities(bundle_path: &Path) -> anyhow::Result<DependencyReport> {
    let discovered = discovery::discover(bundle_path)
        .context("failed to discover providers for dependency validation")?;

    let mut report = DependencyReport {
        satisfied: Vec::new(),
        missing: Vec::new(),
    };

    // Build capability index: capability_name → provider_id.
    let mut capability_providers: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for provider in &discovered.providers {
        if let Ok(caps) = read_pack_capabilities(&provider.pack_path) {
            for cap_name in caps {
                capability_providers
                    .entry(cap_name)
                    .or_insert_with(|| provider.provider_id.clone());
            }
        }
    }

    // Check each pack's dependencies.
    let mut pack_id_set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for provider in &discovered.providers {
        pack_id_set.insert(provider.provider_id.clone());
    }

    for provider in &discovered.providers {
        let deps = match read_pack_dependencies(&provider.pack_path) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for (dep_pack_id, required_caps) in deps {
            // Skip if dependency pack_id is present directly.
            if pack_id_set.contains(&dep_pack_id) {
                continue;
            }
            for cap in &required_caps {
                if let Some(provided_by) = capability_providers.get(cap) {
                    report.satisfied.push(SatisfiedCapability {
                        capability: cap.clone(),
                        required_by: provider.provider_id.clone(),
                        provided_by: provided_by.clone(),
                    });
                } else {
                    report.missing.push(MissingCapability {
                        capability: cap.clone(),
                        required_by: provider.provider_id.clone(),
                    });
                }
            }
        }
    }

    Ok(report)
}

/// Read capability names from a gtpack manifest.
fn read_pack_capabilities(pack_path: &Path) -> anyhow::Result<Vec<String>> {
    let file = std::fs::File::open(pack_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut entry = archive.by_name("manifest.cbor")?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    let cbor: serde_cbor::Value = serde_cbor::from_slice(&bytes)?;

    let mut caps = Vec::new();
    if let serde_cbor::Value::Map(ref map) = cbor
        && let Some(serde_cbor::Value::Array(arr)) =
            map.get(&serde_cbor::Value::Text("capabilities".to_string()))
    {
        for item in arr {
            if let serde_cbor::Value::Map(cap_map) = item
                && let Some(serde_cbor::Value::Text(name)) =
                    cap_map.get(&serde_cbor::Value::Text("name".to_string()))
            {
                caps.push(name.clone());
            }
        }
    }
    Ok(caps)
}

/// Read dependencies from a gtpack manifest.
/// Returns Vec of (pack_id, required_capabilities).
fn read_pack_dependencies(pack_path: &Path) -> anyhow::Result<Vec<(String, Vec<String>)>> {
    let file = std::fs::File::open(pack_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut entry = archive.by_name("manifest.cbor")?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    let cbor: serde_cbor::Value = serde_cbor::from_slice(&bytes)?;

    let mut deps = Vec::new();
    if let serde_cbor::Value::Map(ref map) = cbor
        && let Some(serde_cbor::Value::Array(arr)) =
            map.get(&serde_cbor::Value::Text("dependencies".to_string()))
    {
        for item in arr {
            if let serde_cbor::Value::Map(dep_map) = item {
                let pack_id = dep_map
                    .get(&serde_cbor::Value::Text("pack_id".to_string()))
                    .and_then(|v| {
                        if let serde_cbor::Value::Text(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                let req_caps: Vec<String> = dep_map
                    .get(&serde_cbor::Value::Text(
                        "required_capabilities".to_string(),
                    ))
                    .and_then(|v| {
                        if let serde_cbor::Value::Array(arr) = v {
                            Some(
                                arr.iter()
                                    .filter_map(|item| {
                                        if let serde_cbor::Value::Text(s) = item {
                                            Some(s.clone())
                                        } else {
                                            None
                                        }
                                    })
                                    .collect(),
                            )
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                if !pack_id.is_empty() && !req_caps.is_empty() {
                    deps.push((pack_id, req_caps));
                }
            }
        }
    }
    Ok(deps)
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

    #[test]
    fn find_replacement_from_messaging_providers_build_dir() {
        let root = tempfile::tempdir().unwrap();
        let project_root = root.path().join("workspace");
        let bundle = project_root.join("bundle");
        std::fs::create_dir_all(bundle.join("providers").join("messaging")).unwrap();
        write_test_gtpack(
            &bundle
                .join("providers")
                .join("messaging")
                .join("messaging-test.gtpack"),
            false,
        );

        // greentic-messaging-providers/target/packs at workspace root
        let build_dir = project_root
            .join("greentic-messaging-providers")
            .join("target")
            .join("packs");
        std::fs::create_dir_all(&build_dir).unwrap();
        write_test_gtpack(&build_dir.join("messaging-test.gtpack"), true);

        let replacement = find_replacement_pack("messaging-test.gtpack", &bundle, "messaging")
            .expect("replacement should be found in build output");
        assert!(replacement.ends_with("messaging-test.gtpack"));
    }

    #[test]
    fn validate_and_upgrade_packs_reports_zero_for_empty_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let report = validate_and_upgrade_packs(dir.path()).unwrap();
        assert_eq!(report.checked, 0);
        assert!(report.upgraded.is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn validate_and_upgrade_packs_skips_packs_with_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let providers = dir.path().join("providers").join("messaging");
        std::fs::create_dir_all(&providers).unwrap();
        write_test_gtpack(&providers.join("good.gtpack"), true);

        let report = validate_and_upgrade_packs(dir.path()).unwrap();
        assert_eq!(report.checked, 1);
        assert!(report.upgraded.is_empty());
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn validate_and_upgrade_packs_warns_when_no_replacement_available() {
        let dir = tempfile::tempdir().unwrap();
        let providers = dir.path().join("providers").join("messaging");
        std::fs::create_dir_all(&providers).unwrap();
        write_test_gtpack(&providers.join("legacy.gtpack"), false);

        let report = validate_and_upgrade_packs(dir.path()).unwrap();
        assert_eq!(report.checked, 1);
        assert!(report.upgraded.is_empty());
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].message.contains("legacy.gtpack"));
    }

    #[test]
    fn validate_and_upgrade_packs_upgrades_from_sibling_bundle() {
        let root = tempfile::tempdir().unwrap();

        // Bundle A: legacy pack without capabilities
        let bundle_a = root.path().join("bundle-a");
        let providers_a = bundle_a.join("providers").join("messaging");
        std::fs::create_dir_all(&providers_a).unwrap();
        let pack_a = providers_a.join("messaging-test.gtpack");
        write_test_gtpack(&pack_a, false);

        // Bundle B: same pack name with capabilities
        let bundle_b = root.path().join("bundle-b");
        let providers_b = bundle_b.join("providers").join("messaging");
        std::fs::create_dir_all(&providers_b).unwrap();
        write_test_gtpack(&providers_b.join("messaging-test.gtpack"), true);

        let report = validate_and_upgrade_packs(&bundle_a).unwrap();
        assert_eq!(report.checked, 1);
        assert_eq!(report.upgraded.len(), 1);
        assert!(report.warnings.is_empty());
        // Backup file was written next to the original pack.
        assert!(pack_a.with_extension("gtpack.bak").exists());
        // Original pack now has capabilities.
        assert!(has_capabilities_extension(&pack_a));
    }

    /// Write a gtpack with explicit `capabilities` and `dependencies` arrays.
    fn write_pack_with_capabilities_and_deps(
        path: &Path,
        pack_id: &str,
        capabilities: &[&str],
        dependencies: &[(&str, &[&str])],
    ) {
        let mut map = BTreeMap::new();
        map.insert(
            CV::Text("schema_version".into()),
            CV::Text("pack-v1".into()),
        );
        map.insert(CV::Text("pack_id".into()), CV::Text(pack_id.into()));
        map.insert(CV::Text("version".into()), CV::Text("0.1.0".into()));
        map.insert(CV::Text("kind".into()), CV::Text("provider".into()));

        let cap_array: Vec<CV> = capabilities
            .iter()
            .map(|name| {
                let mut cap = BTreeMap::new();
                cap.insert(CV::Text("name".into()), CV::Text((*name).into()));
                CV::Map(cap)
            })
            .collect();
        map.insert(CV::Text("capabilities".into()), CV::Array(cap_array));

        let dep_array: Vec<CV> = dependencies
            .iter()
            .map(|(dep_pack_id, required)| {
                let mut dep = BTreeMap::new();
                dep.insert(CV::Text("pack_id".into()), CV::Text((*dep_pack_id).into()));
                dep.insert(
                    CV::Text("required_capabilities".into()),
                    CV::Array(
                        required
                            .iter()
                            .map(|name| CV::Text((*name).into()))
                            .collect(),
                    ),
                );
                CV::Map(dep)
            })
            .collect();
        map.insert(CV::Text("dependencies".into()), CV::Array(dep_array));

        let bytes = serde_cbor::to_vec(&CV::Map(map)).expect("encode cbor");
        let file = File::create(path).expect("create file");
        let mut zip = ZipWriter::new(file);
        zip.start_file("manifest.cbor", FileOptions::<()>::default())
            .expect("start file");
        zip.write_all(&bytes).expect("write manifest");
        zip.finish().expect("finish zip");
    }

    #[test]
    fn validate_dependency_capabilities_marks_satisfied_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let providers = dir.path().join("providers").join("messaging");
        std::fs::create_dir_all(&providers).unwrap();

        // Provider A offers `cap.A`. Provider B requires `cap.A` (satisfied)
        // and `cap.MISSING` (not provided by anyone).
        write_pack_with_capabilities_and_deps(
            &providers.join("provider-a.gtpack"),
            "provider-a",
            &["cap.A"],
            &[],
        );
        write_pack_with_capabilities_and_deps(
            &providers.join("provider-b.gtpack"),
            "provider-b",
            &[],
            &[("provider-z", &["cap.A", "cap.MISSING"])],
        );

        let report = validate_dependency_capabilities(dir.path()).unwrap();
        assert!(
            report
                .satisfied
                .iter()
                .any(|s| s.capability == "cap.A" && s.provided_by == "provider-a")
        );
        assert!(report.missing.iter().any(|m| m.capability == "cap.MISSING"));
    }

    #[test]
    fn validate_dependency_capabilities_skips_dependency_present_in_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let providers = dir.path().join("providers").join("messaging");
        std::fs::create_dir_all(&providers).unwrap();

        // Pack X declares a dependency on pack-id "provider-y" — and provider-y
        // is present in the bundle. The dep should be skipped entirely.
        write_pack_with_capabilities_and_deps(
            &providers.join("provider-x.gtpack"),
            "provider-x",
            &[],
            &[("provider-y", &["whatever.cap"])],
        );
        write_pack_with_capabilities_and_deps(
            &providers.join("provider-y.gtpack"),
            "provider-y",
            &[],
            &[],
        );

        let report = validate_dependency_capabilities(dir.path()).unwrap();
        assert!(report.satisfied.is_empty());
        assert!(report.missing.is_empty());
    }

    #[test]
    fn validate_dependency_capabilities_returns_empty_for_empty_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let report = validate_dependency_capabilities(dir.path()).unwrap();
        assert!(report.satisfied.is_empty());
        assert!(report.missing.is_empty());
    }
}
