//! Bundle resolution and pack handling.
//!
//! Functions for resolving bundle sources (directories, .gtbundle files, URLs)
//! and managing pack files.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use url::Url;

use crate::cli_i18n::CliI18n;

/// Resolve bundle source - supports both directories and .gtbundle files.
pub fn resolve_bundle_source(path: &std::path::Path, i18n: &CliI18n) -> Result<PathBuf> {
    use crate::gtbundle;

    let path_str = path.to_string_lossy();
    if path_str.starts_with("https://") || path_str.starts_with("http://") {
        println!("{}", i18n.t("cli.simple.extracting"));
        let temp_dir = download_and_extract_remote_bundle(&path_str)
            .context("failed to fetch and extract remote bundle archive")?;
        println!(
            "{}",
            i18n.tf(
                "cli.simple.extracted_to",
                &[&temp_dir.display().to_string()]
            )
        );
        return Ok(temp_dir);
    }

    if gtbundle::is_gtbundle_file(path) {
        println!("{}", i18n.t("cli.simple.extracting"));
        let temp_dir = gtbundle::extract_gtbundle_to_temp(path)
            .context("failed to extract .gtbundle archive")?;
        println!(
            "{}",
            i18n.tf(
                "cli.simple.extracted_to",
                &[&temp_dir.display().to_string()]
            )
        );
        return Ok(temp_dir);
    }

    if gtbundle::is_gtbundle_dir(path) {
        return Ok(path.to_path_buf());
    }
    if path_str.ends_with(".gtbundle") && !path.exists() {
        bail!(
            "{}",
            i18n.tf(
                "setup.error.bundle_not_found",
                &[&path.display().to_string()]
            )
        );
    }

    if path.is_dir() {
        Ok(path.to_path_buf())
    } else if path.exists() {
        bail!(
            "{}",
            i18n.tf(
                "cli.simple.expected_bundle_format",
                &[&path.display().to_string()]
            )
        );
    } else {
        bail!(
            "{}",
            i18n.tf(
                "setup.error.bundle_not_found",
                &[&path.display().to_string()]
            )
        );
    }
}

/// Persistent output target for simple setup flows.
pub enum SetupOutputTarget {
    Directory(PathBuf),
    Archive(PathBuf),
}

/// Decide whether simple setup should materialize a configured local bundle.
///
/// - For remote `https://.../*.gtbundle`, write `./<file-name>.gtbundle` as a
///   local bundle directory, matching the normal `gtc start ./demo.gtbundle`
///   workspace flow.
/// - For local archive paths, update that same archive in place.
/// - For local bundle directories, keep working in the directory and do not emit
///   a new artifact automatically.
pub fn setup_output_target(source: &Path) -> Result<Option<SetupOutputTarget>> {
    let source_str = source.to_string_lossy();

    if source_str.starts_with("https://") || source_str.starts_with("http://") {
        let parsed =
            Url::parse(&source_str).with_context(|| format!("invalid bundle URL: {source_str}"))?;
        let file_name = parsed
            .path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .filter(|segment| segment.ends_with(".gtbundle"))
            .ok_or_else(|| {
                anyhow::anyhow!("remote bundle URL must point to a .gtbundle archive")
            })?;
        return Ok(Some(SetupOutputTarget::Directory(
            std::env::current_dir()?.join(file_name),
        )));
    }

    if source_str.ends_with(".gtbundle") {
        return Ok(Some(SetupOutputTarget::Archive(source.to_path_buf())));
    }

    Ok(None)
}

/// Download and extract a remote bundle archive.
fn download_and_extract_remote_bundle(url: &str) -> Result<PathBuf> {
    use crate::gtbundle;

    let response = ureq::get(url)
        .call()
        .map_err(|err| anyhow::anyhow!("failed to fetch {url}: {err}"))?;
    let bytes = response
        .into_body()
        .read_to_vec()
        .map_err(|err| anyhow::anyhow!("failed to read {url}: {err}"))?;

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = std::env::temp_dir().join(format!("greentic-setup-remote-{nonce}"));
    fs::create_dir_all(&base)?;

    let file_name = url
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("bundle.gtbundle");
    let archive_path = base.join(file_name);
    fs::write(&archive_path, bytes)?;

    if !gtbundle::is_gtbundle_file(&archive_path) {
        bail!("remote bundle URL must point to a .gtbundle archive: {url}");
    }

    gtbundle::extract_gtbundle_to_temp(&archive_path)
}

/// Resolve bundle directory from optional path argument.
pub fn resolve_bundle_dir(bundle: Option<PathBuf>) -> Result<PathBuf> {
    match bundle {
        Some(path) => Ok(path),
        None => std::env::current_dir().context("failed to get current directory"),
    }
}

/// Recursively copy a directory tree.
pub fn copy_dir_recursive(src: &PathBuf, dst: &PathBuf, _only_used: bool) -> Result<()> {
    if !src.is_dir() {
        bail!("source is not a directory: {}", src.display());
    }

    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path, _only_used)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

/// Detect provider domain from .gtpack filename prefix.
///
/// Known prefixes: messaging-, state-, telemetry-, events-, oauth-, secrets-.
/// Falls back to "messaging" for unrecognized prefixes.
pub fn detect_domain_from_filename(filename: &str) -> &'static str {
    let stem = filename.strip_suffix(".gtpack").unwrap_or(filename);
    if stem.starts_with("messaging-")
        || stem.starts_with("state-")
        || stem.starts_with("telemetry-")
    {
        "messaging"
    } else if stem.starts_with("events-") || stem.starts_with("event-") {
        "events"
    } else if stem.starts_with("oauth-") {
        "oauth"
    } else if stem.starts_with("secrets-") {
        "secrets"
    } else {
        "messaging"
    }
}

/// Resolve a pack source (local path or OCI reference) to a local file path.
pub fn resolve_pack_source(source: &str) -> Result<PathBuf> {
    use crate::bundle_source::BundleSource;

    let parsed = BundleSource::parse(source)?;

    if parsed.is_local() {
        let path = parsed.resolve()?;
        if path.extension().and_then(|e| e.to_str()) != Some("gtpack") {
            anyhow::bail!("Not a .gtpack file: {source}");
        }
        Ok(path)
    } else {
        println!("    Fetching from registry...");
        let path = parsed.resolve()?;
        println!("    Downloaded to cache: {}", path.display());
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{SetupOutputTarget, setup_output_target};
    use std::env;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn setup_output_target_uses_cwd_file_name_directory_for_remote_bundle_urls() {
        let cwd = env::current_dir().expect("cwd");
        let dir = tempdir().expect("tempdir");
        env::set_current_dir(dir.path()).expect("set cwd");

        let output = setup_output_target(Path::new(
            "https://github.com/greenticai/greentic-demo/releases/download/v0.1.9/cloud-deploy-demo.gtbundle",
        ))
        .expect("output target")
        .expect("some output target");

        match output {
            SetupOutputTarget::Directory(path) => {
                assert_eq!(path, dir.path().join("cloud-deploy-demo.gtbundle"));
            }
            SetupOutputTarget::Archive(path) => {
                panic!("expected directory output, got archive {}", path.display());
            }
        }

        env::set_current_dir(cwd).expect("restore cwd");
    }

    #[test]
    fn setup_output_target_updates_local_archives_in_place() {
        let archive = Path::new("/tmp/demo-bundle.gtbundle");
        let output = setup_output_target(archive)
            .expect("output target")
            .expect("some output target");
        match output {
            SetupOutputTarget::Archive(path) => assert_eq!(path, archive),
            SetupOutputTarget::Directory(path) => {
                panic!("expected archive output, got directory {}", path.display());
            }
        }
    }

    #[test]
    fn setup_output_target_skips_local_bundle_directories() {
        let output = setup_output_target(Path::new("./demo-bundle")).expect("output target");
        assert!(output.is_none());
    }
}
