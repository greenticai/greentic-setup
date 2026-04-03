//! Dev secrets store management for bundle setup.
//!
//! Provides helpers for locating the dev secrets file and
//! [`SecretsSetup`] for ensuring pack secrets are seeded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use greentic_secrets_lib::core::Error as SecretError;
use greentic_secrets_lib::{
    ApplyOptions, DevStore, SecretFormat, SecretsStore, SeedDoc, SeedEntry, SeedValue, apply_seed,
};
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
        info!(path = %store_path.display(), "secrets: using dev store backend");
        let store = DevStore::with_path(&store_path).map_err(|err| {
            anyhow!(
                "failed to open dev secrets store {}: {err}",
                store_path.display()
            )
        })?;
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
                let reqs: Vec<SecretRequirement> = serde_json::from_reader(reader)?;
                return Ok(reqs.into_iter().map(|r| r.key).collect());
            }
            Err(zip::result::ZipError::FileNotFound) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(vec![])
}

#[derive(serde::Deserialize)]
struct SecretRequirement {
    key: String,
}

/// Open a `DevStore` from a bundle root path (convenience).
pub fn open_dev_store(bundle_root: &Path) -> Result<DevStore> {
    let store_path = ensure_path(bundle_root)?;
    DevStore::with_path(&store_path).map_err(|err| {
        anyhow!(
            "failed to open dev secrets store {}: {err}",
            store_path.display()
        )
    })
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
            vec!["BOT_TOKEN".to_string(), "API_SECRET".to_string()]
        );
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
