//! Deploy a bundle into an environment via the env-apply engine.
//!
//! `greentic-setup env-deploy <BUNDLE>` synthesizes a
//! `greentic.env-manifest.v1` document from a `.gtbundle` archive (or
//! bundle directory) and routes it through the deployer's idempotent
//! env-apply engine — the same engine behind `gtc op env apply`. The
//! bundle's declared `bundle_id` is read from the archive metadata, not
//! inferred from the filename (see module-level doc on "the bundle_id
//! trap").
//!
//! # The bundle_id trap
//!
//! Multiple `.gtbundle` filenames can declare the *same* `bundle_id` in
//! their metadata (e.g. `quickstart-rich.gtbundle` and
//! `quickstart-rich-v2.gtbundle` both declare `bundle_id:
//! quickstart-bundle`). Using the filename stem instead of the declared
//! id would create a *second* deployment instead of converging on /
//! blue-greening the existing one, breaking the idempotency contract.

use std::path::Path;

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::dispatch::print_outcome;
use greentic_deployer::cli::env_manifest::ENV_MANIFEST_SCHEMA_V1;
use greentic_deployer::environment::{EnvFlock, LocalFsStore, atomic_write_bytes};
use greentic_types::DEFAULT_TEAM;
use serde_json::json;

use crate::gtbundle;

/// Bundle-manifest filename inside a built `.gtbundle` archive / directory.
const BUNDLE_MANIFEST_JSON: &str = "bundle-manifest.json";

/// Read `bundle_id` from a bundle directory's metadata.
///
/// Tries `bundle-manifest.json` first (the build-time manifest emitted by
/// `greentic-bundle build`), then falls back to `bundle.yaml` (the workspace
/// marker). Returns an error when neither file carries a `bundle_id`.
fn read_bundle_id_from_dir(dir: &Path) -> Result<String> {
    // Primary: bundle-manifest.json (build artifact).
    let manifest_path = dir.join(BUNDLE_MANIFEST_JSON);
    if manifest_path.is_file() {
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let doc: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        if let Some(id) = doc.get("bundle_id").and_then(|v| v.as_str())
            && !id.trim().is_empty()
        {
            return Ok(id.to_string());
        }
    }

    // Fallback: bundle.yaml (workspace marker).
    let yaml_path = dir.join(crate::bundle::BUNDLE_WORKSPACE_MARKER);
    if yaml_path.is_file() {
        let raw = std::fs::read_to_string(&yaml_path)
            .with_context(|| format!("read {}", yaml_path.display()))?;
        let doc: serde_yaml_bw::Value = serde_yaml_bw::from_str(&raw)
            .with_context(|| format!("parse {}", yaml_path.display()))?;
        if let Some(serde_yaml_bw::Value::String(id, _)) = doc
            .as_mapping()
            .and_then(|m| m.get(serde_yaml_bw::Value::String("bundle_id".into(), None)))
            && !id.trim().is_empty()
        {
            return Ok(id.clone());
        }
    }

    bail!(
        "cannot determine bundle_id: neither {} nor {} in {} contains a bundle_id field",
        BUNDLE_MANIFEST_JSON,
        crate::bundle::BUNDLE_WORKSPACE_MARKER,
        dir.display(),
    )
}

/// Deploy a bundle into an environment.
///
/// Resolves the input to an absolute `.gtbundle` archive, reads the
/// declared `bundle_id` from the archive metadata, synthesizes an
/// env-manifest document, and runs it through the env-apply engine.
///
/// When `customer_id` is `Some`, it is included in the synthesized
/// env-manifest so the deployer's `resolve_customer_id` finds it
/// (required for non-local environments).
///
/// `team` is `None` when the caller did not name one; the binding then uses
/// `default`, which is what every deployment bound before this parameter
/// existed. Callers that DO know their team must pass it — see
/// [`build_env_manifest`].
pub fn deploy_bundle_to_env(
    bundle: &Path,
    env_id: &str,
    dry_run: bool,
    non_interactive: bool,
    customer_id: Option<&str>,
    tenant: &str,
    team: Option<&str>,
) -> Result<()> {
    // ── Step 1: resolve to an absolute .gtbundle archive file ────────────
    let _temp_dir; // keep alive until after apply returns
    let archive_path: std::path::PathBuf;

    if bundle.is_file() {
        if !gtbundle::is_gtbundle_file(bundle) {
            bail!("{} is not a .gtbundle archive file", bundle.display(),);
        }
        archive_path = bundle
            .canonicalize()
            .with_context(|| format!("canonicalize {}", bundle.display()))?;
        _temp_dir = None;
    } else if bundle.is_dir() {
        let td = tempfile::tempdir().context("create temporary directory for .gtbundle archive")?;
        let stem = bundle
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bundle");
        let out = td.path().join(format!("{stem}.gtbundle"));
        gtbundle::create_gtbundle(bundle, &out)
            .with_context(|| format!("pack directory {} into .gtbundle", bundle.display()))?;
        archive_path = out
            .canonicalize()
            .with_context(|| format!("canonicalize {}", out.display()))?;
        _temp_dir = Some(td);
    } else {
        bail!(
            "{} does not exist or is not a .gtbundle file / bundle directory",
            bundle.display(),
        );
    };

    // ── Step 2: read bundle_id from metadata ────────────────────────────
    // For a directory input we read straight from the source dir (cheaper
    // than extracting the archive we just created). For an archive input we
    // extract to a temp dir, read, and clean up.
    let bundle_id = if bundle.is_dir() {
        read_bundle_id_from_dir(bundle)?
    } else {
        let extract_dir = gtbundle::extract_gtbundle_to_temp(&archive_path)
            .with_context(|| format!("extract {} to read metadata", archive_path.display()))?;
        let id = read_bundle_id_from_dir(&extract_dir);
        // Best-effort cleanup; ignore errors.
        let _ = std::fs::remove_dir_all(&extract_dir);
        id?
    };

    // ── Step 3: synthesize the env-manifest document ────────────────────
    let manifest = build_env_manifest(env_id, &bundle_id, &archive_path, customer_id, tenant, team);

    // ── Step 4: write to a temp file and call env-apply ─────────────────
    let manifest_file = tempfile::NamedTempFile::new().context("create temporary manifest file")?;
    std::fs::write(
        manifest_file.path(),
        serde_json::to_string_pretty(&manifest)?,
    )
    .context("write temporary manifest file")?;

    let root = LocalFsStore::default_root()
        .context("cannot locate the environment store: HOME / USERPROFILE not set")?;
    let store = LocalFsStore::new(root);
    let outcome = crate::env_mode::apply_manifest_with_store(
        &store,
        manifest_file.path(),
        &manifest,
        env_id,
        dry_run,
        non_interactive,
        Default::default(),
    )?;
    print_outcome(&outcome)?;

    // ── Step 5: record this as the default bundle (first deploy wins) ──
    //
    // When the environment has no `host_config.default_bundle` yet, stamp
    // the just-deployed bundle so the bare webchat URL
    // `/v1/web/webchat/{tenant}/` resolves without the fallback ladder.
    // FIRST DEPLOY WINS — an operator's explicit choice (or an earlier
    // deploy) is never overwritten; and a dry-run never mutates the store.
    if !dry_run {
        stamp_default_bundle_if_unset(&store, env_id, &bundle_id)?;
    }

    Ok(())
}

/// Build the synthesized single-bundle env-manifest document.
///
/// Split out so tests exercise the real construction instead of re-deriving it:
/// `customer_id` is required by the deployer's `resolve_customer_id` for every
/// non-`local` env, so dropping it here silently breaks named environments.
///
/// `tenant` is stamped onto the bundle's `route_binding.tenant_selector` so the
/// deployment binds — and therefore the runtime resolves secrets — under the
/// SAME tenant `gtc setup` wrote them to. Without it the deployer falls back to
/// its own default tenant (`default`), which diverges from the setup CLI's
/// default (`demo`); the runtime then reads under `default` while the secrets
/// live under `demo`, and only the reader-side tenant fallback rescues it.
///
/// `team` is stamped the same way, and for the same reason: it was previously
/// hardcoded to `default` here, so `--team` was accepted by the CLI, parsed,
/// and then silently discarded — the binding always said `default` no matter
/// what the operator asked for. `None` preserves that historical value; it is
/// the "caller named no team" case, not a licence to ignore one.
///
/// The deployer rejects a `tenant_selector` with no host/path matcher (a
/// binding with no matchers is unreachable), so we pair it with a `"/"`
/// path prefix — a match-all that is byte-equivalent, for routing, to the
/// empty-matcher binding the implicit default produced. Only the tenant
/// segment changes; every request still routes to this single-tenant bundle.
fn build_env_manifest(
    env_id: &str,
    bundle_id: &str,
    archive_path: &Path,
    customer_id: Option<&str>,
    tenant: &str,
    team: Option<&str>,
) -> serde_json::Value {
    let mut bundle_entry = json!({
        "bundle_id": bundle_id,
        "bundle_path": archive_path.to_string_lossy(),
        "route_binding": {
            "path_prefixes": ["/"],
            // `DEFAULT_TEAM` comes from greentic-types, the same place the
            // deployer takes its own implicit team from. That is the point of
            // the constant: the two ends of this handoff cannot drift apart
            // again the way the tenant defaults did.
            "tenant_selector": { "tenant": tenant, "team": team.unwrap_or(DEFAULT_TEAM) }
        },
    });
    if let Some(cid) = customer_id {
        bundle_entry
            .as_object_mut()
            .expect("bundle_entry is a json object")
            .insert("customer_id".to_string(), json!(cid));
    }
    json!({
        "schema": ENV_MANIFEST_SCHEMA_V1,
        "environment": { "id": env_id },
        "trust_root": "bootstrap",
        "bundles": [bundle_entry]
    })
}

/// If `host_config.default_bundle` is absent (or JSON-null), set it to
/// `bundle_id` and write the environment back. If already set, this is a
/// no-op — first deploy wins; an operator's explicit choice or an earlier
/// deploy is never silently re-pointed.
///
/// The read-modify-write runs under the per-env flock (the same lock the
/// deployer's `LocalFsStore::transact` acquires) so two concurrent deploys
/// cannot race past the "already set?" check. The write itself goes through
/// the deployer's atomic-write helper (temp + rename + fsync) so a crash
/// mid-write never leaves a truncated `environment.json`.
///
/// Reads / writes `environment.json` as raw JSON (the same approach
/// [`auto_detect_bundle_id`](crate::provider_commands::auto_detect_bundle_id)
/// uses) rather than the typed `Environment` struct, because the stamp is a
/// single-field patch and the raw path avoids coupling to the full schema.
fn stamp_default_bundle_if_unset(
    store: &LocalFsStore,
    env_id: &str,
    bundle_id: &str,
) -> Result<()> {
    let env_dir = store.root().join(env_id);
    let env_json_path = env_dir.join("environment.json");
    if !env_json_path.is_file() {
        // The apply engine should have created this. If it didn't (e.g.
        // the env was torn down mid-apply), skip rather than hard-fail —
        // the deploy itself already printed its outcome.
        return Ok(());
    }

    // Acquire the per-env flock so concurrent deploys serialize here.
    let lock_path = env_dir.join(".lock");
    let _guard = EnvFlock::acquire(&lock_path)
        .with_context(|| format!("acquire env lock for `{env_id}`"))?;

    let raw = std::fs::read_to_string(&env_json_path)
        .with_context(|| format!("read {}", env_json_path.display()))?;
    let mut doc: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", env_json_path.display()))?;

    let already_set = doc
        .pointer("/host_config/default_bundle")
        .is_some_and(|v| !v.is_null());
    if already_set {
        return Ok(());
    }

    // Stamp the field. `host_config` must exist (the apply engine writes
    // it), but guard defensively.
    if let Some(hc) = doc.get_mut("host_config").and_then(|v| v.as_object_mut()) {
        hc.insert(
            "default_bundle".to_string(),
            serde_json::Value::String(bundle_id.to_string()),
        );
        let updated =
            serde_json::to_string_pretty(&doc).context("serialize updated environment.json")?;
        atomic_write_bytes(&env_json_path, updated.as_bytes())
            .with_context(|| format!("atomic write {}", env_json_path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_deployer::cli::env_manifest::EnvManifest;
    use std::fs;
    use tempfile::tempdir;

    /// Build a minimal bundle directory with a `bundle-manifest.json`.
    fn make_bundle_dir_with_manifest(dir: &Path, bundle_id: &str) {
        fs::create_dir_all(dir).unwrap();
        // Minimum viable bundle: marker + manifest
        fs::write(
            dir.join(crate::bundle::BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\n",
        )
        .unwrap();
        let manifest = serde_json::json!({
            "format_version": "1",
            "bundle_id": bundle_id,
            "bundle_name": bundle_id,
            "requested_mode": "create",
            "locale": "en",
            "artifact_extension": "gtbundle",
        });
        fs::write(
            dir.join(BUNDLE_MANIFEST_JSON),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    /// Build a minimal bundle directory with `bundle.yaml` only (no
    /// `bundle-manifest.json`).
    fn make_bundle_dir_with_yaml_only(dir: &Path, bundle_id: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(crate::bundle::BUNDLE_WORKSPACE_MARKER),
            format!("schema_version: 1\nbundle_id: {bundle_id}\n"),
        )
        .unwrap();
    }

    // ── Schema guard ────────────────────────────────────────────────────

    #[test]
    fn synthesized_manifest_deserializes_into_env_manifest() {
        let manifest = json!({
            "schema": ENV_MANIFEST_SCHEMA_V1,
            "environment": { "id": "local" },
            "trust_root": "bootstrap",
            "bundles": [{
                "bundle_id": "my-bundle",
                "bundle_path": "/tmp/my-bundle.gtbundle",
            }]
        });
        let parsed: EnvManifest =
            serde_json::from_value(manifest).expect("manifest must deserialize into EnvManifest");
        assert_eq!(parsed.environment.id, "local");
        assert_eq!(parsed.bundles.len(), 1);
        assert_eq!(parsed.bundles[0].bundle_id, "my-bundle");
        assert_eq!(
            parsed.bundles[0].bundle_path.as_deref(),
            Some(std::path::Path::new("/tmp/my-bundle.gtbundle"))
        );
    }

    // ── bundle_path is absolute ─────────────────────────────────────────

    #[test]
    fn bundle_path_in_emitted_manifest_is_absolute() {
        let manifest = json!({
            "schema": ENV_MANIFEST_SCHEMA_V1,
            "environment": { "id": "local" },
            "trust_root": "bootstrap",
            "bundles": [{
                "bundle_id": "test",
                "bundle_path": "/absolute/path/to/bundle.gtbundle",
            }]
        });
        let path = manifest["bundles"][0]["bundle_path"].as_str().unwrap();
        assert!(
            std::path::Path::new(path).is_absolute(),
            "bundle_path must be absolute, got: {path}"
        );
    }

    // ── customer_id in synthesized manifest ───────────────────────────

    #[test]
    fn synthesized_manifest_includes_customer_id_when_provided() {
        // Exercises the REAL construction (`build_env_manifest`). An earlier
        // version of this test re-derived the manifest inline and so passed even
        // with the customer_id insertion deleted from production — a tautology.
        // The deployer's resolve_customer_id REQUIRES this field for every
        // non-local env; without it, `provider add` hard-fails after the
        // endpoint, secrets, and bundle link have already been committed.
        let manifest = build_env_manifest(
            "staging",
            "my-bundle",
            Path::new("/tmp/my-bundle.gtbundle"),
            Some("acme-billing"),
            "demo",
            None,
        );
        let parsed: EnvManifest =
            serde_json::from_value(manifest).expect("manifest must deserialize");
        assert_eq!(
            parsed.bundles[0].customer_id.as_deref(),
            Some("acme-billing"),
            "customer_id must be present in the synthesized manifest"
        );
    }

    #[test]
    fn synthesized_manifest_omits_customer_id_when_none() {
        let manifest = build_env_manifest(
            "local",
            "my-bundle",
            Path::new("/tmp/my-bundle.gtbundle"),
            None,
            "demo",
            None,
        );
        let parsed: EnvManifest =
            serde_json::from_value(manifest).expect("manifest must deserialize");
        assert!(
            parsed.bundles[0].customer_id.is_none(),
            "customer_id must be absent when not provided (local env defaults it)"
        );
    }

    // ── route_binding tenant_selector carries the setup tenant ──────────

    #[test]
    fn synthesized_manifest_binds_under_the_setup_tenant() {
        // The deployment must bind under the SAME tenant `gtc setup` wrote
        // secrets to, so the runtime resolves them directly. Without this the
        // deployer defaults the binding to `default` while the setup CLI
        // defaults to `demo`, and only the reader-side tenant fallback bridges
        // the gap. Guard both: the tenant is honored, and hosts/path_prefixes
        // stay empty so routing is unchanged from the previous implicit default.
        let manifest = build_env_manifest(
            "local",
            "my-bundle",
            Path::new("/tmp/my-bundle.gtbundle"),
            None,
            "demo",
            None,
        );
        let parsed: EnvManifest =
            serde_json::from_value(manifest).expect("manifest must deserialize");
        let rb = parsed.bundles[0]
            .route_binding
            .as_ref()
            .expect("route_binding must be stamped");
        let ts = rb
            .tenant_selector
            .as_ref()
            .expect("tenant_selector must be present");
        assert_eq!(ts.tenant, "demo", "binding must use the setup tenant");
        // A `/` match-all prefix satisfies the deployer's "tenant_selector
        // needs a matcher" rule while routing every request to the bundle,
        // exactly as the previous empty-matcher default binding did.
        assert_eq!(
            rb.path_prefixes,
            vec!["/".to_string()],
            "match-all `/` prefix keeps routing unchanged while satisfying the matcher rule"
        );
        assert!(rb.hosts.is_empty(), "no host matcher is added");
    }

    // ── route_binding tenant_selector carries the setup team ────────────

    /// Extract the stamped `(tenant, team)` pair, which is the whole surface
    /// these two tests care about.
    fn stamped_selector(tenant: &str, team: Option<&str>) -> (String, String) {
        let manifest = build_env_manifest(
            "local",
            "my-bundle",
            Path::new("/tmp/my-bundle.gtbundle"),
            None,
            tenant,
            team,
        );
        let parsed: EnvManifest =
            serde_json::from_value(manifest).expect("manifest must deserialize");
        let ts = parsed.bundles[0]
            .route_binding
            .as_ref()
            .expect("route_binding must be stamped")
            .tenant_selector
            .clone()
            .expect("tenant_selector must be present");
        (ts.tenant, ts.team)
    }

    #[test]
    fn synthesized_manifest_binds_under_the_setup_team() {
        // `team` was hardcoded to "default" here, so `--team` was accepted by
        // the CLI, parsed, threaded to the runtime — and silently discarded at
        // the one place that decides where the deployment BINDS. Secrets are
        // keyed on (tenant, team) (`deployer_secret_path`), so the write went
        // to the real team while the binding claimed `default`.
        assert_eq!(
            stamped_selector("acme", Some("billing")),
            ("acme".to_string(), "billing".to_string()),
            "an explicitly named team must reach the binding"
        );
    }

    #[test]
    fn absent_team_still_binds_default() {
        // `None` is "the caller named no team", and must keep producing the
        // exact binding every pre-existing deployment already carries —
        // otherwise this change silently re-binds live environments.
        assert_eq!(
            stamped_selector("demo", None),
            ("demo".to_string(), "default".to_string()),
            "no team named must stay byte-identical to the historical binding"
        );
    }

    // ── bundle_id from bundle-manifest.json, not filename ───────────────

    #[test]
    fn bundle_id_read_from_manifest_not_filename() {
        let temp = tempdir().unwrap();
        // Deliberately name the directory differently from the declared id.
        let dir = temp.path().join("wrong-filename");
        make_bundle_dir_with_manifest(&dir, "declared-bundle-id");

        let id = read_bundle_id_from_dir(&dir).unwrap();
        assert_eq!(
            id, "declared-bundle-id",
            "must use declared bundle_id, not directory name"
        );
    }

    // ── bundle.yaml fallback ────────────────────────────────────────────

    #[test]
    fn bundle_id_falls_back_to_bundle_yaml() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join("yaml-only");
        make_bundle_dir_with_yaml_only(&dir, "yaml-declared-id");

        let id = read_bundle_id_from_dir(&dir).unwrap();
        assert_eq!(id, "yaml-declared-id");
    }

    // ── neither file -> error ───────────────────────────────────────────

    #[test]
    fn missing_bundle_id_is_an_error() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join("empty-bundle");
        fs::create_dir_all(&dir).unwrap();
        // Write a bundle.yaml without bundle_id.
        fs::write(
            dir.join(crate::bundle::BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\n",
        )
        .unwrap();

        let err = read_bundle_id_from_dir(&dir).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("cannot determine bundle_id"),
            "expected clear error, got: {msg}"
        );
    }

    // ── non-bundle path -> clear error ──────────────────────────────────

    #[test]
    fn non_bundle_path_is_a_clear_error() {
        let temp = tempdir().unwrap();
        let not_a_bundle = temp.path().join("random.txt");
        fs::write(&not_a_bundle, "hello").unwrap();

        let err = deploy_bundle_to_env(&not_a_bundle, "local", true, true, None, "demo", None)
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not a .gtbundle"),
            "expected clear error for non-bundle file, got: {msg}"
        );
    }

    // ── default_bundle stamping (first deploy wins) ──────────────────

    /// Write a minimal `environment.json` into a temp store so
    /// `stamp_default_bundle_if_unset` has something to read.
    fn write_env_json(store: &LocalFsStore, env_id: &str, doc: &serde_json::Value) {
        let dir = store.root().join(env_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("environment.json"),
            serde_json::to_string_pretty(doc).unwrap(),
        )
        .unwrap();
    }

    fn read_env_json(store: &LocalFsStore, env_id: &str) -> serde_json::Value {
        let raw = fs::read_to_string(store.root().join(env_id).join("environment.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    /// Minimal environment.json skeleton — just enough for
    /// `stamp_default_bundle_if_unset` (it only touches `host_config`).
    fn env_json_skeleton(env_id: &str) -> serde_json::Value {
        json!({
            "schema": "greentic.environment.v1",
            "environment_id": env_id,
            "name": env_id,
            "host_config": {
                "env_id": env_id
            },
            "packs": [],
            "bundles": [],
            "revisions": [],
            "traffic_splits": [],
            "messaging_endpoints": [],
            "extensions": [],
            "revocation": {},
            "retention": {},
            "health": {}
        })
    }

    #[test]
    fn first_deploy_stamps_default_bundle() {
        let root = tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        write_env_json(&store, "local", &env_json_skeleton("local"));

        stamp_default_bundle_if_unset(&store, "local", "my-bundle").unwrap();

        let doc = read_env_json(&store, "local");
        assert_eq!(
            doc.pointer("/host_config/default_bundle")
                .and_then(|v| v.as_str()),
            Some("my-bundle"),
            "first deploy must stamp default_bundle"
        );
    }

    #[test]
    fn second_deploy_does_not_overwrite_default_bundle() {
        let root = tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        let mut doc = env_json_skeleton("local");
        doc["host_config"]["default_bundle"] = json!("first-bundle");
        write_env_json(&store, "local", &doc);

        stamp_default_bundle_if_unset(&store, "local", "second-bundle").unwrap();

        let doc = read_env_json(&store, "local");
        assert_eq!(
            doc.pointer("/host_config/default_bundle")
                .and_then(|v| v.as_str()),
            Some("first-bundle"),
            "second deploy must NOT overwrite the existing default_bundle"
        );
    }

    #[test]
    fn default_bundle_round_trips_through_json() {
        let root = tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        write_env_json(&store, "local", &env_json_skeleton("local"));

        stamp_default_bundle_if_unset(&store, "local", "round-trip-bundle").unwrap();

        // Re-read and re-parse to verify the value survives a write/read cycle.
        let raw = fs::read_to_string(store.root().join("local").join("environment.json")).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            doc.pointer("/host_config/default_bundle")
                .and_then(|v| v.as_str()),
            Some("round-trip-bundle"),
            "default_bundle must survive a JSON round-trip"
        );
        // The rest of the document must be intact.
        assert_eq!(
            doc.pointer("/environment_id").and_then(|v| v.as_str()),
            Some("local"),
        );
        assert_eq!(
            doc.pointer("/host_config/env_id").and_then(|v| v.as_str()),
            Some("local"),
        );
    }

    #[test]
    fn stamp_skips_when_env_json_is_missing() {
        let root = tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        // No environment.json at all.
        assert!(
            stamp_default_bundle_if_unset(&store, "local", "bundle").is_ok(),
            "missing environment.json must not be an error"
        );
    }

    #[test]
    fn nonexistent_path_is_a_clear_error() {
        let err = deploy_bundle_to_env(
            Path::new("/nonexistent/path.gtbundle"),
            "local",
            true,
            true,
            None,
            "demo",
            None,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("does not exist"),
            "expected clear error for missing path, got: {msg}"
        );
    }
}
