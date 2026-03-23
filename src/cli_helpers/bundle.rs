//! Bundle resolution and pack handling.
//!
//! Functions for resolving bundle sources (directories, .gtbundle files, URLs)
//! and managing pack files.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

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
