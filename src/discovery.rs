//! Pack discovery — scans a bundle directory for `.gtpack` files across
//! provider domains (messaging, events, oauth) and extracts metadata.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_cbor::Value as CborValue;
use zip::result::ZipError;

/// Result of discovering packs in a bundle.
#[derive(Clone, Debug, Serialize)]
pub struct DiscoveryResult {
    pub domains: DetectedDomains,
    pub providers: Vec<DetectedProvider>,
}

/// Flags indicating which domains are present in the bundle.
#[derive(Clone, Debug, Serialize)]
pub struct DetectedDomains {
    pub messaging: bool,
    pub events: bool,
    pub oauth: bool,
}

/// Metadata for a discovered provider pack.
#[derive(Clone, Debug, Serialize)]
pub struct DetectedProvider {
    pub provider_id: String,
    pub domain: String,
    pub pack_path: PathBuf,
    pub id_source: ProviderIdSource,
}

/// How the provider ID was determined.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderIdSource {
    Manifest,
    Filename,
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

            let (provider_id, id_source) = if options.cbor_only {
                match read_pack_id_cbor_only(&path)? {
                    Some(id) => (id, ProviderIdSource::Manifest),
                    None => return Err(missing_cbor_error(&path)),
                }
            } else {
                match read_pack_id_from_manifest(&path)? {
                    Some(id) => (id, ProviderIdSource::Manifest),
                    None => {
                        let stem = path
                            .file_stem()
                            .and_then(|v| v.to_str())
                            .unwrap_or_default()
                            .to_string();
                        (stem, ProviderIdSource::Filename)
                    }
                }
            };

            providers.push(DetectedProvider {
                provider_id,
                domain: domain.to_string(),
                pack_path: path,
                id_source,
            });
        }
    }

    providers.sort_by(|a, b| a.pack_path.cmp(&b.pack_path));

    let domains = DetectedDomains {
        messaging: providers.iter().any(|p| p.domain == "messaging"),
        events: providers.iter().any(|p| p.domain == "events"),
        oauth: providers.iter().any(|p| p.domain == "oauth"),
    };

    Ok(DiscoveryResult { domains, providers })
}

/// Persist discovery results to JSON files in the bundle's runtime state directory.
pub fn persist(root: &Path, tenant: &str, discovery: &DiscoveryResult) -> anyhow::Result<()> {
    let runtime_root = root.join("state").join("runtime").join(tenant);
    std::fs::create_dir_all(&runtime_root)?;
    let domains_path = runtime_root.join("detected_domains.json");
    let providers_path = runtime_root.join("detected_providers.json");
    write_json(&domains_path, &discovery.domains)?;
    write_json(&providers_path, &discovery.providers)?;
    Ok(())
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

fn read_pack_id_from_manifest(path: &Path) -> anyhow::Result<Option<String>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    if let Some(id) = read_manifest_cbor(&mut archive)? {
        return Ok(Some(id));
    }
    if let Some(id) = read_manifest_json(&mut archive, "pack.manifest.json")? {
        return Ok(Some(id));
    }
    Ok(None)
}

fn read_pack_id_cbor_only(path: &Path) -> anyhow::Result<Option<String>> {
    let file = std::fs::File::open(path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    read_manifest_cbor(&mut archive)
}

fn read_manifest_cbor(
    archive: &mut zip::ZipArchive<std::fs::File>,
) -> anyhow::Result<Option<String>> {
    let mut file = match archive.by_name("manifest.cbor") {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)?;
    let value: CborValue = serde_cbor::from_slice(&bytes)?;
    extract_pack_id_from_cbor(&value)
}

fn read_manifest_json(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> anyhow::Result<Option<String>> {
    let mut file = match archive.by_name(name) {
        Ok(file) => file,
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut file, &mut contents)?;
    let parsed: serde_json::Value = serde_json::from_str(&contents)?;

    if let Some(id) = parsed.get("pack_id").and_then(|v| v.as_str()) {
        return Ok(Some(id.to_string()));
    }
    if let Some(meta) = parsed.get("meta")
        && let Some(id) = meta.get("pack_id").and_then(|v| v.as_str())
    {
        return Ok(Some(id.to_string()));
    }
    Ok(None)
}

fn extract_pack_id_from_cbor(value: &CborValue) -> anyhow::Result<Option<String>> {
    let CborValue::Map(map) = value else {
        return Ok(None);
    };
    let symbols = match map_get(map, "symbols") {
        Some(CborValue::Map(map)) => Some(map),
        _ => None,
    };

    if let Some(pack_id) = map_get(map, "pack_id")
        && let Some(value) = resolve_string_symbol(pack_id, symbols, "pack_ids")?
    {
        return Ok(Some(value));
    }

    if let Some(CborValue::Map(meta)) = map_get(map, "meta")
        && let Some(pack_id) = map_get(meta, "pack_id")
        && let Some(value) = resolve_string_symbol(pack_id, symbols, "pack_ids")?
    {
        return Ok(Some(value));
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

fn missing_cbor_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "demo packs must be CBOR-only (.gtpack must contain manifest.cbor). \
         Rebuild the pack with greentic-pack build (do not use --dev). Missing in {}",
        path.display()
    )
}
