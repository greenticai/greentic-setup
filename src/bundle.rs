//! Bundle directory structure creation and management.
//!
//! Handles creating the demo bundle scaffold, writing configuration files,
//! and managing tenant/team directories.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml_bw::{Mapping as YamlMapping, Sequence as YamlSequence, Value as YamlValue};

pub const LEGACY_BUNDLE_MARKER: &str = "greentic.demo.yaml";
pub const BUNDLE_WORKSPACE_MARKER: &str = "bundle.yaml";
pub const BUNDLE_LOCK_FILE: &str = "bundle.lock.json";

/// The bundle metadata list a pack reference should be written into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleReferenceKind {
    AppPack,
    ExtensionProvider,
}

/// One bundle dependency entry to register in `bundle.yaml` and `bundle.lock.json`.
#[derive(Clone, Debug)]
pub struct BundleReference {
    pub kind: BundleReferenceKind,
    pub reference: String,
    pub digest: Option<String>,
}

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
    write_if_missing(&root.join(LEGACY_BUNDLE_MARKER), &demo_yaml)?;
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

    // Write embedded welcome default.gtpack only when the bundle does not already
    // declare its own app packs (e.g. via wizard --answers).  When app_packs is
    // present in bundle.yaml the user has an explicit pack reference and the
    // generic welcome pack would just shadow it.
    if !bundle_has_app_packs(root) {
        write_default_pack_if_missing(root);
    }

    ensure_bundle_metadata(root, bundle_name)?;

    Ok(())
}

/// Return `true` when `bundle.yaml` already declares at least one app pack.
fn bundle_has_app_packs(bundle_root: &Path) -> bool {
    let workspace = bundle_root.join(BUNDLE_WORKSPACE_MARKER);
    let Ok(contents) = std::fs::read_to_string(&workspace) else {
        return false;
    };
    // Simple check: look for a non-empty `app_packs:` list.
    // A full YAML parse is avoided here to keep the dependency footprint minimal.
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("- packs/") || trimmed.starts_with("- ./packs/") {
            return true;
        }
    }
    false
}

/// Embedded quickstart pack bytes (built from `assets/default-welcome.gtpack`).
///
/// This pack contains an Adaptive Card menu flow (quickstart demo) using the
/// adaptive-card component with text + button routing, i18n support, and
/// Handlebars template rendering for dynamic card content.
const EMBEDDED_WELCOME_PACK: &[u8] = include_bytes!("../assets/default-welcome.gtpack");

/// Write the embedded welcome pack as `packs/default.gtpack` if not already present.
fn write_default_pack_if_missing(bundle_root: &Path) {
    let target = bundle_root.join("packs").join("default.gtpack");
    if target.exists() {
        return;
    }
    if let Err(err) = std::fs::write(&target, EMBEDDED_WELCOME_PACK) {
        eprintln!(
            "  [scaffold] WARNING: failed to write default.gtpack: {}",
            err,
        );
    } else {
        println!("  [scaffold] created default.gtpack (welcome flow)");
    }
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
    if !is_bundle_root(bundle) {
        return Err(anyhow!(
            "bundle {} missing {} or {}",
            bundle.display(),
            LEGACY_BUNDLE_MARKER,
            BUNDLE_WORKSPACE_MARKER,
        ));
    }
    Ok(())
}

pub fn is_bundle_root(bundle: &Path) -> bool {
    bundle.join(LEGACY_BUNDLE_MARKER).exists() || bundle.join(BUNDLE_WORKSPACE_MARKER).exists()
}

/// Ensure normalized bundle metadata files exist.
pub fn ensure_bundle_metadata(root: &Path, bundle_name: Option<&str>) -> anyhow::Result<()> {
    let workspace = load_bundle_workspace_doc(root, bundle_name)?;
    write_bundle_workspace_doc(root, &workspace)?;
    sync_bundle_lock_with_workspace(root, &workspace, &[])?;
    Ok(())
}

/// Register pack references in both `bundle.yaml` and `bundle.lock.json`.
pub fn register_bundle_references(
    root: &Path,
    refs: &[BundleReference],
    bundle_name: Option<&str>,
) -> anyhow::Result<()> {
    let mut workspace = load_bundle_workspace_doc(root, bundle_name)?;
    {
        let map = yaml_object_mut(&mut workspace)?;
        let mut app_packs = yaml_string_list(map, "app_packs");
        let mut extension_providers = yaml_string_list(map, "extension_providers");

        for entry in refs {
            match entry.kind {
                BundleReferenceKind::AppPack => app_packs.push(entry.reference.clone()),
                BundleReferenceKind::ExtensionProvider => {
                    extension_providers.push(entry.reference.clone())
                }
            }
        }

        sort_unique_strings(&mut app_packs);
        sort_unique_strings(&mut extension_providers);
        yaml_set_string_list(map, "app_packs", &app_packs);
        yaml_set_string_list(map, "extension_providers", &extension_providers);
    }

    prune_scaffold_default_pack(root, &workspace)?;
    write_bundle_workspace_doc(root, &workspace)?;
    sync_bundle_lock_with_workspace(root, &workspace, refs)?;
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

fn load_bundle_workspace_doc(root: &Path, bundle_name: Option<&str>) -> anyhow::Result<YamlValue> {
    let path = root.join(BUNDLE_WORKSPACE_MARKER);
    let mut doc = if path.exists() {
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_yaml_bw::from_str::<YamlValue>(&raw)
            .with_context(|| format!("parse {}", path.display()))?
    } else {
        YamlValue::Mapping(YamlMapping::new())
    };

    let bundle_id = infer_bundle_id(root);
    let bundle_name = bundle_name
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| infer_bundle_name(root));

    let map = yaml_object_mut(&mut doc)?;
    yaml_set_default(map, "schema_version", YamlValue::Number(1.into(), None));
    yaml_set_default(map, "bundle_id", yaml_string(bundle_id.clone()));
    yaml_set_default(map, "bundle_name", yaml_string(bundle_name));
    yaml_set_default(map, "locale", yaml_string("en"));
    yaml_set_default(map, "mode", yaml_string("create"));
    yaml_set_default(map, "advanced_setup", YamlValue::Bool(false, None));
    yaml_set_default(map, "app_packs", YamlValue::Sequence(YamlSequence::new()));
    yaml_set_default(
        map,
        "app_pack_mappings",
        YamlValue::Sequence(YamlSequence::new()),
    );
    yaml_set_default(
        map,
        "extension_providers",
        YamlValue::Sequence(YamlSequence::new()),
    );
    yaml_set_default(
        map,
        "remote_catalogs",
        YamlValue::Sequence(YamlSequence::new()),
    );
    yaml_set_default(map, "hooks", YamlValue::Sequence(YamlSequence::new()));
    yaml_set_default(
        map,
        "subscriptions",
        YamlValue::Sequence(YamlSequence::new()),
    );
    yaml_set_default(
        map,
        "capabilities",
        YamlValue::Sequence(YamlSequence::new()),
    );
    yaml_set_default(map, "setup_execution_intent", YamlValue::Bool(false, None));
    yaml_set_default(map, "export_intent", YamlValue::Bool(false, None));
    Ok(doc)
}

fn write_bundle_workspace_doc(root: &Path, doc: &YamlValue) -> anyhow::Result<()> {
    let path = root.join(BUNDLE_WORKSPACE_MARKER);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut rendered =
        serde_yaml_bw::to_string(doc).with_context(|| format!("serialize {}", path.display()))?;
    if let Some(stripped) = rendered.strip_prefix("---\n") {
        rendered = stripped.to_string();
    }
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    std::fs::write(&path, rendered).with_context(|| format!("write {}", path.display()))
}

fn sync_bundle_lock_with_workspace(
    root: &Path,
    workspace: &YamlValue,
    updated_refs: &[BundleReference],
) -> anyhow::Result<()> {
    let path = root.join(BUNDLE_LOCK_FILE);
    let mut doc = if path.exists() {
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str::<JsonValue>(&raw)
            .with_context(|| format!("parse {}", path.display()))?
    } else {
        JsonValue::Object(JsonMap::new())
    };

    let workspace_map = workspace
        .as_mapping()
        .ok_or_else(|| anyhow!("bundle workspace must be a YAML object"))?;
    let bundle_id =
        yaml_get_string(workspace_map, "bundle_id").unwrap_or_else(|| infer_bundle_id(root));
    let mode = yaml_get_string(workspace_map, "mode").unwrap_or_else(|| "create".to_string());
    let app_packs = yaml_string_list(workspace_map, "app_packs");
    let extension_providers = yaml_string_list(workspace_map, "extension_providers");

    let obj = json_object_mut(&mut doc)?;
    json_set_default(obj, "schema_version", JsonValue::from(1));
    json_set_default(obj, "bundle_id", JsonValue::String(bundle_id));
    json_set_default(obj, "requested_mode", JsonValue::String(mode));
    json_set_default(obj, "execution", JsonValue::String("execute".to_string()));
    json_set_default(
        obj,
        "cache_policy",
        JsonValue::String("workspace-local".to_string()),
    );
    obj.insert(
        "tool_version".to_string(),
        JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
    );
    json_set_default(
        obj,
        "build_format_version",
        JsonValue::String("bundle-lock-v1".to_string()),
    );
    obj.insert(
        "workspace_root".to_string(),
        JsonValue::String(BUNDLE_WORKSPACE_MARKER.to_string()),
    );
    obj.insert(
        "lock_file".to_string(),
        JsonValue::String(BUNDLE_LOCK_FILE.to_string()),
    );
    json_set_default(obj, "catalogs", JsonValue::Array(Vec::new()));
    json_set_default(obj, "setup_state_files", JsonValue::Array(Vec::new()));

    let digests_by_ref: std::collections::BTreeMap<String, Option<String>> = updated_refs
        .iter()
        .map(|entry| (entry.reference.clone(), entry.digest.clone()))
        .collect();
    json_set_dependency_locks(obj, "app_packs", &app_packs, &digests_by_ref);
    json_set_dependency_locks(
        obj,
        "extension_providers",
        &extension_providers,
        &digests_by_ref,
    );

    let payload = serde_json::to_string_pretty(&doc)
        .with_context(|| format!("serialize {}", path.display()))?;
    std::fs::write(&path, payload).with_context(|| format!("write {}", path.display()))
}

fn prune_scaffold_default_pack(root: &Path, workspace: &YamlValue) -> anyhow::Result<()> {
    let Some(workspace_map) = workspace.as_mapping() else {
        return Ok(());
    };
    let app_packs = yaml_string_list(workspace_map, "app_packs");
    let has_explicit_non_default = app_packs
        .iter()
        .any(|entry| !entry.ends_with("default.gtpack"));
    if !has_explicit_non_default {
        return Ok(());
    }

    let default_pack = root.join("packs").join("default.gtpack");
    if !default_pack.exists() {
        return Ok(());
    }

    let contents =
        std::fs::read(&default_pack).with_context(|| format!("read {}", default_pack.display()))?;
    if contents == EMBEDDED_WELCOME_PACK {
        std::fs::remove_file(&default_pack)
            .with_context(|| format!("remove {}", default_pack.display()))?;
    }
    Ok(())
}

fn infer_bundle_id(root: &Path) -> String {
    root.file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "bundle".to_string())
}

fn infer_bundle_name(root: &Path) -> String {
    infer_bundle_id(root)
}

fn yaml_object_mut(value: &mut YamlValue) -> anyhow::Result<&mut YamlMapping> {
    if !matches!(value, YamlValue::Mapping(_)) {
        *value = YamlValue::Mapping(YamlMapping::new());
    }
    match value {
        YamlValue::Mapping(map) => Ok(map),
        _ => unreachable!(),
    }
}

fn yaml_set_default(map: &mut YamlMapping, key: &str, value: YamlValue) {
    let key_value = yaml_string(key);
    if !map.contains_key(&key_value) {
        map.insert(key_value, value);
    }
}

fn yaml_get_string(map: &YamlMapping, key: &str) -> Option<String> {
    map.get(yaml_string(key))
        .and_then(YamlValue::as_str)
        .map(ToOwned::to_owned)
}

fn yaml_string_list(map: &YamlMapping, key: &str) -> Vec<String> {
    map.get(yaml_string(key))
        .and_then(YamlValue::as_sequence)
        .map(|values| {
            values
                .iter()
                .filter_map(YamlValue::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn yaml_set_string_list(map: &mut YamlMapping, key: &str, values: &[String]) {
    let mut sequence = YamlSequence::new();
    for value in values {
        sequence.push(yaml_string(value.clone()));
    }
    map.insert(yaml_string(key), YamlValue::Sequence(sequence));
}

fn yaml_string(value: impl Into<String>) -> YamlValue {
    YamlValue::String(value.into(), None)
}

fn sort_unique_strings(values: &mut Vec<String>) {
    values.retain(|value| !value.trim().is_empty());
    values.sort();
    values.dedup();
}

fn json_object_mut(value: &mut JsonValue) -> anyhow::Result<&mut JsonMap<String, JsonValue>> {
    if !matches!(value, JsonValue::Object(_)) {
        *value = JsonValue::Object(JsonMap::new());
    }
    match value {
        JsonValue::Object(map) => Ok(map),
        _ => unreachable!(),
    }
}

fn json_set_default(map: &mut JsonMap<String, JsonValue>, key: &str, value: JsonValue) {
    map.entry(key.to_string()).or_insert(value);
}

fn json_set_dependency_locks(
    map: &mut JsonMap<String, JsonValue>,
    key: &str,
    references: &[String],
    updated_digests: &std::collections::BTreeMap<String, Option<String>>,
) {
    let existing_digests: std::collections::BTreeMap<String, Option<String>> = map
        .get(key)
        .and_then(JsonValue::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let obj = entry.as_object()?;
                    let reference = obj.get("reference")?.as_str()?.to_string();
                    let digest = obj
                        .get("digest")
                        .and_then(JsonValue::as_str)
                        .map(ToOwned::to_owned);
                    Some((reference, digest))
                })
                .collect()
        })
        .unwrap_or_default();

    let entries = references
        .iter()
        .map(|reference| {
            let digest = updated_digests
                .get(reference)
                .cloned()
                .unwrap_or_else(|| existing_digests.get(reference).cloned().unwrap_or(None));
            serde_json::json!({
                "reference": reference,
                "digest": digest,
            })
        })
        .collect::<Vec<_>>();
    map.insert(key.to_string(), JsonValue::Array(entries));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::execute_add_packs_to_bundle;
    use crate::plan::ResolvedPackInfo;
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    fn write_pack(path: &Path, pack_id: &str) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options).unwrap();
        writer
            .write_all(
                serde_json::json!({
                    "pack_id": pack_id,
                    "display_name": pack_id,
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn create_bundle_structure() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo-bundle");
        create_demo_bundle_structure(&root, Some("test")).unwrap();
        assert!(root.join(LEGACY_BUNDLE_MARKER).exists());
        assert!(root.join("providers/messaging").exists());
        assert!(root.join("tenants/demo/teams/default/team.gmap").exists());
    }

    #[test]
    fn embedded_welcome_pack_written_when_no_sibling() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("new-bundle");
        create_demo_bundle_structure(&root, Some("test")).unwrap();
        let pack = root.join("packs").join("default.gtpack");
        assert!(pack.exists(), "embedded welcome pack should be written");
        assert!(
            pack.metadata().unwrap().len() > 1000,
            "pack should not be empty"
        );
    }

    #[test]
    fn embedded_welcome_pack_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("existing-bundle");
        std::fs::create_dir_all(root.join("packs")).unwrap();
        std::fs::write(root.join("packs").join("default.gtpack"), b"custom").unwrap();
        create_demo_bundle_structure(&root, Some("test")).unwrap();
        let contents = std::fs::read(root.join("packs").join("default.gtpack")).unwrap();
        assert_eq!(
            contents, b"custom",
            "existing pack should not be overwritten"
        );
    }

    #[test]
    fn default_pack_skipped_when_bundle_has_app_packs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("custom-bundle");
        std::fs::create_dir_all(root.join("packs")).unwrap();
        // Write a bundle.yaml that declares an app pack
        std::fs::write(
            root.join(BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\napp_packs:\n  - packs/my-flow.pack\n",
        )
        .unwrap();
        create_demo_bundle_structure(&root, Some("test")).unwrap();
        assert!(
            !root.join("packs").join("default.gtpack").exists(),
            "default.gtpack should NOT be created when app_packs are declared"
        );
    }

    #[test]
    fn validate_bundle_exists_fails_for_missing() {
        let result = validate_bundle_exists(Path::new("/nonexistent"));
        assert!(result.is_err());
    }

    #[test]
    fn validate_bundle_exists_accepts_bundle_yaml_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle-workspace");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(BUNDLE_WORKSPACE_MARKER), "schema_version: 1\n").unwrap();

        validate_bundle_exists(&root).unwrap();
        assert!(is_bundle_root(&root));
    }

    #[test]
    fn add_packs_updates_bundle_workspace_and_lock() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bundle-workspace");
        create_demo_bundle_structure(&root, Some("weather-demo")).unwrap();

        let source_dir = temp.path().join("src-packs");
        std::fs::create_dir_all(&source_dir).unwrap();
        let app_pack = source_dir.join("weather-app.gtpack");
        let provider_pack = source_dir.join("messaging-telegram.gtpack");
        write_pack(&app_pack, "weather-app");
        write_pack(&provider_pack, "messaging-telegram");

        execute_add_packs_to_bundle(
            &root,
            &[
                ResolvedPackInfo {
                    source_ref: app_pack.display().to_string(),
                    mapped_ref: app_pack.display().to_string(),
                    resolved_digest: "sha256:app".to_string(),
                    pack_id: "weather-app".to_string(),
                    entry_flows: Vec::new(),
                    cached_path: app_pack.clone(),
                    output_path: app_pack.clone(),
                },
                ResolvedPackInfo {
                    source_ref: provider_pack.display().to_string(),
                    mapped_ref: provider_pack.display().to_string(),
                    resolved_digest: "sha256:provider".to_string(),
                    pack_id: "messaging-telegram".to_string(),
                    entry_flows: Vec::new(),
                    cached_path: provider_pack.clone(),
                    output_path: provider_pack.clone(),
                },
            ],
        )
        .unwrap();

        let bundle_yaml = std::fs::read_to_string(root.join(BUNDLE_WORKSPACE_MARKER)).unwrap();
        assert!(bundle_yaml.contains("app_packs:"));
        assert!(bundle_yaml.contains("packs/weather-app.gtpack"));
        assert!(bundle_yaml.contains("extension_providers:"));
        assert!(bundle_yaml.contains("providers/messaging/messaging-telegram.gtpack"));

        let lock: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(BUNDLE_LOCK_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            lock.pointer("/app_packs/0/reference")
                .and_then(serde_json::Value::as_str),
            Some("packs/weather-app.gtpack")
        );
        assert_eq!(
            lock.pointer("/app_packs/0/digest")
                .and_then(serde_json::Value::as_str),
            Some("sha256:app")
        );
        assert_eq!(
            lock.pointer("/extension_providers/0/reference")
                .and_then(serde_json::Value::as_str),
            Some("providers/messaging/messaging-telegram.gtpack")
        );
        assert_eq!(
            lock.pointer("/extension_providers/0/digest")
                .and_then(serde_json::Value::as_str),
            Some("sha256:provider")
        );
        assert!(
            !root.join("packs").join("default.gtpack").exists(),
            "scaffold welcome pack should be removed once an explicit app pack is added"
        );
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
