//! Pack extraction utilities for the CLI.
//!
//! Handles extracting .gtpack archives from file://, oci://, repo://, and store:// refs.
//! Includes automatic webchat GUI building and embedding.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use tar::Archive;
use zip::ZipArchive;

/// Extract pack from file:// or oci:// URL to bundle directory.
///
/// .gtpack files can be ZIP or tar.gz archives containing:
/// - components/  (WASM files)
/// - flows/       (.ygtc files)
/// - manifest.cbor
/// - sbom.cbor
/// - providers/   (optional provider configs)
pub fn extract_pack_to_bundle(pack_ref: &str, bundle_path: &Path) -> Result<()> {
    // Handle OCI references (oci://, repo://, store://)
    #[cfg(feature = "oci")]
    if pack_ref.starts_with("oci://") || pack_ref.starts_with("repo://") || pack_ref.starts_with("store://") {
        return extract_oci_pack_to_bundle(pack_ref, bundle_path);
    }

    #[cfg(not(feature = "oci"))]
    if pack_ref.starts_with("oci://") || pack_ref.starts_with("repo://") || pack_ref.starts_with("store://") {
        bail!("OCI references require the 'oci' feature. Build with: cargo build --features oci");
    }

    // Handle file:// URLs
    if !pack_ref.starts_with("file://") {
        println!("[greentic-setup] Non-file pack ref, skipping extraction: {}", pack_ref);
        return Ok(());
    }

    // Extract file path from file:// URL and delegate to shared implementation
    let pack_file_path = &pack_ref[7..]; // Remove "file://"
    let pack_file = Path::new(pack_file_path);
    extract_local_pack_to_bundle(pack_file, bundle_path)
}

/// Extract pack from OCI registry and extract to bundle directory.
///
/// Uses greentic-distributor-client to fetch the pack from OCI registry,
/// then extracts it like a local file:// pack.
#[cfg(feature = "oci")]
pub fn extract_oci_pack_to_bundle(pack_ref: &str, bundle_path: &Path) -> Result<()> {
    use greentic_distributor_client::oci_packs::fetch_pack_to_cache;
    use tokio::runtime::Runtime;

    println!("[greentic-setup] Fetching pack from OCI: {}", pack_ref);

    // Create tokio runtime for async fetch
    let rt = Runtime::new().context("Failed to create tokio runtime")?;

    // Fetch pack to local cache
    let resolved = rt.block_on(async {
        fetch_pack_to_cache(pack_ref).await
    }).with_context(|| format!("Failed to fetch pack from OCI: {}", pack_ref))?;

    println!("[greentic-setup] Pack cached at: {}", resolved.path.display());

    // Now extract like a file:// pack
    let pack_file = &resolved.path;
    extract_local_pack_to_bundle(pack_file, bundle_path)
}

/// Extract a local .gtpack file to bundle directory.
///
/// Shared implementation used by both file:// and oci:// refs.
pub fn extract_local_pack_to_bundle(pack_file: &Path, bundle_path: &Path) -> Result<()> {
    if !pack_file.exists() {
        bail!("Pack file not found: {}", pack_file.display());
    }

    println!("[greentic-setup] Extracting pack: {}", pack_file.display());

    // Create temp directory for extraction
    let temp_dir = tempfile::tempdir()
        .context("Failed to create temp directory")?;

    // Detect file format by reading magic bytes
    let mut magic = [0u8; 4];
    {
        let mut file = File::open(pack_file)?;
        file.read_exact(&mut magic).ok();
    }

    // Try ZIP first (PK magic bytes), then tar.gz
    if &magic[0..2] == b"PK" {
        extract_zip_archive(pack_file, temp_dir.path())?;
    } else {
        extract_tar_archive(pack_file, temp_dir.path())?;
    }

    // Copy components if present
    let temp_components = temp_dir.path().join("components");
    if temp_components.exists() {
        println!("[greentic-setup]   Copying components...");
        let bundle_components = bundle_path.join("components");
        std::fs::create_dir_all(&bundle_components)?;
        copy_dir_contents(&temp_components, &bundle_components)?;

        let wasm_count = count_files_with_extension(&bundle_components, "wasm");
        println!("[greentic-setup]   \u{2713} Copied {} component(s)", wasm_count);
    }

    // Copy flows if present
    let temp_flows = temp_dir.path().join("flows");
    if temp_flows.exists() {
        println!("[greentic-setup]   Copying flows...");
        let bundle_flows = bundle_path.join("flows");
        std::fs::create_dir_all(&bundle_flows)?;
        copy_dir_contents(&temp_flows, &bundle_flows)?;

        let flow_count = count_files_with_extension(&bundle_flows, "ygtc");
        println!("[greentic-setup]   \u{2713} Copied {} flow(s)", flow_count);
    }

    // Copy manifest if present
    let temp_manifest = temp_dir.path().join("manifest.cbor");
    if temp_manifest.exists() {
        std::fs::copy(&temp_manifest, bundle_path.join("manifest.cbor"))?;
        println!("[greentic-setup]   \u{2713} Copied manifest.cbor");
    }

    // Copy sbom if present
    let temp_sbom = temp_dir.path().join("sbom.cbor");
    if temp_sbom.exists() {
        std::fs::copy(&temp_sbom, bundle_path.join("sbom.cbor"))?;
        println!("[greentic-setup]   \u{2713} Copied sbom.cbor");
    }

    // Copy provider configs if present
    let temp_providers = temp_dir.path().join("providers");
    if temp_providers.exists() {
        let bundle_providers = bundle_path.join("providers");
        std::fs::create_dir_all(&bundle_providers)?;
        copy_dir_contents(&temp_providers, &bundle_providers)?;
    }

    // Copy .gtpack to providers/<domain>/ directory for operator discovery
    if let Some(pack_filename) = pack_file.file_name().and_then(|n| n.to_str()) {
        let domain = determine_pack_domain(pack_filename);
        let provider_domain_dir = bundle_path.join("providers").join(domain);
        std::fs::create_dir_all(&provider_domain_dir)?;
        let dest_pack = provider_domain_dir.join(pack_filename);
        std::fs::copy(pack_file, &dest_pack)?;
        println!("[greentic-setup]   \u{2713} Copied {} to providers/{}/", pack_filename, domain);

        // Handle webchat GUI embedding (auto-build if source available)
        if let Err(e) = handle_webchat_gui(pack_filename, bundle_path) {
            println!("[greentic-setup]   Warning: Failed to embed webchat GUI: {}", e);
        }
    }

    println!("[greentic-setup] \u{2713} Pack extracted to bundle");
    Ok(())
}

/// Extract ZIP archive to target directory.
fn extract_zip_archive(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let file = File::open(archive_path)
        .context(format!("Failed to open pack file: {}", archive_path.display()))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .context("Failed to open ZIP archive")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = target_dir.join(file.mangled_name());

        if file.is_dir() {
            std::fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut outfile = File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }
    Ok(())
}

/// Extract tar.gz or plain tar archive to target directory.
fn extract_tar_archive(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let file = File::open(archive_path)
        .context(format!("Failed to open pack file: {}", archive_path.display()))?;

    // Try gzip first
    let extract_result = {
        let gz = GzDecoder::new(BufReader::new(&file));
        let mut archive = Archive::new(gz);
        archive.unpack(target_dir)
    };

    // If gzip fails, try plain tar
    if extract_result.is_err() {
        let file = File::open(archive_path)?;
        let mut archive = Archive::new(BufReader::new(file));
        archive.unpack(target_dir)
            .context("Failed to extract pack (not a valid tar or ZIP archive)")?;
    }
    Ok(())
}

/// Copy all contents from src directory to dst directory.
pub fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir_contents(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Count files with a specific extension in a directory (recursive).
pub fn count_files_with_extension(dir: &Path, extension: &str) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_with_extension(&path, extension);
            } else if path.extension().is_some_and(|e| e == extension) {
                count += 1;
            }
        }
    }
    count
}

/// Determine the domain from pack filename.
///
/// e.g., "messaging-webchat.gtpack" -> "messaging"
pub fn determine_pack_domain(pack_filename: &str) -> &'static str {
    if pack_filename.starts_with("messaging-") {
        "messaging"
    } else if pack_filename.starts_with("events-") {
        "events"
    } else if pack_filename.starts_with("oauth-") {
        "oauth"
    } else if pack_filename.starts_with("secrets-") {
        "secrets"
    } else if pack_filename.starts_with("state-") {
        "state"
    } else {
        "other"
    }
}

/// Check if pack is a webchat pack.
fn is_webchat_pack(pack_filename: &str) -> bool {
    pack_filename.contains("webchat")
}

/// Find webchat SPA source directory.
///
/// Searches for greentic-webchat in common locations:
/// 1. GREENTIC_WEBCHAT_PATH environment variable (explicit override)
/// 2. Relative paths from bundle (../greentic-webchat, ../../greentic-webchat)
/// 3. Current working directory siblings
///
/// For production builds, the GUI should be embedded in the gtpack during pack build.
/// This function is mainly for local development when SPA source is available.
fn find_webchat_spa_source(bundle_path: &Path) -> Option<PathBuf> {
    // Priority 1: Explicit env var
    if let Ok(path) = std::env::var("GREENTIC_WEBCHAT_PATH") {
        let candidate = PathBuf::from(&path);
        let spa_dir = candidate.join("apps/webchat-spa");
        if spa_dir.exists() && spa_dir.join("package.json").exists() {
            return Some(candidate);
        }
        println!("[greentic-setup]   Warning: GREENTIC_WEBCHAT_PATH set but invalid: {}", path);
    }

    // Priority 2: Relative paths from bundle
    let candidates = [
        // From bundle parent (typical workspace layout: /workspace/bundle, /workspace/greentic-webchat)
        bundle_path.parent().map(|p| p.join("greentic-webchat")),
        // Two levels up (nested bundle: /workspace/demo/bundle, /workspace/greentic-webchat)
        bundle_path.parent().and_then(|p| p.parent()).map(|p| p.join("greentic-webchat")),
        // Current working directory sibling
        std::env::current_dir().ok().map(|p| p.join("greentic-webchat")),
        // Parent of cwd
        std::env::current_dir().ok().and_then(|p| p.parent().map(|pp| pp.join("greentic-webchat"))),
    ];

    for candidate in candidates.into_iter().flatten() {
        let spa_dir = candidate.join("apps/webchat-spa");
        if spa_dir.exists() && spa_dir.join("package.json").exists() {
            return Some(candidate);
        }
    }
    None
}

/// Build webchat SPA and return path to dist directory.
///
/// Runs `npm install` and `npm run build` in the webchat directory.
fn build_webchat_spa(webchat_dir: &Path) -> Result<PathBuf> {
    let spa_dir = webchat_dir.join("apps/webchat-spa");
    let dist_dir = spa_dir.join("dist");

    // Check if already built and recent (skip rebuild if dist exists)
    if dist_dir.exists() && dist_dir.join("index.html").exists() {
        println!("[greentic-setup]   Using existing webchat build: {}", dist_dir.display());
        return Ok(dist_dir);
    }

    println!("[greentic-setup]   Building webchat SPA...");

    // Check if node_modules exists, if not run npm install
    let node_modules = webchat_dir.join("node_modules");
    if !node_modules.exists() {
        println!("[greentic-setup]   Running npm install...");
        let status = Command::new("npm")
            .arg("install")
            .current_dir(webchat_dir)
            .status()
            .context("Failed to run npm install")?;

        if !status.success() {
            bail!("npm install failed with status: {}", status);
        }
    }

    // Run npm build
    println!("[greentic-setup]   Running npm run build...");
    let status = Command::new("npm")
        .arg("run")
        .arg("build")
        .current_dir(webchat_dir)
        .status()
        .context("Failed to run npm run build")?;

    if !status.success() {
        bail!("npm run build failed with status: {}", status);
    }

    if !dist_dir.exists() {
        bail!("Build completed but dist directory not found: {}", dist_dir.display());
    }

    println!("[greentic-setup]   \u{2713} Webchat SPA built successfully");
    Ok(dist_dir)
}

/// Embed webchat GUI into bundle's gui-cache directory.
///
/// The operator looks for GUI assets in `bundle/state/.gui-cache/webchat/`.
fn embed_webchat_gui(bundle_path: &Path, dist_dir: &Path) -> Result<()> {
    let gui_cache = bundle_path.join("state").join(".gui-cache").join("webchat");

    // Clean and recreate
    let _ = std::fs::remove_dir_all(&gui_cache);
    std::fs::create_dir_all(&gui_cache)?;

    // Copy dist contents
    copy_dir_contents(dist_dir, &gui_cache)?;

    let file_count = count_files_in_dir(&gui_cache);
    println!("[greentic-setup]   \u{2713} Embedded {} GUI files to state/.gui-cache/webchat/", file_count);
    Ok(())
}

/// Count files in directory (recursive).
fn count_files_in_dir(dir: &Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_in_dir(&path);
            } else {
                count += 1;
            }
        }
    }
    count
}

/// Handle webchat GUI building and embedding for webchat packs.
///
/// This is called during pack extraction to automatically build and embed
/// the webchat GUI if the source is available locally.
pub fn handle_webchat_gui(pack_filename: &str, bundle_path: &Path) -> Result<()> {
    if !is_webchat_pack(pack_filename) {
        return Ok(());
    }

    println!("[greentic-setup] Detected webchat pack, looking for GUI source...");

    let webchat_dir = match find_webchat_spa_source(bundle_path) {
        Some(dir) => {
            println!("[greentic-setup]   Found webchat source: {}", dir.display());
            dir
        }
        None => {
            println!("[greentic-setup]   Webchat source not found, skipping GUI embed");
            println!("[greentic-setup]   Set GREENTIC_WEBCHAT_PATH env var to specify location");
            return Ok(());
        }
    };

    // Build SPA
    let dist_dir = build_webchat_spa(&webchat_dir)?;

    // Embed into bundle
    embed_webchat_gui(bundle_path, &dist_dir)?;

    Ok(())
}

/// Extract provider ID from pack ref.
///
/// Examples:
/// - "file://./packs/messaging-webchat.gtpack" -> "messaging-webchat"
/// - "oci://ghcr.io/greenticai/greentic-packs/messaging-webchat:0.4.34" -> "messaging-webchat"
/// - "repo://greentic/messaging-webchat" -> "messaging-webchat"
pub fn get_provider_id_from_pack_ref(pack_ref: &str) -> Option<String> {
    // Handle OCI refs (oci://registry/org/repo/pack-name:tag)
    if pack_ref.starts_with("oci://") || pack_ref.starts_with("repo://") || pack_ref.starts_with("store://") {
        // Extract the pack name from OCI ref (last path segment before :tag)
        let without_prefix = pack_ref.split("://").nth(1)?;
        let without_tag = without_prefix.split(':').next()?;
        let pack_name = without_tag.rsplit('/').next()?;
        return Some(pack_name.to_string());
    }

    // Handle file:// URLs
    let path_str = if pack_ref.starts_with("file://") {
        &pack_ref[7..]
    } else {
        pack_ref
    };

    std::path::Path::new(path_str)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_provider_id_from_file_ref() {
        assert_eq!(
            get_provider_id_from_pack_ref("file:///path/to/messaging-webchat.gtpack"),
            Some("messaging-webchat".to_string())
        );
    }

    #[test]
    fn test_get_provider_id_from_oci_ref() {
        assert_eq!(
            get_provider_id_from_pack_ref("oci://ghcr.io/org/repo/messaging-webchat:0.4.34"),
            Some("messaging-webchat".to_string())
        );
    }

    #[test]
    fn test_determine_pack_domain() {
        assert_eq!(determine_pack_domain("messaging-webchat.gtpack"), "messaging");
        assert_eq!(determine_pack_domain("events-webhook.gtpack"), "events");
        assert_eq!(determine_pack_domain("state-redis.gtpack"), "state");
        assert_eq!(determine_pack_domain("custom-pack.gtpack"), "other");
    }
}
