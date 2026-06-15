//! Env-manifest mode: route a `greentic.env-manifest.v1` answers document
//! to the deployer's env-apply engine (operator-surface PR-4).
//!
//! `greentic-setup --answers <manifest>` is porcelain over
//! `greentic_deployer::cli::env_apply::apply` — the same engine behind
//! `gtc op env apply`. The manifest stays the durable artifact; setup adds
//! routing (schema sniff), the `--env` cross-check, and flag mapping
//! (`--dry-run` → `ApplyMode::DryRun`, `--non-interactive` → the headless
//! contract). Env mode never starts the web UI: the manifest names every
//! input, and the engine's missing-inputs contract + TTY fill-in own the
//! gaps.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::env_apply::{self, ApplyMode, ApplyOptions};
use greentic_deployer::cli::env_manifest::ENV_MANIFEST_SCHEMA_V1;
use greentic_deployer::cli::{OpFlags, OpOutcome, dispatch::print_outcome};
use greentic_deployer::environment::LocalFsStore;
use greentic_deployer::runtime_secrets::SecretValue;
use serde_json::Value;

/// Positive identification of an env-manifest answers document.
///
/// Returns the parsed document when `path` reads and parses (JSON first,
/// YAML fallback — mirroring the engine's own `--answers` loader) as an
/// object whose `schema` is exactly [`ENV_MANIFEST_SCHEMA_V1`]. Every
/// failure mode (unreadable file, parse error, any other shape) returns
/// `None` so the caller falls through to the existing bundle-onboarding
/// path and its established error messages — the sniff must never invent
/// a new failure for a document it doesn't own.
pub fn sniff_env_manifest(path: &Path) -> Option<Value> {
    let bytes = std::fs::read(path).ok()?;
    let doc: Value = serde_json::from_slice(&bytes)
        .ok()
        .or_else(|| serde_yaml_bw::from_slice(&bytes).ok())?;
    (doc.get("schema").and_then(Value::as_str) == Some(ENV_MANIFEST_SCHEMA_V1)).then_some(doc)
}

/// Env-mode entry: build the per-user store (the same default root
/// `gtc op` uses) and run the engine, printing the standard
/// `{op, noun, result}` envelope on stdout — stderr carries the engine's
/// human plan rows, byte-compatible with `gtc op env apply`.
pub fn run_env_apply(
    answers_path: &Path,
    manifest: &Value,
    resolved_env: &str,
    dry_run: bool,
    non_interactive: bool,
    prefilled_secrets: BTreeMap<String, SecretValue>,
) -> Result<()> {
    let root = LocalFsStore::default_root()
        .context("cannot locate the environment store: HOME / USERPROFILE not set")?;
    let store = LocalFsStore::new(root);
    let outcome = apply_manifest_with_store(
        &store,
        answers_path,
        manifest,
        resolved_env,
        dry_run,
        non_interactive,
        prefilled_secrets,
    )?;
    print_outcome(&outcome)?;
    Ok(())
}

/// Engine call with the store injected (tests pass a temp-rooted store).
///
/// The `--env` cross-check happens here: when the manifest names an
/// environment id that differs from the resolved `--env`, that is an
/// error — the manifest is never silently overridden, and the flag never
/// silently retargets the manifest. A manifest without a readable
/// `environment.id` is NOT rejected here; the engine's validation owns
/// that diagnosis and reports it precisely.
pub fn apply_manifest_with_store(
    store: &LocalFsStore,
    answers_path: &Path,
    manifest: &Value,
    resolved_env: &str,
    dry_run: bool,
    non_interactive: bool,
    prefilled_secrets: BTreeMap<String, SecretValue>,
) -> Result<OpOutcome> {
    if let Some(manifest_env) = manifest.pointer("/environment/id").and_then(Value::as_str)
        && manifest_env != resolved_env
    {
        bail!(
            "answers manifest `{}` targets environment `{manifest_env}` but --env resolves to \
             `{resolved_env}`; pass `--env {manifest_env}` to apply it (the manifest is never \
             silently overridden)",
            answers_path.display(),
        );
    }
    let flags = OpFlags {
        schema_only: false,
        answers: Some(answers_path.to_path_buf()),
    };
    let opts = ApplyOptions {
        mode: if dry_run {
            ApplyMode::DryRun
        } else {
            ApplyMode::Apply
        },
        // Audit principal: mutations composed through setup's porcelain say so.
        updated_by: Some("greentic-setup".to_string()),
        yes: false,
        non_interactive,
        prefilled_secrets,
    };
    env_apply::apply(store, &flags, opts)
        .with_context(|| format!("env-manifest apply failed for `{}`", answers_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn manifest_value(env_id: &str) -> Value {
        json!({"schema": ENV_MANIFEST_SCHEMA_V1, "environment": {"id": env_id}})
    }

    #[test]
    fn sniff_identifies_manifest_json_and_yaml() {
        let dir = tempfile::tempdir().unwrap();
        let json_path = write(dir.path(), "env.json", &manifest_value("local").to_string());
        assert!(sniff_env_manifest(&json_path).is_some());

        let yaml_path = write(
            dir.path(),
            "env.yaml",
            &format!("schema: {ENV_MANIFEST_SCHEMA_V1}\nenvironment:\n  id: local\n"),
        );
        assert!(sniff_env_manifest(&yaml_path).is_some());
    }

    #[test]
    fn sniff_falls_through_on_everything_else() {
        let dir = tempfile::tempdir().unwrap();
        // Bundle-shaped setup answers: valid JSON, no env-manifest schema.
        let bundle_answers = write(
            dir.path(),
            "answers.json",
            r#"{"setup_answers": {"telegram": {"token": "secret://x"}}}"#,
        );
        assert!(sniff_env_manifest(&bundle_answers).is_none());
        // A different schema string.
        let other_schema = write(
            dir.path(),
            "other.json",
            r#"{"schema": "greentic.something-else.v1"}"#,
        );
        assert!(sniff_env_manifest(&other_schema).is_none());
        // Unparseable and missing files fall through silently — the bundle
        // path owns their (established) error messages.
        let garbage = write(dir.path(), "garbage.bin", "\u{0}\u{1}not-a-doc{{{");
        assert!(sniff_env_manifest(&garbage).is_none());
        assert!(sniff_env_manifest(&dir.path().join("missing.json")).is_none());
    }

    #[test]
    fn env_mismatch_is_an_error_not_an_override() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(dir.path().join("store"));
        let manifest = manifest_value("demo");
        let path = write(dir.path(), "demo.env.json", &manifest.to_string());

        let err = apply_manifest_with_store(
            &store,
            &path,
            &manifest,
            "local",
            false,
            true,
            Default::default(),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`demo`"), "names the manifest env: {msg}");
        assert!(msg.contains("`local`"), "names the --env value: {msg}");
        // Rejected before the engine ran: the store root was never created.
        assert!(!dir.path().join("store").exists());
    }

    #[test]
    fn dry_run_routes_to_the_engine_and_mutates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = LocalFsStore::new(&store_root);
        let manifest = manifest_value("local");
        let path = write(dir.path(), "local.env.json", &manifest.to_string());

        let outcome = apply_manifest_with_store(
            &store,
            &path,
            &manifest,
            "local",
            true,
            true,
            Default::default(),
        )
        .expect("dry-run succeeds");
        assert_eq!(outcome.noun, "env");
        assert_eq!(outcome.op, "apply");
        assert_eq!(outcome.result["mode"], "dry-run", "{}", outcome.result);
        // A preview never mutates: the store root is still empty.
        assert_eq!(std::fs::read_dir(&store_root).unwrap().count(), 0);
    }

    #[test]
    fn non_interactive_missing_secret_surfaces_the_engine_report() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(dir.path().join("store"));
        let manifest = json!({
            "schema": ENV_MANIFEST_SCHEMA_V1,
            "environment": {"id": "local"},
            "trust_root": "bootstrap",
            "secrets": [{
                "path": "default/_/messaging-telegram/telegram_bot_token",
                "from_env": "GREENTIC_SETUP_PR4_TEST_UNSET"
            }]
        });
        let path = write(dir.path(), "local.env.json", &manifest.to_string());

        let err = apply_manifest_with_store(
            &store,
            &path,
            &manifest,
            "local",
            false,
            true,
            Default::default(),
        )
        .unwrap_err();
        // The engine's missing-inputs report surfaces verbatim through the
        // anyhow chain: count, env-var name, and manifest secret path.
        let msg = format!("{err:#}");
        assert!(msg.contains("missing input(s)"), "got: {msg}");
        assert!(msg.contains("GREENTIC_SETUP_PR4_TEST_UNSET"), "got: {msg}");
        assert!(
            msg.contains("default/_/messaging-telegram/telegram_bot_token"),
            "got: {msg}"
        );
    }
}
