//! Crash-safe writer for `<bundle_root>/state/pack-configs/<pack_id>.json`.
//!
//! The invariant this module holds: after a write attempt, the final path is
//! either a complete, parseable `pack-config-input.v1` document or absent. It
//! never holds a truncated one. greentic-deployer refuses an unparseable
//! pack-config input at runtime boot, and a bundle carrying one kills every
//! container in the environment, whereas an ABSENT file only drops back to the
//! C4.2 DevStore compatibility shim.

use std::path::Path;

use anyhow::{Context, Result};

use super::PackConfigInput;

/// Production writer: greentic-deployer's own atomic write. It writes to a
/// hidden `NamedTempFile` in the same directory (so the rename stays on one
/// filesystem), `sync_all`s it, renames it over `path`, then fsyncs the
/// directory. The temp file is removed on drop if any step fails, so no stray
/// `.tmp*` sibling is left behind for a deployer that scans the directory.
pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    greentic_deployer::environment::atomic_write_bytes(path, bytes)
        .with_context(|| format!("write pack-config-input {}", path.display()))
}

/// Write `bytes` to `path` through `write`; on failure, remove whatever is at
/// `path` unless it is still a valid pack-config input.
///
/// No read-back on success, deliberately: `write` is an fsynced temp file
/// renamed over the target, and a rename is atomic, so on `Ok` the path holds
/// exactly the bytes we serialized a moment ago from a `PackConfigInput`.
/// Parsing them again could only fail if the filesystem lied about the
/// fsync, which no amount of reading back in-process would detect either.
///
/// On failure the rename either never happened (the previous file, if any, is
/// untouched) or `write` is a non-atomic writer that may have left a partial
/// file. We do not trust which: we re-read the path and remove anything that
/// does not parse. A still-valid file from an earlier run is kept — it is a
/// complete answer set, and removing it would turn a transient write failure
/// into lost configuration.
pub(super) fn write_or_remove_corrupt(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let Err(err) = write(path, bytes) else {
        return Ok(());
    };
    match remove_if_unparseable(path) {
        Ok(Leftover::None) => Err(err.context("no pack-config-input file was left behind")),
        Ok(Leftover::PreviousValid) => Err(err.context(
            "the pack-config-input from a previous run was kept; it does not carry these answers",
        )),
        Err(cleanup) => Err(err.context(format!(
            "and an unparseable pack-config-input may remain at {}: {cleanup:#}",
            path.display()
        ))),
    }
}

enum Leftover {
    /// Nothing is at the path any more (or never was).
    None,
    /// A complete document from an earlier write is still there.
    PreviousValid,
}

fn remove_if_unparseable(path: &Path) -> Result<Leftover> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Leftover::None),
        // Unreadable means unverifiable: treat it like a corrupt file rather
        // than ship something the deployer may not be able to read either.
        Err(_) => return remove(path),
    };
    if serde_json::from_slice::<PackConfigInput>(&bytes).is_ok() {
        return Ok(Leftover::PreviousValid);
    }
    remove(path)
}

fn remove(path: &Path) -> Result<Leftover> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(Leftover::None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Leftover::None),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa::persist::{PACK_CONFIG_INPUT_SCHEMA, emit_pack_config_input};
    use qa_spec::{FormSpec, QuestionSpec, QuestionType};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn valid_doc() -> Vec<u8> {
        let input = PackConfigInput {
            schema: PACK_CONFIG_INPUT_SCHEMA.to_string(),
            pack_id: "p".into(),
            env_id: "local".into(),
            bundle_id: "b".into(),
            non_secret: BTreeMap::from([("enabled".into(), json!(true))]),
            secret_refs: BTreeMap::new(),
        };
        serde_json::to_vec_pretty(&input).expect("serialize")
    }

    fn form() -> FormSpec {
        FormSpec {
            id: "f".into(),
            title: "f".into(),
            version: "1".into(),
            description: None,
            presentation: None,
            progress_policy: None,
            secrets_policy: None,
            store: vec![],
            validations: vec![],
            includes: vec![],
            questions: vec![QuestionSpec {
                id: "enabled".into(),
                kind: QuestionType::Boolean,
                title: "enabled".into(),
                title_i18n: None,
                description: None,
                description_i18n: None,
                required: false,
                choices: None,
                default_value: None,
                secret: false,
                visible_if: None,
                constraint: None,
                list: None,
                computed: None,
                policy: Default::default(),
                computed_overridable: false,
            }],
        }
    }

    /// The simulated failure the production bug produced: the target got
    /// truncated/created, then the write errored.
    fn truncate_then_fail(path: &Path, _bytes: &[u8]) -> Result<()> {
        std::fs::write(path, b"")?;
        anyhow::bail!("simulated ENOSPC")
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// (a) + (b): a normal emit yields a parseable document and nothing else
    /// in the directory — no leftover temp sibling.
    #[test]
    fn pack_config_emit_is_parseable_and_leaves_no_temp_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = emit_pack_config_input(
            tmp.path(),
            "local",
            "b",
            "messaging-webchat-gui",
            &json!({"enabled": true}),
            &form(),
        )
        .expect("emit")
        .expect("path");
        let parsed: PackConfigInput =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("parse");
        assert_eq!(parsed.pack_id, "messaging-webchat-gui");
        assert_eq!(
            entries(path.parent().expect("parent")),
            vec!["messaging-webchat-gui.json".to_string()]
        );
    }

    /// A 0-byte file left by an earlier (pre-fix) run is replaced, not kept.
    #[test]
    fn pack_config_emit_replaces_an_existing_truncated_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("p.json");
        std::fs::write(&path, b"").expect("seed");
        write_or_remove_corrupt(&path, &valid_doc(), atomic_write).expect("write");
        serde_json::from_slice::<PackConfigInput>(&std::fs::read(&path).expect("read"))
            .expect("parse");
    }

    /// (c): a write that fails after truncating the target leaves NO file.
    #[test]
    fn pack_config_failed_write_leaves_no_truncated_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("p.json");
        let err = write_or_remove_corrupt(&path, &valid_doc(), truncate_then_fail)
            .expect_err("must fail");
        assert!(format!("{err:#}").contains("simulated ENOSPC"));
        assert!(!path.exists(), "truncated file must be removed");
        assert!(entries(tmp.path()).is_empty());
    }

    /// (c): a failure over a garbage file from an earlier run removes it too.
    #[test]
    fn pack_config_failed_write_removes_a_preexisting_corrupt_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("p.json");
        std::fs::write(&path, b"{\"schema\":").expect("seed");
        write_or_remove_corrupt(&path, &valid_doc(), |_, _| anyhow::bail!("boom"))
            .expect_err("must fail");
        assert!(!path.exists());
    }

    /// A failure over a still-valid earlier document keeps it.
    #[test]
    fn pack_config_failed_write_keeps_a_previous_valid_file() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("p.json");
        std::fs::write(&path, valid_doc()).expect("seed");
        write_or_remove_corrupt(&path, b"{}", |_, _| anyhow::bail!("boom")).expect_err("must fail");
        assert_eq!(std::fs::read(&path).expect("read"), valid_doc());
    }

    /// (c) with the REAL writer: a directory we cannot create files in makes
    /// the atomic write fail before the rename, and neither the final file
    /// nor a temp sibling appears.
    #[cfg(unix)]
    #[test]
    fn pack_config_real_writer_failure_leaves_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let dir = tmp.path().join("pack-configs");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        // Root ignores directory permissions; the test proves nothing there.
        if std::fs::write(dir.join("probe"), b"").is_ok() {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
            return;
        }
        let path = dir.join("p.json");
        let result = write_or_remove_corrupt(&path, &valid_doc(), atomic_write);
        let listing = entries(&dir);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        result.expect_err("must fail in a read-only dir");
        assert!(listing.is_empty(), "left behind: {listing:?}");
    }
}
