//! Bundle directory structure creation and management.
//!
//! Handles creating the demo bundle scaffold, writing configuration files,
//! and managing tenant/team directories.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};

/// Create the standard demo bundle directory structure.
pub fn create_demo_bundle_structure(root: &Path, bundle_name: Option<&str>) -> anyhow::Result<()> {
    let directories = [
        "",
        "providers",
        "providers/messaging",
        "providers/events",
        "providers/secrets",
        "providers/oauth",
        "packs",
        "resolved",
        "state",
        "state/resolved",
        "state/runs",
        "state/pids",
        "state/logs",
        "state/runtime",
        "state/doctor",
        "tenants",
        "tenants/default",
        "tenants/default/teams",
        "tenants/demo",
        "tenants/demo/teams",
        "tenants/demo/teams/default",
        "logs",
    ];
    for directory in directories {
        std::fs::create_dir_all(root.join(directory))?;
    }

    let mut demo_yaml = "version: \"1\"\nproject_root: \"./\"\n".to_string();
    if let Some(name) = bundle_name.filter(|v| !v.trim().is_empty()) {
        demo_yaml.push_str(&format!("bundle_name: \"{}\"\n", name.replace('"', "")));
    }
    write_if_missing(&root.join("greentic.demo.yaml"), &demo_yaml)?;
    write_if_missing(
        &root.join("tenants").join("default").join("tenant.gmap"),
        "_ = forbidden\n",
    )?;
    write_if_missing(
        &root.join("tenants").join("demo").join("tenant.gmap"),
        "_ = forbidden\n",
    )?;
    write_if_missing(
        &root
            .join("tenants")
            .join("demo")
            .join("teams")
            .join("default")
            .join("team.gmap"),
        "_ = forbidden\n",
    )?;
    Ok(())
}

/// Write a file only if it doesn't already exist.
pub fn write_if_missing(path: &Path, contents: &str) -> anyhow::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}

/// Validate that a bundle directory exists and has the expected marker file.
pub fn validate_bundle_exists(bundle: &Path) -> anyhow::Result<()> {
    if !bundle.exists() {
        return Err(anyhow!("bundle path {} does not exist", bundle.display()));
    }
    if !bundle.join("greentic.demo.yaml").exists() {
        return Err(anyhow!(
            "bundle {} missing greentic.demo.yaml",
            bundle.display()
        ));
    }
    Ok(())
}

/// Compute the gmap file path for a tenant/team in a bundle.
pub fn gmap_path(bundle: &Path, tenant: &str, team: Option<&str>) -> PathBuf {
    let mut path = bundle.join("tenants").join(tenant);
    if let Some(team) = team {
        path = path.join("teams").join(team).join("team.gmap");
    } else {
        path = path.join("tenant.gmap");
    }
    path
}

/// Compute the resolved manifest filename for a tenant/team.
pub fn resolved_manifest_filename(tenant: &str, team: Option<&str>) -> String {
    match team {
        Some(team) => format!("{tenant}.{team}.yaml"),
        None => format!("{tenant}.yaml"),
    }
}

/// Locate a provider's `.gtpack` file in the bundle by provider_id stem.
pub fn find_provider_pack_path(bundle: &Path, provider_id: &str) -> Option<PathBuf> {
    for subdir in &["providers/messaging", "providers/events", "packs"] {
        let candidate = bundle.join(subdir).join(format!("{provider_id}.gtpack"));
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Discover tenants inside the bundle.
///
/// Scans `{bundle}/tenants/` for subdirectories and files, returning
/// tenant names (directory names or file stems without extension).
///
/// If `domain` is provided, first checks `{bundle}/{domain}/tenants/`
/// and falls back to the general `{bundle}/tenants/` directory.
pub fn discover_tenants(bundle: &Path, domain: Option<&str>) -> anyhow::Result<Vec<String>> {
    // Try domain-specific tenants directory first
    if let Some(domain_name) = domain {
        let domain_dir = bundle.join(domain_name).join("tenants");
        if let Some(tenants) = read_tenants_from_dir(&domain_dir)? {
            return Ok(tenants);
        }
    }

    // Fall back to general tenants directory
    let general_dir = bundle.join("tenants");
    if let Some(tenants) = read_tenants_from_dir(&general_dir)? {
        return Ok(tenants);
    }

    Ok(Vec::new())
}

/// Read tenant names from a directory.
fn read_tenants_from_dir(dir: &Path) -> anyhow::Result<Option<Vec<String>>> {
    use std::collections::BTreeSet;

    if !dir.exists() {
        return Ok(None);
    }

    let mut tenants = BTreeSet::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|v| v.to_str()) {
                tenants.insert(name.to_string());
            }
            continue;
        }

        if path.is_file()
            && let Some(stem) = path.file_stem().and_then(|v| v.to_str())
        {
            tenants.insert(stem.to_string());
        }
    }

    Ok(Some(tenants.into_iter().collect()))
}

/// Read and parse the provider registry JSON from a bundle.
pub fn load_provider_registry(bundle: &Path) -> anyhow::Result<serde_json::Value> {
    let path = bundle.join("providers").join("providers.json");
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read provider registry {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("parse provider registry {}", path.display()))
    } else {
        Ok(serde_json::json!({ "providers": [] }))
    }
}

/// Write the provider registry JSON to a bundle.
pub fn write_provider_registry(bundle: &Path, root: &serde_json::Value) -> anyhow::Result<()> {
    let path = bundle.join("providers").join("providers.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(root)
        .with_context(|| format!("serialize provider registry {}", path.display()))?;
    std::fs::write(&path, payload).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_bundle_structure() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo-bundle");
        create_demo_bundle_structure(&root, Some("test")).unwrap();
        assert!(root.join("greentic.demo.yaml").exists());
        assert!(root.join("providers/messaging").exists());
        assert!(root.join("tenants/demo/teams/default/team.gmap").exists());
    }

    #[test]
    fn validate_bundle_exists_fails_for_missing() {
        let result = validate_bundle_exists(Path::new("/nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn gmap_paths() {
        let p = gmap_path(Path::new("/b"), "demo", None);
        assert_eq!(p, PathBuf::from("/b/tenants/demo/tenant.gmap"));

        let p = gmap_path(Path::new("/b"), "demo", Some("ops"));
        assert_eq!(p, PathBuf::from("/b/tenants/demo/teams/ops/team.gmap"));
    }

    #[test]
    fn resolved_manifest_filenames() {
        assert_eq!(resolved_manifest_filename("demo", None), "demo.yaml");
        assert_eq!(
            resolved_manifest_filename("demo", Some("ops")),
            "demo.ops.yaml"
        );
    }

    #[test]
    fn discover_tenants_reads_dirs_and_files() {
        let bundle = tempfile::tempdir().unwrap();
        let tenants_dir = bundle.path().join("tenants");
        std::fs::create_dir_all(tenants_dir.join("alpha")).unwrap();
        std::fs::write(tenants_dir.join("beta.json"), "{}").unwrap();

        let tenants = discover_tenants(bundle.path(), None).unwrap();
        assert!(tenants.contains(&"alpha".to_string()));
        assert!(tenants.contains(&"beta".to_string()));
    }

    #[test]
    fn discover_tenants_domain_specific() {
        let bundle = tempfile::tempdir().unwrap();
        let domain_dir = bundle.path().join("messaging").join("tenants");
        std::fs::create_dir_all(domain_dir.join("gamma")).unwrap();

        let tenants = discover_tenants(bundle.path(), Some("messaging")).unwrap();
        assert_eq!(tenants, vec!["gamma".to_string()]);
    }

    #[test]
    fn discover_tenants_falls_back_to_general() {
        let bundle = tempfile::tempdir().unwrap();
        let tenants_dir = bundle.path().join("tenants");
        std::fs::create_dir_all(tenants_dir.join("delta")).unwrap();

        // No domain-specific directory, should fall back
        let tenants = discover_tenants(bundle.path(), Some("events")).unwrap();
        assert_eq!(tenants, vec!["delta".to_string()]);
    }
}
