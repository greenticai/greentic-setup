//! Pack discovery — scans a bundle directory for `.gtpack` files across
//! provider domains (messaging, events, oauth) and extracts metadata.

use std::path::{Path, PathBuf};

use base64::Engine;
use serde::Serialize;
use serde_cbor::Value as CborValue;
use zip::result::ZipError;

/// Result of discovering packs in a bundle.
#[derive(Clone, Debug, Serialize)]
pub struct DiscoveryResult {
    pub domains: DetectedDomains,
    pub providers: Vec<DetectedProvider>,
    pub app_packs: Vec<DetectedProvider>,
}

/// Flags indicating which domains are present in the bundle.
#[derive(Clone, Debug, Serialize)]
pub struct DetectedDomains {
    pub messaging: bool,
    pub events: bool,
    pub oauth: bool,
    pub state: bool,
    pub secrets: bool,
}

/// Metadata for a discovered provider pack.
#[derive(Clone, Debug, Serialize)]
pub struct DetectedProvider {
    pub provider_id: String,
    pub display_name: Option<String>,
    pub domain: String,
    pub pack_path: PathBuf,
    pub id_source: ProviderIdSource,
    pub kind: DetectedPackKind,
}

/// The broad role this pack plays inside the bundle.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DetectedPackKind {
    Provider,
    App,
}

/// How the provider ID was determined.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderIdSource {
    Manifest,
    Filename,
}

/// Metadata extracted from a pack manifest.
struct PackMeta {
    pack_id: String,
    display_name: Option<String>,
}

/// Public manifest metadata helper returned by [`read_pack_meta`].
#[derive(Clone, Debug)]
pub struct DiscoveredPackMeta {
    pub pack_id: String,
    pub display_name: Option<String>,
}

/// Options for discovery.
#[derive(Default)]
pub struct DiscoveryOptions {
    /// Require CBOR manifests (no JSON fallback).
    pub cbor_only: bool,
}

/// Well-known provider domain directories.
const DOMAIN_DIRS: &[(&str, &str)] = &[
    ("messaging", "providers/messaging"),
    ("events", "providers/events"),
    ("oauth", "providers/oauth"),
    ("state", "providers/state"),
    ("secrets", "providers/secrets"),
];

/// Discover provider packs in a bundle root directory.
pub fn discover(root: &Path) -> anyhow::Result<DiscoveryResult> {
    discover_with_options(root, DiscoveryOptions::default())
}

/// Discover provider packs with custom options.
pub fn discover_with_options(
    root: &Path,
    options: DiscoveryOptions,
) -> anyhow::Result<DiscoveryResult> {
    let mut providers = Vec::new();

    for &(domain, dir) in DOMAIN_DIRS {
        let providers_dir = root.join(dir);
        if !providers_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&providers_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("gtpack") {
                continue;
            }
            let (provider_id, display_name, id_source) =
                read_pack_identity(&path, options.cbor_only)?;
            providers.push(DetectedProvider {
                provider_id,
                display_name,
                domain: domain.to_string(),
                pack_path: path,
                id_source,
                kind: DetectedPackKind::Provider,
            });
        }
    }

    // Extension providers land at `providers/<name>.gtpack[/inner.gtpack]`
    // (depth 1), outside the DOMAIN_DIRS subdirs. Catch them here.
    let providers_root = root.join("providers");
    if providers_root.exists() {
        let known_subdirs: std::collections::HashSet<&str> = DOMAIN_DIRS
            .iter()
            .filter_map(|(_, dir)| {
                std::path::Path::new(dir)
                    .file_name()
                    .and_then(|name| name.to_str())
            })
            .collect();
        for entry in std::fs::read_dir(&providers_root)? {
            let entry = entry?;
            let entry_path = entry.path();
            let name_str = entry_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();

            // Accept both `providers/<name>.gtpack` (file) and the wrapper
            // dir shape `providers/<name>.gtpack/<inner>.gtpack`.
            let pack_path = if entry.file_type()?.is_file() {
                if entry_path.extension().and_then(|e| e.to_str()) != Some("gtpack") {
                    continue;
                }
                entry_path.clone()
            } else if entry.file_type()?.is_dir() {
                if known_subdirs.contains(name_str) {
                    continue;
                }
                if !name_str.ends_with(".gtpack") {
                    continue;
                }
                let inner = entry_path.join(name_str);
                if inner.is_file() {
                    inner
                } else {
                    // Fall back to the first `.gtpack` inside.
                    match std::fs::read_dir(&entry_path)?
                        .filter_map(|e| e.ok())
                        .find(|e| e.path().extension().and_then(|x| x.to_str()) == Some("gtpack"))
                        .map(|e| e.path())
                    {
                        Some(found) => found,
                        None => continue,
                    }
                }
            } else {
                continue;
            };

            let (provider_id, display_name, id_source) =
                read_pack_identity(&pack_path, options.cbor_only)?;

            // Skip if DOMAIN_DIRS already picked up the same provider.
            if providers.iter().any(|p| p.provider_id == provider_id) {
                continue;
            }
            let domain = crate::cli_helpers::detect_domain_from_filename(
                pack_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default(),
            )
            .to_string();
            providers.push(DetectedProvider {
                provider_id,
                display_name,
                domain,
                pack_path,
                id_source,
                kind: DetectedPackKind::Provider,
            });
        }
    }

    let mut app_packs = Vec::new();
    let packs_dir = root.join("packs");
    if packs_dir.exists() {
        for entry in std::fs::read_dir(&packs_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("gtpack") {
                continue;
            }
            let (provider_id, display_name, id_source) =
                read_pack_identity(&path, options.cbor_only)?;
            app_packs.push(DetectedProvider {
                provider_id,
                display_name,
                domain: "app".to_string(),
                pack_path: path,
                id_source,
                kind: DetectedPackKind::App,
            });
        }
    }

    providers.sort_by(|a, b| a.pack_path.cmp(&b.pack_path));
    app_packs.sort_by(|a, b| a.pack_path.cmp(&b.pack_path));

    let domains = DetectedDomains {
        messaging: providers.iter().any(|p| p.domain == "messaging"),
        events: providers.iter().any(|p| p.domain == "events"),
        oauth: providers.iter().any(|p| p.domain == "oauth"),
        state: providers.iter().any(|p| p.domain == "state"),
        secrets: providers.iter().any(|p| p.domain == "secrets"),
    };

    Ok(DiscoveryResult {
        domains,
        providers,
        app_packs,
    })
}

/// Persist discovery results to JSON files in the bundle's runtime state directory.
pub fn persist(root: &Path, tenant: &str, discovery: &DiscoveryResult) -> anyhow::Result<()> {
    let runtime_root = root.join("state").join("runtime").join(tenant);
    std::fs::create_dir_all(&runtime_root)?;
    let domains_path = runtime_root.join("detected_domains.json");
    let providers_path = runtime_root.join("detected_providers.json");
    let app_packs_path = runtime_root.join("detected_app_packs.json");
    write_json(&domains_path, &discovery.domains)?;
    write_json(&providers_path, &discovery.providers)?;
    write_json(&app_packs_path, &discovery.app_packs)?;
    Ok(())
}

impl DiscoveryResult {
    /// Return every discovered pack that can participate in setup.
    pub fn setup_targets(&self) -> Vec<&DetectedProvider> {
        self.providers.iter().chain(self.app_packs.iter()).collect()
    }

    /// Find a discovered setup target by pack/provider ID.
    pub fn find_setup_target(&self, provider_id: &str) -> Option<&DetectedProvider> {
        self.providers
            .iter()
            .chain(self.app_packs.iter())
            .find(|pack| pack.provider_id == provider_id)
    }
}

/// Resolve `(provider_id, display_name, id_source)` for a `.gtpack` file,
/// using the manifest when available and falling back to the filename stem.
/// Honours `cbor_only` mode, which requires the manifest-derived metadata.
fn read_pack_identity(
    path: &Path,
    cbor_only: bool,
) -> anyhow::Result<(String, Option<String>, ProviderIdSource)> {
    if cbor_only {
        match read_pack_meta_cbor_only(path)? {
            Some(meta) => Ok((meta.pack_id, meta.display_name, ProviderIdSource::Manifest)),
            None => Err(missing_cbor_error(path)),
        }
    } else {
        match read_pack_meta_from_manifest(path)? {
            Some(meta) => Ok((meta.pack_id, meta.display_name, ProviderIdSource::Manifest)),
            None => {
                let stem = path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default()
                    .to_string();
                Ok((stem, None, ProviderIdSource::Filename))
            }
        }
    }
}

/// Read the pack ID and display name from a `.gtpack` manifest when available.
pub fn read_pack_meta(path: &Path) -> anyhow::Result<Option<DiscoveredPackMeta>> {
    read_pack_meta_from_manifest(path).map(|meta| {
        meta.map(|meta| DiscoveredPackMeta {
            pack_id: meta.pack_id,
            display_name: meta.display_name,
        })
    })
}

/// Read a top-level manifest extension value by key from a `.gtpack`.
pub fn read_pack_extension(
    path: &Path,
    extension_key: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let file = std::fs::File::open(path)?;
    match zip::ZipArchive::new(file) {
        Ok(mut archive) => {
            if let Some(value) = read_manifest_extension_cbor(&mut archive, extension_key)? {
                return Ok(Some(value));
            }
            if let Some(value) =
                read_manifest_extension_json(&mut archive, "pack.manifest.json", extension_key)?
            {
                return Ok(Some(value));
            }
        }
        Err(_) => {
            if let Some(value) = read_manifest_extension_cbor_from_tar(path, extension_key)? {
                return Ok(Some(value));
            }
        }
    }
    Ok(None)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(value)?;
    std::fs::write(path, payload)?;
    Ok(())
}

// ── Manifest reading ────────────────────────────────────────────────────────

fn read_pack_meta_from_manifest(path: &Path) -> anyhow::Result<Option<PackMeta>> {
    let file = std::fs::File::open(path)?;
    match zip::ZipArchive::new(file) {
        Ok(mut archive) => {
            if let Some(meta) = read_manifest_cbor(&mut archive)? {
                return Ok(Some(meta));
            }
            if let Some(meta) = read_manifest_json(&mut archive, "pack.manifest.json")? {
                return Ok(Some(meta));
            }
        }
        Err(_) => {
            if let Some(meta) = read_manifest_cbor_from_tar(path)? {
                return Ok(Some(meta));
            }
        }
    }
    Ok(None)
}

fn read_pack_meta_cbor_only(path: &Path) -> anyhow::Result<Option<PackMeta>> {
    let file = std::fs::File::open(path)?;
    match zip::ZipArchive::new(file) {
        Ok(mut archive) => read_manifest_cbor(&mut archive),
        Err(_) => read_manifest_cbor_from_tar(path),
    }
}

fn read_manifest_cbor(
    archive: &mut zip::ZipArchive<std::fs::File>,
) -> anyhow::Result<Option<PackMeta>> {
    let mut file = match archive.by_name("manifest.cbor") {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    let value: CborValue = serde_cbor::from_slice(&bytes)?;
    extract_pack_meta_from_cbor(&value)
}

fn read_manifest_extension_cbor(
    archive: &mut zip::ZipArchive<std::fs::File>,
    extension_key: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let mut file = match archive.by_name("manifest.cbor") {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    let value: CborValue = serde_cbor::from_slice(&bytes)?;
    let CborValue::Map(map) = &value else {
        return Ok(None);
    };
    if let Some(value) = map_get(map, extension_key) {
        return Ok(Some(cbor_to_json(value)));
    }
    let Some(CborValue::Map(extensions)) = map_get(map, "extensions") else {
        return Ok(None);
    };
    Ok(map_get(extensions, extension_key).map(cbor_to_json))
}

fn read_manifest_json(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> anyhow::Result<Option<PackMeta>> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut file, &mut contents)?;
    let parsed: serde_json::Value = serde_json::from_str(&contents)?;

    let resolve_dn = |obj: &serde_json::Value| -> Option<String> {
        obj.get("display_name")
            .and_then(|v| v.as_str())
            .or_else(|| obj.get("name").and_then(|v| v.as_str()))
            .map(String::from)
    };

    let display_name = resolve_dn(&parsed);

    if let Some(id) = parsed.get("pack_id").and_then(|v| v.as_str()) {
        return Ok(Some(PackMeta {
            pack_id: id.to_string(),
            display_name,
        }));
    }
    if let Some(meta) = parsed.get("meta")
        && let Some(id) = meta.get("pack_id").and_then(|v| v.as_str())
    {
        let dn = resolve_dn(meta).or(display_name);
        return Ok(Some(PackMeta {
            pack_id: id.to_string(),
            display_name: dn,
        }));
    }
    Ok(None)
}

fn read_manifest_extension_json(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
    extension_key: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut file, &mut contents)?;
    let parsed: serde_json::Value = serde_json::from_str(&contents)?;
    Ok(parsed.get(extension_key).cloned().or_else(|| {
        parsed
            .get("extensions")
            .and_then(|extensions| extensions.get(extension_key))
            .cloned()
    }))
}

fn read_manifest_cbor_from_tar(path: &Path) -> anyhow::Result<Option<PackMeta>> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new("manifest.cbor") {
            continue;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes)?;
        let value: CborValue = serde_cbor::from_slice(&bytes)?;
        return extract_pack_meta_from_cbor(&value);
    }
    Ok(None)
}

fn read_manifest_extension_cbor_from_tar(
    path: &Path,
    extension_key: &str,
) -> anyhow::Result<Option<serde_json::Value>> {
    let file = std::fs::File::open(path)?;
    let mut archive = tar::Archive::new(file);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new("manifest.cbor") {
            continue;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes)?;
        let value: CborValue = serde_cbor::from_slice(&bytes)?;
        let CborValue::Map(map) = &value else {
            return Ok(None);
        };
        return Ok(map_get(map, extension_key).map(cbor_to_json));
    }
    Ok(None)
}

fn extract_pack_meta_from_cbor(value: &CborValue) -> anyhow::Result<Option<PackMeta>> {
    let CborValue::Map(map) = value else {
        return Ok(None);
    };
    let symbols = match map_get(map, "symbols") {
        Some(CborValue::Map(map)) => Some(map),
        _ => None,
    };

    let resolve_display_name =
        |source_map: &std::collections::BTreeMap<CborValue, CborValue>| -> Option<String> {
            map_get(source_map, "display_name")
                .and_then(|v| match v {
                    CborValue::Text(text) => Some(text.clone()),
                    _ => resolve_string_symbol(v, symbols, "display_names")
                        .ok()
                        .flatten(),
                })
                .or_else(|| {
                    map_get(source_map, "name").and_then(|v| match v {
                        CborValue::Text(text) => Some(text.clone()),
                        _ => resolve_string_symbol(v, symbols, "names").ok().flatten(),
                    })
                })
        };

    if let Some(pack_id) = map_get(map, "pack_id")
        && let Some(id) = resolve_string_symbol(pack_id, symbols, "pack_ids")?
    {
        return Ok(Some(PackMeta {
            pack_id: id,
            display_name: resolve_display_name(map),
        }));
    }

    if let Some(CborValue::Map(meta)) = map_get(map, "meta")
        && let Some(pack_id) = map_get(meta, "pack_id")
        && let Some(id) = resolve_string_symbol(pack_id, symbols, "pack_ids")?
    {
        return Ok(Some(PackMeta {
            pack_id: id,
            display_name: resolve_display_name(meta).or_else(|| resolve_display_name(map)),
        }));
    }

    Ok(None)
}

fn resolve_string_symbol(
    value: &CborValue,
    symbols: Option<&std::collections::BTreeMap<CborValue, CborValue>>,
    symbol_key: &str,
) -> anyhow::Result<Option<String>> {
    match value {
        CborValue::Text(text) => Ok(Some(text.clone())),
        CborValue::Integer(idx) => {
            let Some(symbols) = symbols else {
                return Ok(Some(idx.to_string()));
            };
            let Some(CborValue::Array(values)) = map_get(symbols, symbol_key)
                .or_else(|| map_get(symbols, symbol_key.strip_suffix('s').unwrap_or(symbol_key)))
            else {
                return Ok(Some(idx.to_string()));
            };
            let idx = usize::try_from(*idx).unwrap_or(usize::MAX);
            match values.get(idx) {
                Some(CborValue::Text(text)) => Ok(Some(text.clone())),
                _ => Ok(Some(idx.to_string())),
            }
        }
        _ => Ok(None),
    }
}

fn map_get<'a>(
    map: &'a std::collections::BTreeMap<CborValue, CborValue>,
    key: &str,
) -> Option<&'a CborValue> {
    map.iter().find_map(|(k, v)| match k {
        CborValue::Text(text) if text == key => Some(v),
        _ => None,
    })
}

fn cbor_to_json(value: &CborValue) -> serde_json::Value {
    match value {
        CborValue::Null => serde_json::Value::Null,
        CborValue::Bool(v) => serde_json::Value::Bool(*v),
        CborValue::Integer(v) => i64::try_from(*v)
            .map(serde_json::Number::from)
            .map(serde_json::Value::Number)
            .unwrap_or_else(|_| serde_json::Value::String(v.to_string())),
        CborValue::Float(v) => serde_json::Number::from_f64(*v)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        CborValue::Bytes(bytes) => {
            serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        CborValue::Text(text) => serde_json::Value::String(text.clone()),
        CborValue::Array(values) => {
            serde_json::Value::Array(values.iter().map(cbor_to_json).collect())
        }
        CborValue::Map(map) => {
            let mut obj = serde_json::Map::new();
            for (key, value) in map {
                let key = match key {
                    CborValue::Text(text) => text.clone(),
                    CborValue::Integer(value) => value.to_string(),
                    other => serde_json::to_string(&cbor_to_json(other)).unwrap_or_default(),
                };
                obj.insert(key, cbor_to_json(value));
            }
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

fn missing_cbor_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "demo packs must be CBOR-only (.gtpack must contain manifest.cbor). \
         Rebuild the pack with greentic-pack build (do not use --dev). Missing in {}",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    fn write_test_pack(path: &Path, pack_id: &str, display_name: &str) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": pack_id,
                "display_name": display_name,
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;
        Ok(())
    }

    fn write_test_pack_manifest(path: &Path, manifest: serde_json::Value) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(manifest.to_string().as_bytes())?;
        writer.finish()?;
        Ok(())
    }

    #[test]
    fn discover_picks_up_extension_provider_in_wrapper_dir() -> anyhow::Result<()> {
        // Layout the bundle wizard produces for extension providers (declared
        // via `extension_provider_entries`): a directory under `providers/`
        // ending in `.gtpack` that contains the actual `.gtpack` file inside.
        // Before this fix the depth-1 directory was invisible to discovery,
        // which silently dropped the provider's setup-answers page and
        // prevented secrets like `jwt_signing_key` from ever reaching the
        // dev secrets store at runtime.
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let wrapper = root.join("providers").join("messaging-webchat-gui.gtpack");
        std::fs::create_dir_all(&wrapper)?;
        let inner = wrapper.join("messaging-webchat-gui.gtpack");
        write_test_pack(&inner, "messaging-webchat-gui", "WebChat GUI")?;

        let discovered = discover(root)?;
        assert_eq!(discovered.providers.len(), 1);
        let provider = &discovered.providers[0];
        assert_eq!(provider.provider_id, "messaging-webchat-gui");
        assert_eq!(provider.domain, "messaging");
        assert_eq!(provider.kind, DetectedPackKind::Provider);
        // pack_path must resolve to the inner zip so downstream consumers
        // (setup_to_formspec::pack_to_form_spec, load_setup_spec) can open it
        // with `File::open` + `ZipArchive::new`.
        assert_eq!(provider.pack_path, inner);
        assert!(
            discovered
                .find_setup_target("messaging-webchat-gui")
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn discover_does_not_double_count_when_pack_lives_in_both_locations() -> anyhow::Result<()> {
        // Defensive: if a pack happens to be installed both under the canonical
        // `providers/<domain>/` dir AND as a wrapper dir at depth 1, we should
        // pick exactly one (the DOMAIN_DIRS pass wins because it runs first).
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        std::fs::create_dir_all(root.join("providers/messaging"))?;
        write_test_pack(
            &root
                .join("providers/messaging")
                .join("messaging-telegram.gtpack"),
            "messaging-telegram",
            "Telegram",
        )?;
        let wrapper = root.join("providers").join("messaging-telegram.gtpack");
        std::fs::create_dir_all(&wrapper)?;
        write_test_pack(
            &wrapper.join("messaging-telegram.gtpack"),
            "messaging-telegram",
            "Telegram",
        )?;

        let discovered = discover(root)?;
        // Either path is acceptable for the de-duped entry; we just must not
        // see the same provider twice.
        let matching: Vec<_> = discovered
            .providers
            .iter()
            .filter(|p| p.provider_id == "messaging-telegram")
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one entry, got {matching:?}"
        );
        Ok(())
    }

    #[test]
    fn discover_includes_app_packs_in_setup_targets() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        std::fs::create_dir_all(root.join("providers/messaging"))?;
        std::fs::create_dir_all(root.join("packs"))?;

        write_test_pack(
            &root
                .join("providers")
                .join("messaging")
                .join("messaging-telegram.gtpack"),
            "messaging-telegram",
            "Telegram",
        )?;
        write_test_pack(
            &root.join("packs").join("weather-app.gtpack"),
            "weather-app",
            "Weather App",
        )?;

        let discovered = discover(root)?;
        assert_eq!(discovered.providers.len(), 1);
        assert_eq!(discovered.app_packs.len(), 1);
        assert_eq!(discovered.setup_targets().len(), 2);
        assert_eq!(discovered.app_packs[0].provider_id, "weather-app");
        assert_eq!(discovered.app_packs[0].domain, "app");
        assert_eq!(discovered.app_packs[0].kind, DetectedPackKind::App);
        Ok(())
    }

    #[test]
    fn read_pack_extension_reads_json_manifest_extension() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-example.gtpack");
        write_test_pack_manifest(
            &pack,
            serde_json::json!({
                "pack_id": "messaging-example",
                "extensions": {
                    "messaging.oauth.v1": {
                        "token_url": "https://example.com/token",
                        "secret_keys": ["EXAMPLE_TOKEN"]
                    }
                }
            }),
        )?;
        let extension = read_pack_extension(&pack, "messaging.oauth.v1")?.unwrap();
        assert_eq!(extension["token_url"], "https://example.com/token");
        Ok(())
    }

    #[test]
    fn read_pack_extension_reads_cbor_manifest_extensions_map() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-example.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = zip::ZipWriter::new(file);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("manifest.cbor", options)?;
        let manifest = serde_cbor::value::to_value(serde_json::json!({
            "pack_id": "messaging-example",
            "extensions": {
                "messaging.oauth.v1": {
                    "inline": {
                        "token_url": "https://example.com/token"
                    }
                }
            }
        }))?;
        writer.write_all(&serde_cbor::to_vec(&manifest)?)?;
        writer.finish()?;

        let extension = read_pack_extension(&pack, "messaging.oauth.v1")?.unwrap();
        assert_eq!(
            extension["inline"]["token_url"],
            "https://example.com/token"
        );
        Ok(())
    }
}
