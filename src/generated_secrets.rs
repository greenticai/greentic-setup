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
use greentic_secrets_lib::core::Error as SecretError;
use greentic_secrets_lib::{DevStore, SecretFormat, SecretsStore};
use greentic_types::{ExtensionInline, decode_pack_manifest};
use rand::RngExt as _;
use serde::Deserialize;
use serde_json::Value;
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
    let mut bytes = Vec::new();
    match archive.by_name("manifest.cbor") {
        Ok(mut entry) => {
            entry.read_to_end(&mut bytes)?;
        }
        Err(ZipError::FileNotFound) => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    }
    // The pack manifest CBOR uses a symbol table to intern keys, so it must be
    // decoded through `decode_pack_manifest` rather than navigated as raw CBOR.
    let manifest = match decode_pack_manifest(&bytes) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(Vec::new()),
    };
    let Some(extension) = manifest
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get(EXT_GENERATED_SECRETS_V1))
    else {
        return Ok(Vec::new());
    };
    let Some(ExtensionInline::Other(value)) = extension.inline.as_ref() else {
        return Ok(Vec::new());
    };
    let ext: GeneratedSecretsExtension = serde_json::from_value(value.clone())?;
    Ok(ext.into_requirements())
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

/// Generate and introduce a pack's declared generated secrets into `store`.
///
/// This is the single place that materialises generated secrets at setup time
/// so `gtc start` can move the already-resolved value into the deployment
/// target's secrets manager rather than regenerating a divergent one.
///
/// - Existing values are preserved unless the pack opts into
///   `regenerate_if_present`. In the default (`false`) case, a present value
///   is never touched — the `generated_present` short-circuit below returns
///   before a write is even attempted, so there is nothing for
///   [`crate::secret_collision::check`] to guard there.
/// - Optional secrets that are absent are warned about and left for runtime
///   resolution (mirroring runtime-acquired secrets such as OAuth tokens).
/// - When `regenerate_if_present` is `true`, this function unconditionally
///   overwrites whatever is already stored — that write goes through the
///   same collision guard as every other write in this crate.
///
/// `existing_answers` is threaded through for consistency with the other
/// guarded callers, but a generated secret's key is never recorded there: it
/// is synthesised by setup itself, not typed in by the operator, so it never
/// appears in `state/config/<provider_id>/setup-answers.json`. There is no
/// other did-I-write-this marker available at this call site either — the
/// stored value can't serve as one, since [`generate_secret_value`] mints a
/// fresh random value on every call, so even this same bundle regenerating
/// its own secret would almost never produce byte-identical output to
/// compare against. Passing the marker through anyway (rather than
/// hardcoding `None`) keeps this call symmetric with the others and leaves
/// room for a real marker later, but today it is a no-op: once a
/// `regenerate_if_present: true` secret has been written once, any later
/// write to that address — including by the same bundle — reads as "cannot
/// prove I own this" and is refused, fail-closed, the same way an unreadable
/// store is. As of this writing no pack in the workspace sets
/// `regenerate_if_present: true` (all declared instances use `false`), so
/// this is a real but currently unexercised trade-off, not a regression of
/// an observed working flow.
///
/// Returns the keys actually introduced (for logging).
pub async fn introduce_into_store(
    store: &DevStore,
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    pack_path: &Path,
    existing_answers: Option<&Value>,
) -> Result<Vec<String>> {
    let mut introduced = Vec::new();
    for req in load_generated_requirements_from_pack(pack_path)? {
        let team = scope_team(&req.spec, team);
        let uri = crate::canonical_secret_uri(env, tenant, team, provider_id, &req.key);

        if !req.spec.regenerate_if_present
            && generated_present(store, env, tenant, team, provider_id, &req).await?
        {
            continue;
        }
        if !req.required {
            tracing::warn!(
                uri = %uri,
                provider = %provider_id,
                key = %req.key,
                "optional generated secret not introduced at setup; leaving for runtime resolution"
            );
            continue;
        }
        let value = generate_secret_value(&req.spec)?;
        if let Some(collision) = crate::secret_collision::check(
            store,
            existing_answers,
            tenant,
            team,
            provider_id,
            &req.key,
            &uri,
            &value,
        )
        .await
        {
            anyhow::bail!(
                "{}",
                crate::secret_collision::message_unattributed(&collision)
            );
        }
        store
            .put(&uri, SecretFormat::Text, value.as_bytes())
            .await
            .map_err(|err| anyhow!("failed to store generated secret {uri}: {err}"))?;
        tracing::info!(
            uri = %uri,
            provider = %provider_id,
            key = %req.key,
            "setup: generated and introduced secret into the local store"
        );
        introduced.push(req.key.clone());
    }
    Ok(introduced)
}

/// Whether a generated secret already exists under its canonical key or any
/// declared alias.
async fn generated_present<S: SecretsStore + ?Sized>(
    store: &S,
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    req: &GeneratedRequirement,
) -> Result<bool> {
    for key in std::iter::once(req.key.as_str()).chain(req.aliases.iter().map(String::as_str)) {
        let uri = crate::canonical_secret_uri(env, tenant, team, provider_id, key);
        match store.get(&uri).await {
            Ok(_) => return Ok(true),
            Err(SecretError::NotFound { .. }) => continue,
            Err(err) => return Err(anyhow!("failed to read secret {uri}: {err}")),
        }
    }
    Ok(false)
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

    fn jwt_manifest() -> serde_json::Value {
        serde_json::json!({
            "extensions": {
                "greentic.generated-secrets.v1": {
                    "inline": { "secrets": [{
                        "key": "jwt_signing_key",
                        "required": true,
                        "policy": "random",
                        "length": 20,
                        "encoding": "raw_text",
                        "scope": {"level": "tenant", "team": "_"},
                        "regenerate_if_present": false
                    }] }
                }
            }
        })
    }

    /// Same declared secret as [`jwt_manifest`], but opted into unconditional
    /// regeneration on every introduction — the only branch of
    /// `introduce_into_store` that reaches an actual `store.put` when a value
    /// is already present, and therefore the only branch where the collision
    /// guard has anything to check.
    fn jwt_manifest_regenerating() -> serde_json::Value {
        serde_json::json!({
            "extensions": {
                "greentic.generated-secrets.v1": {
                    "inline": { "secrets": [{
                        "key": "jwt_signing_key",
                        "required": true,
                        "policy": "random",
                        "length": 20,
                        "encoding": "raw_text",
                        "scope": {"level": "tenant", "team": "_"},
                        "regenerate_if_present": true
                    }] }
                }
            }
        })
    }

    #[tokio::test]
    async fn introduce_into_store_generates_persists_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("messaging-webchat-gui.gtpack");
        write_pack(
            &pack,
            &[(
                "pack.manifest.json",
                serde_json::to_vec(&jwt_manifest()).unwrap(),
            )],
        );
        let store = DevStore::with_path(dir.path().join(".dev.secrets.env")).expect("store");

        let introduced = introduce_into_store(
            &store,
            "dev",
            "demo",
            Some("default"),
            "messaging-webchat-gui",
            &pack,
            None,
        )
        .await
        .expect("introduce");
        assert_eq!(introduced, vec!["jwt_signing_key".to_string()]);

        // Tenant-scoped secret collapses the team segment to `_`.
        let uri = crate::canonical_secret_uri(
            "dev",
            "demo",
            None,
            "messaging-webchat-gui",
            "jwt_signing_key",
        );
        let value = store.get(&uri).await.expect("stored");
        assert_eq!(value.len(), 20);

        // regenerate_if_present=false -> a second pass preserves the value.
        let again = introduce_into_store(
            &store,
            "dev",
            "demo",
            Some("default"),
            "messaging-webchat-gui",
            &pack,
            None,
        )
        .await
        .expect("introduce again");
        assert!(again.is_empty());
        assert_eq!(store.get(&uri).await.expect("still"), value);
    }

    // ── secret-collision guard (Task 3b) ───────────────────────────────────

    /// A pack that opts into `regenerate_if_present: true` still introduces
    /// cleanly on a bundle's first run — nothing is stored yet, so
    /// `secret_collision::check` sees an absent address and does not refuse.
    /// This is the legitimate flow for this path: the common case (a fresh
    /// bundle generating its own secret for the first time).
    #[tokio::test]
    async fn regenerating_pack_introduces_cleanly_on_first_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("messaging-webchat-gui.gtpack");
        write_pack(
            &pack,
            &[(
                "pack.manifest.json",
                serde_json::to_vec(&jwt_manifest_regenerating()).unwrap(),
            )],
        );
        let store = DevStore::with_path(dir.path().join(".dev.secrets.env")).expect("store");

        let introduced = introduce_into_store(
            &store,
            "dev",
            "demo",
            Some("default"),
            "messaging-webchat-gui",
            &pack,
            None,
        )
        .await
        .expect("a fresh bundle's first introduction must not be refused");
        assert_eq!(introduced, vec!["jwt_signing_key".to_string()]);
    }

    /// A second bundle regenerating an already-occupied generated secret must
    /// be refused, the same as any other write this crate makes to the
    /// shared dev store. This is the concrete case the task names: two
    /// bundles under one tenant both declaring `messaging-webchat-gui`'s
    /// `jwt_signing_key` must not silently clobber each other.
    ///
    /// There is no did-I-write-this marker for a generated secret (see the
    /// doc comment on `introduce_into_store`), so `existing_answers` is
    /// `None` here exactly as it would be in practice — this test exercises
    /// the fail-closed path, not a contrived one.
    #[tokio::test]
    async fn a_second_bundle_regenerating_an_occupied_secret_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("messaging-webchat-gui.gtpack");
        write_pack(
            &pack,
            &[(
                "pack.manifest.json",
                serde_json::to_vec(&jwt_manifest_regenerating()).unwrap(),
            )],
        );
        let store = DevStore::with_path(dir.path().join(".dev.secrets.env")).expect("store");

        // Bundle A's own generated value, already resolved and stored.
        let uri = crate::canonical_secret_uri(
            "dev",
            "demo",
            None,
            "messaging-webchat-gui",
            "jwt_signing_key",
        );
        store
            .put(&uri, SecretFormat::Text, b"bundle-a-signing-key")
            .await
            .expect("seed bundle A's value");

        // Bundle B runs setup for the same provider under the same tenant,
        // with no record of ever having written this key.
        let err = introduce_into_store(
            &store,
            "dev",
            "demo",
            Some("default"),
            "messaging-webchat-gui",
            &pack,
            None,
        )
        .await
        .expect_err("a second bundle must not clobber bundle A's generated secret");

        let text = err.to_string();
        assert!(text.contains("jwt_signing_key"), "{text}");
        assert!(
            text.contains("--team"),
            "the error must tell the operator how to proceed: {text}"
        );
        assert!(
            !text.contains("written by a different bundle"),
            "the guard cannot verify who wrote it — bundle B is the only bundle in this \
             test, so claiming a second one would be false: {text}"
        );
    }

    /// The landmine this fail-closed choice creates, pinned so CI catches it
    /// the day someone flips a pack to `regenerate_if_present: true`: the
    /// *same* bundle, re-running `introduce_into_store` a second time for a
    /// secret it generated itself, is refused exactly like a different
    /// bundle would be — there is no did-I-write-this marker that can tell
    /// the two apart (see the doc comment on `introduce_into_store`). No
    /// pack in the workspace sets this flag today, so nothing breaks in
    /// practice yet; this test exists so the day one does, the trade-off is
    /// enforced by CI rather than discovered in production.
    #[tokio::test]
    async fn same_bundle_regenerating_an_existing_secret_is_refused_fail_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pack = dir.path().join("messaging-webchat-gui.gtpack");
        write_pack(
            &pack,
            &[(
                "pack.manifest.json",
                serde_json::to_vec(&jwt_manifest_regenerating()).unwrap(),
            )],
        );
        let store = DevStore::with_path(dir.path().join(".dev.secrets.env")).expect("store");

        introduce_into_store(
            &store,
            "dev",
            "demo",
            Some("default"),
            "messaging-webchat-gui",
            &pack,
            None,
        )
        .await
        .expect("this bundle's own first introduction must not be refused");

        // Same bundle, same `existing_answers` (`None`, exactly as before —
        // a generated secret's key was never going to be in there either
        // way), running setup again.
        let err = introduce_into_store(
            &store,
            "dev",
            "demo",
            Some("default"),
            "messaging-webchat-gui",
            &pack,
            None,
        )
        .await
        .expect_err(
            "fail-closed means even this same bundle's second run is refused, \
             not just a different bundle's — that is the documented trade-off",
        );

        let text = err.to_string();
        assert!(text.contains("jwt_signing_key"), "{text}");
        assert!(
            !text.contains("written by a different bundle"),
            "no other bundle exists in this test — the message must not claim one does: {text}"
        );
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
