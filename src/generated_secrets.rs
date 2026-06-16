//! Generation of provider runtime secrets declared via the
//! `greentic.generated-secrets.v1` pack extension.
//!
//! Setup is the single place that *introduces* secrets into the local dev
//! secrets store. Generated secrets (for example a provider JWT signing key)
//! are materialised here so that `gtc start` can simply move them into the
//! deployment target's secrets manager without re-generating divergent values.
//!
//! The parsing mirrors `greentic-start`'s contract: a secret is declared under
//! `extensions["greentic.generated-secrets.v1"].inline.secrets[]`. We prefer the
//! JSON manifest (`pack.manifest.json`) because it is free of the CBOR
//! symbol-interning used by `manifest.cbor`, and fall back to a best-effort CBOR
//! navigation when only the binary manifest is present.

use std::fmt::Write as _;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngExt as _;
use serde::Deserialize;
use serde_cbor::Value as CborValue;
use zip::{ZipArchive, result::ZipError};

const EXT_GENERATED_SECRETS_V1: &str = "greentic.generated-secrets.v1";

/// A pack-declared secret that setup must generate and introduce locally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedRequirement {
    pub key: String,
    pub aliases: Vec<String>,
    /// Whether the secret is mandatory. Optional generated secrets are not
    /// force-introduced at setup — they are warned about and left for runtime
    /// resolution (mirroring runtime-acquired secrets such as OAuth tokens).
    pub required: bool,
    pub spec: GeneratedSecretSpec,
}

/// Generation policy for a declared secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedSecretSpec {
    pub policy: String,
    pub length: usize,
    pub encoding: String,
    pub scope_level: String,
    pub scope_team: Option<String>,
    pub regenerate_if_present: bool,
}

/// Load the generated-secret requirements declared by a `.gtpack`.
///
/// Returns an empty vector when the pack declares none (or is not a readable
/// zip archive); discovery failures elsewhere must not block setup.
pub fn load_generated_requirements_from_pack(
    pack_path: &Path,
) -> Result<Vec<GeneratedRequirement>> {
    let file = match File::open(pack_path) {
        Ok(file) => file,
        Err(_) => return Ok(Vec::new()),
    };
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(_) => return Ok(Vec::new()),
    };

    if let Some(reqs) = load_from_json_manifest(&mut archive)? {
        return Ok(reqs);
    }
    load_from_cbor_manifest(&mut archive)
}

fn load_from_json_manifest(
    archive: &mut ZipArchive<File>,
) -> Result<Option<Vec<GeneratedRequirement>>> {
    let mut contents = String::new();
    match archive.by_name("pack.manifest.json") {
        Ok(mut entry) => {
            entry.read_to_string(&mut contents)?;
        }
        Err(ZipError::FileNotFound) => return Ok(None),
        Err(err) => return Err(err.into()),
    }
    let manifest: serde_json::Value = serde_json::from_str(&contents)?;
    let Some(inline) = manifest
        .get("extensions")
        .and_then(|extensions| extensions.get(EXT_GENERATED_SECRETS_V1))
        .and_then(|extension| extension.get("inline"))
    else {
        return Ok(Some(Vec::new()));
    };
    let ext: GeneratedSecretsExtension = serde_json::from_value(inline.clone())?;
    Ok(Some(ext.into_requirements()))
}

fn load_from_cbor_manifest(archive: &mut ZipArchive<File>) -> Result<Vec<GeneratedRequirement>> {
    let manifest_names: Vec<String> = (0..archive.len())
        .filter_map(|index| {
            archive
                .by_index(index)
                .ok()
                .map(|entry| entry.name().to_string())
        })
        .filter(|name| name == "manifest.cbor" || name.ends_with(".manifest.cbor"))
        .collect();

    for name in manifest_names {
        let mut bytes = Vec::new();
        archive.by_name(&name)?.read_to_end(&mut bytes)?;
        let Ok(value) = serde_cbor::from_slice::<CborValue>(&bytes) else {
            continue;
        };
        let Some(inline) = cbor_get(&value, "extensions")
            .and_then(|extensions| cbor_get(extensions, EXT_GENERATED_SECRETS_V1))
            .and_then(|extension| cbor_get(extension, "inline"))
        else {
            continue;
        };
        // Re-encode the inline subtree and decode it through the typed shape so
        // we reuse the same defaults as the JSON path.
        let Ok(buf) = serde_cbor::to_vec(inline) else {
            continue;
        };
        if let Ok(ext) = serde_cbor::from_slice::<GeneratedSecretsExtension>(&buf) {
            let reqs = ext.into_requirements();
            if !reqs.is_empty() {
                return Ok(reqs);
            }
        }
    }
    Ok(Vec::new())
}

fn cbor_get<'a>(value: &'a CborValue, key: &str) -> Option<&'a CborValue> {
    match value {
        CborValue::Map(map) => map.get(&CborValue::Text(key.to_string())),
        _ => None,
    }
}

/// Generate the value for a declared secret according to its policy.
pub fn generate_secret_value(spec: &GeneratedSecretSpec) -> Result<String> {
    if !spec.policy.eq_ignore_ascii_case("random") {
        return Err(anyhow!(
            "unsupported generated secret policy `{}`",
            spec.policy
        ));
    }
    let length = spec.length.max(1);
    match spec.encoding.as_str() {
        "raw_text" => Ok(random_ascii(length)),
        "base64url" => {
            let mut bytes = vec![0u8; length];
            rand::rng().fill(&mut bytes[..]);
            Ok(URL_SAFE_NO_PAD.encode(bytes))
        }
        "hex" => {
            let mut bytes = vec![0u8; length];
            rand::rng().fill(&mut bytes[..]);
            let mut out = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                let _ = write!(out, "{byte:02x}");
            }
            Ok(out)
        }
        other => Err(anyhow!("unsupported generated secret encoding `{other}`")),
    }
}

fn random_ascii(length: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
    let mut bytes = vec![0u8; length];
    rand::rng().fill(&mut bytes[..]);
    bytes
        .into_iter()
        .map(|byte| ALPHABET[usize::from(byte) % ALPHABET.len()] as char)
        .collect()
}

/// Resolve the team segment for a generated secret's canonical URI.
///
/// Tenant-scoped secrets (or those that pin `team: "_"`) collapse to the
/// tenant-wide `_` placeholder; otherwise the declared team wins, falling back
/// to the active setup team.
pub fn scope_team<'a>(
    spec: &'a GeneratedSecretSpec,
    default_team: Option<&'a str>,
) -> Option<&'a str> {
    if spec.scope_level.eq_ignore_ascii_case("tenant") || spec.scope_team.as_deref() == Some("_") {
        return None;
    }
    spec.scope_team.as_deref().or(default_team)
}

#[derive(Deserialize)]
struct GeneratedSecretsExtension {
    #[serde(default)]
    secrets: Vec<GeneratedSecretEntry>,
}

impl GeneratedSecretsExtension {
    fn into_requirements(self) -> Vec<GeneratedRequirement> {
        self.secrets
            .into_iter()
            .map(|secret| GeneratedRequirement {
                key: secret.key.to_lowercase(),
                aliases: secret.aliases,
                required: secret.required.unwrap_or(true),
                spec: GeneratedSecretSpec {
                    policy: secret.policy.unwrap_or_else(|| "random".to_string()),
                    length: secret.length.unwrap_or(20),
                    encoding: secret.encoding.unwrap_or_else(|| "raw_text".to_string()),
                    scope_level: secret
                        .scope
                        .as_ref()
                        .and_then(|scope| scope.level.clone())
                        .unwrap_or_else(|| "tenant".to_string()),
                    scope_team: secret.scope.and_then(|scope| scope.team),
                    regenerate_if_present: secret.regenerate_if_present.unwrap_or(false),
                },
            })
            .collect()
    }
}

#[derive(Deserialize)]
struct GeneratedSecretEntry {
    key: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    required: Option<bool>,
    policy: Option<String>,
    length: Option<usize>,
    encoding: Option<String>,
    scope: Option<GeneratedSecretScope>,
    #[serde(default)]
    regenerate_if_present: Option<bool>,
}

#[derive(Deserialize)]
struct GeneratedSecretScope {
    level: Option<String>,
    team: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::FileOptions;

    fn write_pack(path: &Path, entries: &[(&str, Vec<u8>)]) {
        let file = File::create(path).expect("create pack");
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in entries {
            zip.start_file(*name, FileOptions::<()>::default())
                .expect("start file");
            zip.write_all(bytes).expect("write file");
        }
        zip.finish().expect("finish pack");
    }

    #[test]
    fn reads_generated_requirement_from_json_manifest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("messaging-webchat-gui.gtpack");
        let manifest = serde_json::json!({
            "extensions": {
                "greentic.generated-secrets.v1": {
                    "kind": "greentic.generated-secrets.v1",
                    "inline": {
                        "secrets": [{
                            "key": "jwt_signing_key",
                            "aliases": ["JWT_SIGNING_KEY"],
                            "required": true,
                            "policy": "random",
                            "length": 20,
                            "encoding": "raw_text",
                            "scope": {"level": "tenant", "team": "_"},
                            "regenerate_if_present": false
                        }]
                    }
                }
            }
        });
        write_pack(
            &pack,
            &[(
                "pack.manifest.json",
                serde_json::to_vec(&manifest).expect("manifest json"),
            )],
        );

        let reqs = load_generated_requirements_from_pack(&pack).expect("load");
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].key, "jwt_signing_key");
        assert_eq!(reqs[0].aliases, vec!["JWT_SIGNING_KEY"]);
        assert_eq!(reqs[0].spec.length, 20);
        assert_eq!(reqs[0].spec.scope_level, "tenant");
        assert!(!reqs[0].spec.regenerate_if_present);
    }

    #[test]
    fn empty_pack_yields_no_generated_requirements() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("empty.gtpack");
        write_pack(&pack, &[("pack.manifest.json", b"{}".to_vec())]);
        assert!(
            load_generated_requirements_from_pack(&pack)
                .expect("load")
                .is_empty()
        );
    }

    #[test]
    fn generated_value_honours_length_and_encoding() {
        let raw = generate_secret_value(&GeneratedSecretSpec {
            policy: "random".into(),
            length: 20,
            encoding: "raw_text".into(),
            scope_level: "tenant".into(),
            scope_team: Some("_".into()),
            regenerate_if_present: false,
        })
        .expect("raw");
        assert_eq!(raw.len(), 20);
        assert!(!raw.contains("placeholder"));

        let hex = generate_secret_value(&GeneratedSecretSpec {
            policy: "random".into(),
            length: 16,
            encoding: "hex".into(),
            scope_level: "tenant".into(),
            scope_team: None,
            regenerate_if_present: false,
        })
        .expect("hex");
        assert_eq!(hex.len(), 32);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn unsupported_policy_is_rejected() {
        let err = generate_secret_value(&GeneratedSecretSpec {
            policy: "fixed".into(),
            length: 20,
            encoding: "raw_text".into(),
            scope_level: "tenant".into(),
            scope_team: None,
            regenerate_if_present: false,
        })
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported generated secret policy")
        );
    }

    #[test]
    fn tenant_scope_collapses_team_to_none() {
        let spec = GeneratedSecretSpec {
            policy: "random".into(),
            length: 20,
            encoding: "raw_text".into(),
            scope_level: "tenant".into(),
            scope_team: Some("ops".into()),
            regenerate_if_present: false,
        };
        assert_eq!(scope_team(&spec, Some("default")), None);

        let team_scoped = GeneratedSecretSpec {
            scope_level: "team".into(),
            scope_team: Some("ops".into()),
            ..spec
        };
        assert_eq!(scope_team(&team_scoped, Some("default")), Some("ops"));
    }
}
