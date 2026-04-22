//! Dev secrets store management for bundle setup.
//!
//! Provides helpers for locating the dev secrets file and
//! [`SecretsSetup`] for ensuring pack secrets are seeded.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use greentic_secrets_lib::core::Error as SecretError;
use greentic_secrets_lib::{
    ApplyOptions, DevStore, SecretFormat, SecretsStore, SeedDoc, SeedEntry, SeedValue, apply_seed,
};
use serde_cbor::Value as CborValue;
use tracing::{debug, info};

use crate::canonical_secret_uri;

// ── Dev store path helpers ──────────────────────────────────────────────────

const STORE_RELATIVE: &str = ".greentic/dev/.dev.secrets.env";
const STORE_STATE_RELATIVE: &str = ".greentic/state/dev/.dev.secrets.env";
const OVERRIDE_ENV: &str = "GREENTIC_DEV_SECRETS_PATH";

/// Returns a path explicitly configured via `$GREENTIC_DEV_SECRETS_PATH`.
pub fn override_path() -> Option<PathBuf> {
    std::env::var(OVERRIDE_ENV).ok().map(PathBuf::from)
}

/// Checks for an existing dev store inside the bundle root.
pub fn find_existing(bundle_root: &Path) -> Option<PathBuf> {
    find_existing_with_override(bundle_root, override_path().as_deref())
}

/// Looks for an existing dev store using an override path before consulting default candidates.
pub fn find_existing_with_override(
    bundle_root: &Path,
    override_path: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = override_path
        && path.exists()
    {
        return Some(path.to_path_buf());
    }
    candidate_paths(bundle_root)
        .into_iter()
        .find(|candidate| candidate.exists())
}

/// Ensures the default dev store path exists (creating parent directories) before returning it.
pub fn ensure_path(bundle_root: &Path) -> Result<PathBuf> {
    if let Some(path) = override_path() {
        ensure_parent(&path)?;
        return Ok(path);
    }
    let path = bundle_root.join(STORE_RELATIVE);
    ensure_parent(&path)?;
    Ok(path)
}

/// Returns the default dev store path without creating anything.
pub fn default_path(bundle_root: &Path) -> PathBuf {
    bundle_root.join(STORE_RELATIVE)
}

fn candidate_paths(bundle_root: &Path) -> [PathBuf; 2] {
    [
        bundle_root.join(STORE_RELATIVE),
        bundle_root.join(STORE_STATE_RELATIVE),
    ]
}

fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

// ── SecretsSetup ────────────────────────────────────────────────────────────

/// Single entry-point for secrets initialization and resolution.
///
/// Opens exactly one dev store per instance and ensures every required secret
/// discovered from packs is canonicalized and registered.
pub struct SecretsSetup {
    store: DevStore,
    store_path: PathBuf,
    env: String,
    tenant: String,
    team: Option<String>,
    seeds: HashMap<String, SeedEntry>,
}

impl SecretsSetup {
    pub fn new(bundle_root: &Path, env: &str, tenant: &str, team: Option<&str>) -> Result<Self> {
        let store_path = ensure_path(bundle_root)?;
        let store = if let Some(provider) = GLOBAL_KEY_PROVIDER.get() {
            let allow = ALLOW_DOWNGRADE.get().copied().unwrap_or(false);
            info!(path = %store_path.display(), "secrets: using encrypted dev store backend");
            DevStore::with_path_encrypted(&store_path, provider.clone(), allow).map_err(|err| {
                anyhow!(
                    "failed to open encrypted dev secrets store {}: {err}",
                    store_path.display()
                )
            })?
        } else {
            info!(path = %store_path.display(), "secrets: using legacy dev store backend");
            DevStore::with_path(&store_path).map_err(|err| {
                anyhow!(
                    "failed to open dev secrets store {}: {err}",
                    store_path.display()
                )
            })?
        };
        let seeds = load_seed_entries(bundle_root)?;
        Ok(Self {
            store,
            store_path,
            env: env.to_string(),
            tenant: tenant.to_string(),
            team: team.map(|v| v.to_string()),
            seeds,
        })
    }

    /// Path to the dev store file on disk.
    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    /// Reference to the underlying `DevStore`.
    pub fn store(&self) -> &DevStore {
        &self.store
    }

    /// Resolve required-but-missing secret keys for a pack without
    /// seeding placeholders. The caller (typically the setup CLI) is
    /// expected to prompt the user for each returned `MissingKey` and
    /// then call [`SecretsSetup::set_secret_text`] to populate it.
    ///
    /// Keys that resolve via `seeds.yaml` are still seeded
    /// transparently — they do NOT appear in the returned vector. Only
    /// keys with no existing value AND no seed entry are returned.
    pub async fn missing_pack_secrets(
        &self,
        pack_path: &Path,
        provider_id: &str,
    ) -> Result<Vec<MissingKey>> {
        let reqs = load_secret_requirements_from_pack(pack_path)?;
        let mut missing = Vec::new();
        for req in reqs {
            let uri = canonical_secret_uri(
                &self.env,
                &self.tenant,
                self.team.as_deref(),
                provider_id,
                &req.key,
            );
            match self.store.get(&uri).await {
                Ok(_) => continue,
                Err(SecretError::NotFound { .. }) => {}
                Err(err) => return Err(anyhow!("failed to read secret {uri}: {err}")),
            }
            // Auto-apply seed entry transparently if available.
            if let Some(seed) = self.seeds.get(&uri) {
                let _report = apply_seed(
                    &self.store,
                    &SeedDoc {
                        entries: vec![seed.clone()],
                    },
                    ApplyOptions::default(),
                )
                .await;
                continue;
            }
            missing.push(MissingKey {
                provider_id: provider_id.to_string(),
                key: req.key.clone(),
                uri,
                description: req.description.clone(),
                required: req.required,
                looks_secret: looks_secret(&req.key),
            });
        }
        Ok(missing)
    }

    /// Persist a single text-valued secret at the given canonical URI.
    pub async fn set_secret_text(&self, uri: &str, value: &str) -> Result<()> {
        self.store
            .put(uri, SecretFormat::Text, value.as_bytes())
            .await
            .map_err(|err| anyhow!("failed to write secret {uri}: {err}"))
    }

    /// Ensure all required secrets for a pack exist in the dev store.
    ///
    /// Reads `assets/secret-requirements.json` from the pack and seeds any
    /// missing keys from `seeds.yaml` or with a placeholder.
    pub async fn ensure_pack_secrets(&self, pack_path: &Path, provider_id: &str) -> Result<()> {
        let keys = load_secret_keys_from_pack(pack_path)?;
        if keys.is_empty() {
            return Ok(());
        }

        let mut missing = Vec::new();
        for key in keys {
            let uri = canonical_secret_uri(
                &self.env,
                &self.tenant,
                self.team.as_deref(),
                provider_id,
                &key,
            );
            debug!(uri = %uri, provider = %provider_id, key = %key, "canonicalized secret requirement");
            match self.store.get(&uri).await {
                Ok(_) => continue,
                Err(SecretError::NotFound { .. }) => {
                    let source = if self.seeds.contains_key(&uri) {
                        "seeds.yaml"
                    } else {
                        "placeholder"
                    };
                    debug!(uri = %uri, source, "seeding missing secret");
                    missing.push(
                        self.seeds
                            .get(&uri)
                            .cloned()
                            .unwrap_or_else(|| placeholder_entry(uri)),
                    );
                }
                Err(err) => {
                    return Err(anyhow!("failed to read secret {uri}: {err}"));
                }
            }
        }

        if missing.is_empty() {
            return Ok(());
        }
        let report = apply_seed(
            &self.store,
            &SeedDoc { entries: missing },
            ApplyOptions::default(),
        )
        .await;
        if !report.failed.is_empty() {
            return Err(anyhow!("failed to seed secrets: {:?}", report.failed));
        }
        Ok(())
    }
}

/// Description of a required pack secret that has no value in the
/// store and no entry in `seeds.yaml`. Returned by
/// [`SecretsSetup::missing_pack_secrets`]; callers should prompt the
/// user and persist via [`SecretsSetup::set_secret_text`].
#[derive(Debug, Clone)]
pub struct MissingKey {
    /// Provider that declared the requirement (e.g. `messaging-telegram`).
    pub provider_id: String,
    /// The key name as declared in the pack manifest (e.g. `BOT_TOKEN`).
    pub key: String,
    /// Canonical secret URI (already includes env/tenant/team scope).
    pub uri: String,
    /// Human-readable description from the pack manifest, if any.
    pub description: Option<String>,
    /// Whether this key is marked `required: true` in the manifest.
    pub required: bool,
    /// True when the key name looks like a secret (token/password/etc.)
    /// and should be prompted with no-echo input. False for plain
    /// configuration values that can be echoed.
    pub looks_secret: bool,
}

/// Heuristic: name suggests this is a secret value (no-echo prompt) vs
/// a plain config value (echoed prompt).
fn looks_secret(key: &str) -> bool {
    let k = key.to_lowercase();
    [
        "token",
        "secret",
        "password",
        "api_key",
        "apikey",
        "private_key",
        "credential",
        "passphrase",
    ]
    .iter()
    .any(|needle| k.contains(needle))
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn load_seed_entries(bundle_root: &Path) -> Result<HashMap<String, SeedEntry>> {
    for candidate in seed_paths(bundle_root) {
        if candidate.exists() {
            let contents = std::fs::read_to_string(&candidate)?;
            let doc: SeedDoc = serde_yaml_bw::from_str(&contents)?;
            return Ok(doc
                .entries
                .into_iter()
                .map(|entry| (entry.uri.clone(), entry))
                .collect());
        }
    }
    Ok(HashMap::new())
}

fn seed_paths(bundle_root: &Path) -> [PathBuf; 2] {
    [
        bundle_root.join("seeds.yaml"),
        bundle_root.join("state").join("seeds.yaml"),
    ]
}

fn placeholder_entry(uri: String) -> SeedEntry {
    SeedEntry {
        uri: uri.clone(),
        format: SecretFormat::Text,
        value: SeedValue::Text {
            text: format!("placeholder for {uri}"),
        },
        description: Some("auto-applied placeholder".to_string()),
    }
}

/// Load secret requirement keys from a `.gtpack` archive.
///
/// Tries `assets/secret-requirements.json` first, then falls back to
/// CBOR manifest extraction.
pub fn load_secret_keys_from_pack(pack_path: &Path) -> Result<Vec<String>> {
    Ok(load_secret_requirements_from_pack(pack_path)?
        .into_iter()
        .map(|req| req.key)
        .collect())
}

/// Rich secret requirements extracted from a `.gtpack` archive.
pub fn load_secret_requirements_from_pack(pack_path: &Path) -> Result<Vec<PackSecretRequirement>> {
    let file = std::fs::File::open(pack_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    for entry_name in &[
        "assets/secret-requirements.json",
        "assets/secret_requirements.json",
        "secret-requirements.json",
        "secret_requirements.json",
    ] {
        match archive.by_name(entry_name) {
            Ok(reader) => {
                let reqs: Vec<PackSecretRequirement> = serde_json::from_reader(reader)?;
                return Ok(dedup_requirements(reqs));
            }
            Err(zip::result::ZipError::FileNotFound) => continue,
            Err(err) => return Err(err.into()),
        }
    }

    let mut reqs = Vec::new();
    for index in 0..archive.len() {
        let name = {
            let entry = archive.by_index(index)?;
            entry.name().to_string()
        };
        if name != "manifest.cbor" && !name.ends_with(".manifest.cbor") {
            continue;
        }
        let mut entry = archive.by_name(&name)?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes)?;
        let value: CborValue = serde_cbor::from_slice(&bytes)?;
        collect_secret_requirements_from_cbor(&value, &mut reqs);
    }

    Ok(dedup_requirements(reqs))
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct PackSecretRequirement {
    pub key: String,
    #[serde(default = "default_required")]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_required() -> bool {
    true
}

fn dedup_requirements(reqs: Vec<PackSecretRequirement>) -> Vec<PackSecretRequirement> {
    let mut by_key = BTreeMap::new();
    for req in reqs {
        by_key.entry(req.key.clone()).or_insert(req);
    }
    by_key.into_values().collect()
}

fn collect_secret_requirements_from_cbor(value: &CborValue, out: &mut Vec<PackSecretRequirement>) {
    match value {
        CborValue::Array(values) => {
            for value in values {
                collect_secret_requirements_from_cbor(value, out);
            }
        }
        CborValue::Map(map) => {
            if let Some(req) = parse_secret_requirement_map(map) {
                out.push(req);
            }
            for value in map.values() {
                collect_secret_requirements_from_cbor(value, out);
            }
        }
        _ => {}
    }
}

fn parse_secret_requirement_map(
    map: &BTreeMap<CborValue, CborValue>,
) -> Option<PackSecretRequirement> {
    let key = map_get_text(map, "key")?;
    let has_secret_shape = map.contains_key(&CborValue::Text("required".to_string()))
        || map.contains_key(&CborValue::Text("scope".to_string()))
        || map.contains_key(&CborValue::Text("format".to_string()))
        || map.contains_key(&CborValue::Text("description".to_string()));
    if !has_secret_shape {
        return None;
    }
    Some(PackSecretRequirement {
        key,
        required: map_get_bool(map, "required").unwrap_or(true),
        description: map_get_text(map, "description"),
    })
}

fn map_get_text(map: &BTreeMap<CborValue, CborValue>, key: &str) -> Option<String> {
    map.get(&CborValue::Text(key.to_string()))
        .and_then(|value| match value {
            CborValue::Text(text) => Some(text.clone()),
            _ => None,
        })
}

fn map_get_bool(map: &BTreeMap<CborValue, CborValue>, key: &str) -> Option<bool> {
    map.get(&CborValue::Text(key.to_string()))
        .and_then(|value| match value {
            CborValue::Bool(flag) => Some(*flag),
            _ => None,
        })
}

/// Open a `DevStore` from a bundle root path (convenience).
///
/// If a global key provider has been installed via
/// [`set_global_key_provider`], the store is opened with AES-256-GCM
/// encryption. Otherwise it opens in legacy plaintext mode (back-compat).
pub fn open_dev_store(bundle_root: &Path) -> Result<DevStore> {
    let store_path = ensure_path(bundle_root)?;
    if let Some(provider) = GLOBAL_KEY_PROVIDER.get() {
        let allow = ALLOW_DOWNGRADE.get().copied().unwrap_or(false);
        DevStore::with_path_encrypted(&store_path, provider.clone(), allow).map_err(|err| {
            anyhow!(
                "failed to open encrypted dev secrets store {}: {err}",
                store_path.display()
            )
        })
    } else {
        DevStore::with_path(&store_path).map_err(|err| {
            anyhow!(
                "failed to open dev secrets store {}: {err}",
                store_path.display()
            )
        })
    }
}

static GLOBAL_KEY_PROVIDER: std::sync::OnceLock<
    std::sync::Arc<secrets_provider_dev::PassphraseKeyProvider>,
> = std::sync::OnceLock::new();
static ALLOW_DOWNGRADE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Install a process-global passphrase-derived key provider.
///
/// All subsequent calls to [`open_dev_store`] (and [`SecretsSetup::new`])
/// will use AES-256-GCM with this provider. Idempotent — only the first
/// call wins. Call this exactly once at CLI startup, after resolving the
/// user passphrase, and before any code path that opens the dev store.
pub fn set_global_key_provider(
    provider: std::sync::Arc<secrets_provider_dev::PassphraseKeyProvider>,
    allow_downgrade: bool,
) {
    let _ = GLOBAL_KEY_PROVIDER.set(provider);
    let _ = ALLOW_DOWNGRADE.set(allow_downgrade);
}

/// Returns true if a global key provider has been installed.
pub fn has_global_key_provider() -> bool {
    GLOBAL_KEY_PROVIDER.get().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn write_pack_with_secret_requirements(path: &Path, req_json: &str) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "assets/secret-requirements.json",
            SimpleFileOptions::default(),
        )?;
        zip.write_all(req_json.as_bytes())?;
        zip.finish()?;
        Ok(())
    }

    #[test]
    fn ensure_path_creates_parent_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("bundle");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        let path = ensure_path(&bundle).expect("ensure path");
        assert!(path.ends_with(".greentic/dev/.dev.secrets.env"));
        assert!(path.parent().expect("parent").exists());
    }

    #[test]
    fn find_existing_with_override_prefers_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("bundle");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        let override_file = temp.path().join("custom.env");
        std::fs::write(&override_file, "KEY=value\n").expect("write override");

        let found = find_existing_with_override(&bundle, Some(&override_file));
        assert_eq!(found.as_deref(), Some(override_file.as_path()));
    }

    #[test]
    fn find_existing_finds_default_locations() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("bundle");
        let store_path = bundle.join(STORE_RELATIVE);
        std::fs::create_dir_all(store_path.parent().expect("parent")).expect("create dirs");
        std::fs::write(&store_path, "K=V\n").expect("write store");

        let found = find_existing_with_override(&bundle, None).expect("found");
        assert_eq!(found, store_path);
    }

    #[test]
    fn load_secret_keys_from_pack_reads_requirements() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pack = temp.path().join("provider.gtpack");
        write_pack_with_secret_requirements(&pack, r#"[{"key":"BOT_TOKEN"},{"key":"API_SECRET"}]"#)
            .expect("write pack");

        let keys = load_secret_keys_from_pack(&pack).expect("load keys");
        assert_eq!(
            keys,
            vec!["API_SECRET".to_string(), "BOT_TOKEN".to_string()]
        );
    }

    #[test]
    fn load_secret_keys_from_pack_reads_cbor_manifest_requirements() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pack = temp.path().join("provider.gtpack");
        let file = std::fs::File::create(&pack).expect("create pack");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("manifest.cbor", SimpleFileOptions::default())
            .expect("start entry");
        let manifest = serde_json::json!({
            "components": [
                {
                    "host": {
                        "secrets": {
                            "required": [
                                {
                                    "key": "auth.param.get_weather.key",
                                    "required": true,
                                    "description": "Weather key",
                                    "scope": {"env": "runtime", "tenant": "runtime"},
                                    "format": "text"
                                }
                            ]
                        }
                    }
                }
            ]
        });
        let bytes = serde_cbor::to_vec(&manifest).expect("serialize cbor");
        zip.write_all(&bytes).expect("write manifest");
        zip.finish().expect("finish zip");

        let keys = load_secret_keys_from_pack(&pack).expect("load keys");
        assert_eq!(keys, vec!["auth.param.get_weather.key".to_string()]);

        let reqs = load_secret_requirements_from_pack(&pack).expect("load reqs");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].description.as_deref(), Some("Weather key"));
    }

    #[test]
    fn load_secret_keys_from_pack_returns_empty_without_requirements() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pack = temp.path().join("provider.gtpack");
        let file = std::fs::File::create(&pack).expect("create pack");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("assets/setup.yaml", SimpleFileOptions::default())
            .expect("start entry");
        zip.write_all(b"questions: []\n").expect("write setup");
        zip.finish().expect("finish zip");

        let keys = load_secret_keys_from_pack(&pack).expect("load keys");
        assert!(keys.is_empty());
    }

    #[tokio::test]
    async fn ensure_pack_secrets_seeds_placeholders_for_missing_keys() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("bundle");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        let pack = temp.path().join("provider.gtpack");
        write_pack_with_secret_requirements(&pack, r#"[{"key":"BOT_TOKEN"}]"#).expect("pack");

        let setup = SecretsSetup::new(&bundle, "dev", "tenant-a", Some("core")).expect("setup");
        setup
            .ensure_pack_secrets(&pack, "messaging-telegram")
            .await
            .expect("ensure secrets");

        let uri = canonical_secret_uri(
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-telegram",
            "BOT_TOKEN",
        );
        let value = setup.store().get(&uri).await.expect("seeded value");
        let value = String::from_utf8(value).expect("utf8");
        assert!(
            value.contains("placeholder for secrets://"),
            "unexpected placeholder value: {value}"
        );
    }

    #[tokio::test]
    async fn ensure_pack_secrets_uses_seed_values_when_available() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("bundle");
        std::fs::create_dir_all(&bundle).expect("bundle dir");
        let seed_uri = canonical_secret_uri(
            "dev",
            "tenant-a",
            Some("core"),
            "messaging-telegram",
            "BOT_TOKEN",
        );
        let seeds_yaml = serde_yaml_bw::to_string(&SeedDoc {
            entries: vec![SeedEntry {
                uri: seed_uri.clone(),
                format: SecretFormat::Text,
                value: SeedValue::Text {
                    text: "seeded-secret".to_string(),
                },
                description: Some("test seed".to_string()),
            }],
        })
        .expect("serialize seeds");
        std::fs::write(bundle.join("seeds.yaml"), seeds_yaml).expect("write seeds");

        let pack = temp.path().join("provider.gtpack");
        write_pack_with_secret_requirements(&pack, r#"[{"key":"BOT_TOKEN"}]"#).expect("pack");

        let setup = SecretsSetup::new(&bundle, "dev", "tenant-a", Some("core")).expect("setup");
        setup
            .ensure_pack_secrets(&pack, "messaging-telegram")
            .await
            .expect("ensure secrets");

        let value = setup.store().get(&seed_uri).await.expect("seeded value");
        let value = String::from_utf8(value).expect("utf8");
        assert_eq!(value, "seeded-secret");
    }
}
