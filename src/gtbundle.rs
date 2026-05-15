//! .gtbundle archive format support.
//!
//! A `.gtbundle` file is an archive containing a complete Greentic bundle.
//! Supports both SquashFS (default) and ZIP formats.
//!
//! ## Format
//!
//! ```text
//! my-bundle.gtbundle (SquashFS or ZIP archive)
//! ├── greentic.demo.yaml or bundle.yaml
//! ├── packs/
//! ├── providers/
//! ├── resolved/
//! ├── state/
//! └── tenants/
//! ```

use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// Archive format for gtbundle files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFormat {
    /// SquashFS format (read-only compressed filesystem)
    #[cfg(feature = "squashfs")]
    SquashFs,
    /// ZIP format (portable compressed archive)
    Zip,
}

// Feature-conditional default: SquashFs when `squashfs` feature enabled, otherwise Zip.
// Cannot use `#[derive(Default)]` with conditional `#[default]` attributes.
#[allow(clippy::derivable_impls)]
impl Default for BundleFormat {
    fn default() -> Self {
        #[cfg(feature = "squashfs")]
        {
            Self::SquashFs
        }
        #[cfg(not(feature = "squashfs"))]
        {
            Self::Zip
        }
    }
}

/// Detect the format of a gtbundle file by reading its magic bytes.
pub fn detect_bundle_format(path: &Path) -> Result<BundleFormat> {
    let mut file = File::open(path).context("failed to open bundle file")?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)
        .context("failed to read magic bytes")?;

    // SquashFS magic: "hsqs" (little-endian) or "sqsh" (big-endian)
    if &magic == b"hsqs" || &magic == b"sqsh" {
        #[cfg(feature = "squashfs")]
        return Ok(BundleFormat::SquashFs);
        #[cfg(not(feature = "squashfs"))]
        bail!("squashfs format detected but squashfs feature is not enabled");
    }

    // ZIP magic: PK\x03\x04
    if &magic == b"PK\x03\x04" {
        return Ok(BundleFormat::Zip);
    }

    bail!("unknown archive format (magic: {:?})", magic);
}

/// Create a .gtbundle archive from a bundle directory using the default format.
///
/// # Arguments
/// * `bundle_dir` - Source bundle directory
/// * `output_path` - Destination .gtbundle file path
///
/// # Example
/// ```ignore
/// create_gtbundle(Path::new("./my-bundle"), Path::new("./dist/my-bundle.gtbundle"))?;
/// ```
pub fn create_gtbundle(bundle_dir: &Path, output_path: &Path) -> Result<()> {
    create_gtbundle_with_format(bundle_dir, output_path, BundleFormat::default())
}

/// Create a .gtbundle archive with a specific format.
pub fn create_gtbundle_with_format(
    bundle_dir: &Path,
    output_path: &Path,
    format: BundleFormat,
) -> Result<()> {
    // Phase 0 secret-leak hotfix is enforced inline by the per-format writer
    // walkers (add_directory_to_squashfs / add_directory_to_zip): they skip
    // dev-store paths (.greentic/dev/, .greentic/state/dev/, .dev.secrets.env)
    // and bail on any symlink. Doing it in the same walk that reads bytes
    // closes the preflight-vs-writer TOCTOU window that Codex's adversarial
    // review flagged on the earlier denylist approach.
    // See plans/next-gen-deployment.md P0.1.
    match format {
        #[cfg(feature = "squashfs")]
        BundleFormat::SquashFs => create_gtbundle_squashfs(bundle_dir, output_path),
        BundleFormat::Zip => create_gtbundle_zip(bundle_dir, output_path),
    }
}

// Phase 0 secret-leak hotfix matcher. Used by the writer walkers below to
// skip dev-store paths from the archive — `.greentic/dev/`,
// `.greentic/state/dev/`, and any `.dev.secrets.env` file. These are the
// dev-store paths declared in `greentic-setup/src/secrets.rs:STORE_RELATIVE
// / STORE_STATE_RELATIVE`. Skipping (vs. bailing) lets the normal setup
// flow round-trip: ApplyPackSetup writes the dev store under the bundle
// root, the post-setup repack call here ignores those paths instead of
// erroring out, and the secrets stay on disk for runtime use until Phase B
// migrates the in-memory map to SecretRef.
fn dev_secret_match(relative: &Path) -> Option<&'static str> {
    let parts: Vec<&str> = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    for window in parts.windows(2) {
        if window[0] == ".greentic" && window[1] == "dev" {
            return Some(".greentic/dev/ tree");
        }
    }
    for window in parts.windows(3) {
        if window[0] == ".greentic" && window[1] == "state" && window[2] == "dev" {
            return Some(".greentic/state/dev/ tree");
        }
    }
    if parts.last().copied() == Some(".dev.secrets.env") {
        return Some(".dev.secrets.env file");
    }
    None
}

/// Create a .gtbundle archive using SquashFS format.
#[cfg(feature = "squashfs")]
fn create_gtbundle_squashfs(bundle_dir: &Path, output_path: &Path) -> Result<()> {
    use backhand::FilesystemWriter;

    if !bundle_dir.is_dir() {
        bail!("bundle directory not found: {}", bundle_dir.display());
    }

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).context("failed to create output directory")?;
    }

    let mut writer = FilesystemWriter::default();
    // The root inode header inherits `NodeHeader::default()` (mode 0o000)
    // unless we override it — same trap as the per-entry headers below.
    writer.set_root_mode(0o755);

    let result = (|| -> Result<()> {
        add_directory_to_squashfs(&mut writer, bundle_dir, bundle_dir)?;
        let mut output = File::create(output_path)
            .with_context(|| format!("failed to create archive: {}", output_path.display()))?;
        writer
            .write(&mut output)
            .context("failed to write squashfs archive")?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(output_path);
    }
    result
}

/// Add a directory and its contents to a SquashFS filesystem.
#[cfg(feature = "squashfs")]
fn add_directory_to_squashfs(
    writer: &mut backhand::FilesystemWriter,
    base_dir: &Path,
    current_dir: &Path,
) -> Result<()> {
    use std::io::Cursor;

    let entries = fs::read_dir(current_dir)
        .with_context(|| format!("failed to read directory: {}", current_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let relative_path = path
            .strip_prefix(base_dir)
            .context("failed to compute relative path")?;
        let name = relative_path.to_string_lossy().to_string();

        // Phase 0 P0.1: skip dev-store paths in the same walk that reads
        // bytes (no separate preflight, no TOCTOU window).
        if dev_secret_match(relative_path).is_some() {
            continue;
        }

        // Phase 0 P0.1: reject symlinks. `entry.file_type()` does NOT follow
        // them; `path.is_dir()` and `fs::read(&path)` below DO follow. A
        // benign-looking symlink whose target is a dev-secret path would
        // otherwise leak target bytes under the symlink's safe name.
        let file_type = entry
            .file_type()
            .with_context(|| format!("file type for {}", path.display()))?;
        if file_type.is_symlink() {
            bail!(
                "refusing to archive symlink {} (symlinks are not supported by gtbundle writers and may bypass the dev-secret skip by dereferencing through to a leaked target)",
                relative_path.display()
            );
        }

        if file_type.is_dir() {
            writer
                .push_dir(&name, dir_node_header())
                .with_context(|| format!("failed to add directory: {}", name))?;
            add_directory_to_squashfs(writer, base_dir, &path)?;
        } else {
            let content = fs::read(&path)
                .with_context(|| format!("failed to read file: {}", path.display()))?;
            let cursor = Cursor::new(content);
            writer
                .push_file(cursor, &name, file_node_header())
                .with_context(|| format!("failed to add file: {}", name))?;
        }
    }

    Ok(())
}

// `NodeHeader::default()` zero-fills permissions, which yields squashfs
// archives whose extracted directories have mode `0o000` and cannot be
// `read_dir()`'d by `gtc start`. Stamp world-readable defaults so any
// consumer can extract and start the bundle without a manual chmod.
#[cfg(feature = "squashfs")]
fn dir_node_header() -> backhand::NodeHeader {
    backhand::NodeHeader::new(0o755, 0, 0, 0)
}

#[cfg(feature = "squashfs")]
fn file_node_header() -> backhand::NodeHeader {
    backhand::NodeHeader::new(0o644, 0, 0, 0)
}

/// Create a .gtbundle archive using ZIP format.
fn create_gtbundle_zip(bundle_dir: &Path, output_path: &Path) -> Result<()> {
    if !bundle_dir.is_dir() {
        bail!("bundle directory not found: {}", bundle_dir.display());
    }

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).context("failed to create output directory")?;
    }

    let file = File::create(output_path)
        .with_context(|| format!("failed to create archive: {}", output_path.display()))?;
    let mut zip = ZipWriter::new(file);

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);

    let result = (|| -> Result<()> {
        add_directory_to_zip(&mut zip, bundle_dir, bundle_dir, options)?;
        zip.finish().context("failed to finalize archive")?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(output_path);
    }
    result
}

/// Extract a .gtbundle archive to a directory.
///
/// Auto-detects the archive format (SquashFS or ZIP) and extracts accordingly.
///
/// # Arguments
/// * `gtbundle_path` - Source .gtbundle file
/// * `output_dir` - Destination directory (will be created if needed)
///
/// # Example
/// ```ignore
/// extract_gtbundle(Path::new("./my-bundle.gtbundle"), Path::new("/tmp/my-bundle"))?;
/// ```
pub fn extract_gtbundle(gtbundle_path: &Path, output_dir: &Path) -> Result<()> {
    if !gtbundle_path.is_file() {
        bail!("gtbundle file not found: {}", gtbundle_path.display());
    }

    let format = detect_bundle_format(gtbundle_path)?;
    match format {
        #[cfg(feature = "squashfs")]
        BundleFormat::SquashFs => extract_gtbundle_squashfs(gtbundle_path, output_dir),
        BundleFormat::Zip => extract_gtbundle_zip(gtbundle_path, output_dir),
    }
}

/// Extract a .gtbundle archive using SquashFS format.
#[cfg(feature = "squashfs")]
fn extract_gtbundle_squashfs(gtbundle_path: &Path, output_dir: &Path) -> Result<()> {
    use backhand::FilesystemReader;

    let file = BufReader::new(
        File::open(gtbundle_path)
            .with_context(|| format!("failed to open archive: {}", gtbundle_path.display()))?,
    );
    let reader = FilesystemReader::from_reader(file).context("failed to read squashfs archive")?;

    fs::create_dir_all(output_dir).context("failed to create output directory")?;

    // Extract all entries
    for node in reader.files() {
        let path_str = node.fullpath.to_string_lossy();

        // Security: prevent path traversal
        if path_str.contains("..") {
            bail!("invalid path in archive: {}", path_str);
        }

        // Skip root directory
        if path_str == "/" || path_str.is_empty() {
            continue;
        }

        // Remove leading slash for joining
        let relative_path = path_str.trim_start_matches('/');
        let out_path = output_dir.join(relative_path);

        match &node.inner {
            backhand::InnerNode::Dir(_) => {
                fs::create_dir_all(&out_path)?;
            }
            backhand::InnerNode::File(file_reader) => {
                if let Some(parent) = out_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out_file = File::create(&out_path)
                    .with_context(|| format!("failed to create: {}", out_path.display()))?;
                let content = reader.file(file_reader);
                let mut decompressed = Vec::new();
                content
                    .reader()
                    .read_to_end(&mut decompressed)
                    .context("failed to decompress file")?;
                out_file
                    .write_all(&decompressed)
                    .context("failed to write file")?;
            }
            backhand::InnerNode::Symlink(link) => {
                #[cfg(unix)]
                {
                    if let Some(parent) = out_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let target = link.link.to_string_lossy();
                    std::os::unix::fs::symlink(&*target, &out_path).with_context(|| {
                        format!("failed to create symlink: {}", out_path.display())
                    })?;
                }
                #[cfg(not(unix))]
                {
                    // Skip symlinks on non-Unix platforms
                    let _ = link;
                }
            }
            _ => {
                // Skip other node types (devices, etc.)
            }
        }
    }

    Ok(())
}

/// Extract a .gtbundle archive using ZIP format.
fn extract_gtbundle_zip(gtbundle_path: &Path, output_dir: &Path) -> Result<()> {
    let file = File::open(gtbundle_path)
        .with_context(|| format!("failed to open archive: {}", gtbundle_path.display()))?;
    let mut archive = ZipArchive::new(file).context("failed to read archive")?;

    fs::create_dir_all(output_dir).context("failed to create output directory")?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .context("failed to read archive entry")?;
        let name = file.name().to_string();

        // Security: prevent path traversal
        if name.contains("..") {
            bail!("invalid path in archive: {}", name);
        }

        let out_path = output_dir.join(&name);

        if file.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = File::create(&out_path)
                .with_context(|| format!("failed to create: {}", out_path.display()))?;
            std::io::copy(&mut file, &mut out_file)?;

            // Restore permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    fs::set_permissions(&out_path, fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }

    Ok(())
}

/// Extract a .gtbundle to a temporary directory and return the path.
///
/// The caller is responsible for cleaning up the temporary directory.
pub fn extract_gtbundle_to_temp(gtbundle_path: &Path) -> Result<PathBuf> {
    let temp_dir = std::env::temp_dir().join(format!(
        "gtbundle-{}",
        gtbundle_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("bundle")
    ));

    // Clean up existing temp directory
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir).ok();
    }

    extract_gtbundle(gtbundle_path, &temp_dir)?;

    Ok(temp_dir)
}

/// Check if a path is a .gtbundle archive file.
pub fn is_gtbundle_file(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|ext| ext == "gtbundle")
}

/// Check if a path is a .gtbundle directory (named *.gtbundle but is a dir).
pub fn is_gtbundle_dir(path: &Path) -> bool {
    path.is_dir() && path.extension().is_some_and(|ext| ext == "gtbundle")
}

// ── Internal helpers ─────────────────────────────────────────────────────────

fn add_directory_to_zip<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    base_dir: &Path,
    current_dir: &Path,
    options: SimpleFileOptions,
) -> Result<()> {
    let entries = fs::read_dir(current_dir)
        .with_context(|| format!("failed to read directory: {}", current_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let relative_path = path
            .strip_prefix(base_dir)
            .context("failed to compute relative path")?;
        let name = relative_path.to_string_lossy();

        // Phase 0 P0.1: skip dev-store paths in the same walk that reads
        // bytes (no separate preflight, no TOCTOU window).
        if dev_secret_match(relative_path).is_some() {
            continue;
        }

        // Phase 0 P0.1: reject symlinks. `entry.file_type()` does NOT follow
        // them; `path.is_dir()` and `File::open(&path)` below DO follow. A
        // benign-looking symlink whose target is a dev-secret path would
        // otherwise leak target bytes under the symlink's safe name.
        let file_type = entry
            .file_type()
            .with_context(|| format!("file type for {}", path.display()))?;
        if file_type.is_symlink() {
            bail!(
                "refusing to archive symlink {} (symlinks are not supported by gtbundle writers and may bypass the dev-secret skip by dereferencing through to a leaked target)",
                relative_path.display()
            );
        }

        if file_type.is_dir() {
            // Add directory entry
            zip.add_directory(format!("{}/", name), options)?;
            // Recurse
            add_directory_to_zip(zip, base_dir, &path, options)?;
        } else {
            // Add file
            zip.start_file(name.to_string(), options)?;
            let mut file = File::open(&path)?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)?;
            zip.write_all(&buffer)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::{BUNDLE_WORKSPACE_MARKER, LEGACY_BUNDLE_MARKER};
    use std::fs;
    use tempfile::tempdir;

    fn create_test_bundle(bundle_dir: &Path) {
        fs::create_dir_all(bundle_dir).unwrap();
        fs::write(bundle_dir.join(LEGACY_BUNDLE_MARKER), "name: test").unwrap();
        fs::create_dir_all(bundle_dir.join("packs")).unwrap();
        fs::write(bundle_dir.join("packs/test.txt"), "hello").unwrap();
    }

    fn verify_extracted_bundle(extract_dir: &Path) {
        assert!(extract_dir.join(LEGACY_BUNDLE_MARKER).exists());
        assert!(extract_dir.join("packs/test.txt").exists());

        let content = fs::read_to_string(extract_dir.join("packs/test.txt")).unwrap();
        assert_eq!(content, "hello");
    }

    fn create_test_bundle_workspace(bundle_dir: &Path) {
        fs::create_dir_all(bundle_dir).unwrap();
        fs::write(
            bundle_dir.join(BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\n",
        )
        .unwrap();
        fs::create_dir_all(bundle_dir.join("packs")).unwrap();
        fs::write(bundle_dir.join("packs/test.txt"), "hello").unwrap();
    }

    #[test]
    fn test_create_and_extract_gtbundle_zip() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("test-bundle");
        let gtbundle_path = temp.path().join("test.gtbundle");
        let extract_dir = temp.path().join("extracted");

        create_test_bundle(&bundle_dir);

        // Create ZIP archive
        create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::Zip).unwrap();
        assert!(gtbundle_path.exists());

        // Verify format detection
        let format = detect_bundle_format(&gtbundle_path).unwrap();
        assert_eq!(format, BundleFormat::Zip);

        // Extract archive
        extract_gtbundle(&gtbundle_path, &extract_dir).unwrap();
        verify_extracted_bundle(&extract_dir);
    }

    #[cfg(feature = "squashfs")]
    #[test]
    fn test_create_and_extract_gtbundle_squashfs() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("test-bundle");
        let gtbundle_path = temp.path().join("test.gtbundle");
        let extract_dir = temp.path().join("extracted");

        create_test_bundle(&bundle_dir);

        // Create SquashFS archive
        create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::SquashFs).unwrap();
        assert!(gtbundle_path.exists());

        // Verify format detection
        let format = detect_bundle_format(&gtbundle_path).unwrap();
        assert_eq!(format, BundleFormat::SquashFs);

        // Extract archive
        extract_gtbundle(&gtbundle_path, &extract_dir).unwrap();
        verify_extracted_bundle(&extract_dir);
    }

    #[test]
    fn test_create_and_extract_gtbundle_default() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("test-bundle");
        let gtbundle_path = temp.path().join("test.gtbundle");
        let extract_dir = temp.path().join("extracted");

        create_test_bundle(&bundle_dir);

        // Create archive with default format
        create_gtbundle(&bundle_dir, &gtbundle_path).unwrap();
        assert!(gtbundle_path.exists());

        // Extract archive
        extract_gtbundle(&gtbundle_path, &extract_dir).unwrap();
        verify_extracted_bundle(&extract_dir);
    }

    #[test]
    fn test_create_and_extract_gtbundle_with_bundle_yaml_root() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("test-bundle");
        let gtbundle_path = temp.path().join("test.gtbundle");
        let extract_dir = temp.path().join("extracted");

        create_test_bundle_workspace(&bundle_dir);

        create_gtbundle(&bundle_dir, &gtbundle_path).unwrap();
        extract_gtbundle(&gtbundle_path, &extract_dir).unwrap();

        assert!(extract_dir.join(BUNDLE_WORKSPACE_MARKER).exists());
        assert!(extract_dir.join("packs/test.txt").exists());
    }

    #[test]
    fn test_is_gtbundle() {
        let temp = tempdir().unwrap();

        // Create a file
        let file_path = temp.path().join("test.gtbundle");
        fs::write(&file_path, "test").unwrap();
        assert!(is_gtbundle_file(&file_path));
        assert!(!is_gtbundle_dir(&file_path));

        // Create a directory
        let dir_path = temp.path().join("test2.gtbundle");
        fs::create_dir(&dir_path).unwrap();
        assert!(!is_gtbundle_file(&dir_path));
        assert!(is_gtbundle_dir(&dir_path));
    }

    #[test]
    fn test_detect_unknown_format() {
        let temp = tempdir().unwrap();
        let file_path = temp.path().join("unknown.gtbundle");
        fs::write(&file_path, "UNKN").unwrap();

        let result = detect_bundle_format(&file_path);
        assert!(result.is_err());
    }

    // Phase 0 secret-leak hotfix regression tests.
    // See plans/next-gen-deployment.md P0.1.
    //
    // Codex adversarial review on PR #109 caught that the original bail-on-detect
    // approach broke the normal setup→repack flow (ApplyPackSetup writes
    // .greentic/dev/.dev.secrets.env under the bundle root, then create_gtbundle
    // bailed). The current implementation skips dev-store paths during the
    // archive walk instead: the dev store stays on disk for runtime use, but
    // the .gtbundle artifact never contains it.

    fn extracted_paths(bundle_path: &Path) -> Vec<String> {
        let temp = tempdir().unwrap();
        extract_gtbundle(bundle_path, temp.path()).expect("extract");
        let mut paths = Vec::new();
        collect_paths(temp.path(), temp.path(), &mut paths);
        paths.sort();
        paths
    }

    fn collect_paths(root: &Path, current: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap();
            out.push(rel.to_string_lossy().to_string());
            if path.is_dir() {
                collect_paths(root, &path, out);
            }
        }
    }

    #[test]
    fn dev_secret_match_detects_dev_directory() {
        assert_eq!(
            dev_secret_match(Path::new(".greentic/dev/whatever.bin")),
            Some(".greentic/dev/ tree")
        );
    }

    #[test]
    fn dev_secret_match_detects_state_dev_directory() {
        assert_eq!(
            dev_secret_match(Path::new(".greentic/state/dev/something")),
            Some(".greentic/state/dev/ tree")
        );
    }

    #[test]
    fn dev_secret_match_detects_stray_dev_secrets_env_filename() {
        assert_eq!(
            dev_secret_match(Path::new("packs/.dev.secrets.env")),
            Some(".dev.secrets.env file")
        );
    }

    #[test]
    fn dev_secret_match_passes_through_safe_paths() {
        assert_eq!(dev_secret_match(Path::new("packs/pack-a.gtpack")), None);
        assert_eq!(
            dev_secret_match(Path::new("state/setup/provider-a.json")),
            None
        );
    }

    fn assert_no_dev_secret_paths_in_archive(archived: &[String]) {
        for path in archived {
            assert!(
                !path.starts_with(".greentic/dev") && !path.contains("/.greentic/dev"),
                ".greentic/dev tree leaked into archive: {path}"
            );
            assert!(
                !path.starts_with(".greentic/state/dev") && !path.contains("/.greentic/state/dev"),
                ".greentic/state/dev tree leaked into archive: {path}"
            );
            assert!(
                !path.ends_with(".dev.secrets.env"),
                ".dev.secrets.env file leaked into archive: {path}"
            );
        }
    }

    #[test]
    fn create_gtbundle_zip_skips_dev_secret_directory() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("bundle");
        create_test_bundle(&bundle_dir);
        fs::create_dir_all(bundle_dir.join(".greentic/dev")).unwrap();
        let leaked = "GTC_TOKEN=must-not-leak";
        fs::write(bundle_dir.join(".greentic/dev/.dev.secrets.env"), leaked).unwrap();

        let gtbundle_path = temp.path().join("clean.gtbundle");
        create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::Zip)
            .expect("repack must succeed after dev-store seeding");
        assert!(gtbundle_path.exists(), "artifact must be produced");

        let archived = extracted_paths(&gtbundle_path);
        assert_no_dev_secret_paths_in_archive(&archived);
        let raw = fs::read(&gtbundle_path).unwrap();
        assert!(
            !raw.windows(leaked.len())
                .any(|window| window == leaked.as_bytes()),
            "raw archive bytes must not contain dev-secret content"
        );
        // Source on disk is untouched — runtime still has its dev store.
        assert!(bundle_dir.join(".greentic/dev/.dev.secrets.env").exists());
    }

    #[cfg(feature = "squashfs")]
    #[test]
    fn create_gtbundle_squashfs_skips_state_dev_directory() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("bundle");
        create_test_bundle(&bundle_dir);
        fs::create_dir_all(bundle_dir.join(".greentic/state/dev")).unwrap();
        let leaked = "GTC_TOKEN=must-not-leak-state";
        fs::write(
            bundle_dir.join(".greentic/state/dev/.dev.secrets.env"),
            leaked,
        )
        .unwrap();

        let gtbundle_path = temp.path().join("clean.gtbundle");
        create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::SquashFs)
            .expect("repack must succeed after state-dev seeding");
        assert!(gtbundle_path.exists());

        let archived = extracted_paths(&gtbundle_path);
        assert_no_dev_secret_paths_in_archive(&archived);
        let raw = fs::read(&gtbundle_path).unwrap();
        assert!(
            !raw.windows(leaked.len())
                .any(|window| window == leaked.as_bytes())
        );
    }

    #[test]
    fn create_gtbundle_skips_stray_dev_secrets_env_filename() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("bundle");
        create_test_bundle(&bundle_dir);
        let leaked = "STRAY_TOKEN=must-not-ship";
        fs::write(bundle_dir.join("packs/.dev.secrets.env"), leaked).unwrap();

        let gtbundle_path = temp.path().join("stray.gtbundle");
        create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::Zip)
            .expect("repack must succeed when stray dev-secrets file present");

        let archived = extracted_paths(&gtbundle_path);
        assert_no_dev_secret_paths_in_archive(&archived);
        let raw = fs::read(&gtbundle_path).unwrap();
        assert!(
            !raw.windows(leaked.len())
                .any(|window| window == leaked.as_bytes())
        );
    }

    // Phase 0 P0.1: simulate the executors.rs:209-219 + bin/greentic_setup.rs:294
    // flow. ApplyPackSetup writes .greentic/dev/.dev.secrets.env under the
    // bundle root, then run_simple_setup calls create_gtbundle on the same
    // bundle dir. The previous bail-on-detect implementation broke this; the
    // skip-in-walker implementation must round-trip cleanly.
    #[test]
    fn post_setup_repack_round_trips_when_dev_store_present() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("bundle");
        create_test_bundle(&bundle_dir);

        // Step 1: ApplyPackSetup analogue — seed both possible dev-store paths
        // and a state/config/*/setup-answers.json with non-secret data that
        // MUST be preserved (the secret leak in this file is Phase B's job).
        fs::create_dir_all(bundle_dir.join(".greentic/dev")).unwrap();
        fs::write(
            bundle_dir.join(".greentic/dev/.dev.secrets.env"),
            "BOT_TOKEN=leaked-via-dev-store",
        )
        .unwrap();
        fs::create_dir_all(bundle_dir.join(".greentic/state/dev")).unwrap();
        fs::write(
            bundle_dir.join(".greentic/state/dev/.dev.secrets.env"),
            "ALT_TOKEN=leaked-via-state-dev",
        )
        .unwrap();
        fs::create_dir_all(bundle_dir.join("state/config/messaging-telegram")).unwrap();
        fs::write(
            bundle_dir.join("state/config/messaging-telegram/setup-answers.json"),
            r#"{"name":"my-bot","region":"eu-west-1"}"#,
        )
        .unwrap();

        // Step 2: run_simple_setup analogue — repack the same dir.
        let gtbundle_path = temp.path().join("configured.gtbundle");
        create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::Zip)
            .expect("post-setup repack must succeed");
        assert!(gtbundle_path.exists());

        // Step 3: extracted bundle contains exactly the right paths.
        let archived = extracted_paths(&gtbundle_path);
        assert!(
            !archived.iter().any(|p| p.starts_with(".greentic/dev")
                || p.starts_with(".greentic/state/dev")
                || p.ends_with(".dev.secrets.env")),
            "archive must not contain any dev-store path, got: {archived:?}"
        );
        assert!(
            archived
                .iter()
                .any(|p| p == "state/config/messaging-telegram/setup-answers.json"),
            "non-secret setup-answers.json must round-trip (secret leak is Phase B), got: {archived:?}"
        );

        // Step 4: raw bytes contain neither leaked token.
        let raw = fs::read(&gtbundle_path).unwrap();
        for forbidden in ["leaked-via-dev-store", "leaked-via-state-dev"] {
            assert!(
                !raw.windows(forbidden.len())
                    .any(|window| window == forbidden.as_bytes()),
                "raw archive bytes must not contain {forbidden}"
            );
        }

        // Step 5: source on disk untouched — runtime can still read its store.
        assert!(bundle_dir.join(".greentic/dev/.dev.secrets.env").exists());
        assert!(
            bundle_dir
                .join(".greentic/state/dev/.dev.secrets.env")
                .exists()
        );
    }

    // Phase 0 P0.1 symlink-bypass regression tests.
    //
    // The denylist must refuse symlinks because the legacy archive walkers in
    // this file unconditionally dereference them: `path.is_dir()` follows
    // symlinks, and the else branch reads target bytes via `fs::read` /
    // `File::open`. Without this guard, a benign-looking symlink whose target
    // is `.greentic/dev/.dev.secrets.env` would ship target bytes into the
    // archive under the symlink's safe-looking name.

    #[cfg(unix)]
    fn make_symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("create symlink");
    }

    #[cfg(unix)]
    #[test]
    fn create_gtbundle_zip_refuses_file_symlink_targeting_dev_secret() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("bundle");
        create_test_bundle(&bundle_dir);
        // Plant the secret OUTSIDE the bundle source — proves the leak is via
        // dereference, not via a deny-listed path inside the source tree.
        let secret_path = temp.path().join("external.dev.secrets.env");
        fs::write(&secret_path, "GTC_TOKEN=must-not-leak").unwrap();
        make_symlink(&secret_path, &bundle_dir.join("packs/seed.env"));

        let gtbundle_path = temp.path().join("symlink.gtbundle");
        let err = create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::Zip)
            .expect_err("symlink must be refused");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("refusing to archive symlink"),
            "expected symlink refusal; got: {msg}"
        );
        assert!(
            !gtbundle_path.exists(),
            "denylisted build must not produce artifact"
        );
    }

    #[cfg(all(unix, feature = "squashfs"))]
    #[test]
    fn create_gtbundle_squashfs_refuses_directory_symlink_targeting_dev_dir() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("bundle");
        create_test_bundle(&bundle_dir);
        let external_dev = temp.path().join("external-dev");
        fs::create_dir_all(&external_dev).unwrap();
        fs::write(external_dev.join(".dev.secrets.env"), "GTC_TOKEN=leaked").unwrap();
        make_symlink(&external_dev, &bundle_dir.join("packs/seed-dir"));

        let gtbundle_path = temp.path().join("symlink-dir.gtbundle");
        let err = create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::SquashFs)
            .expect_err("directory symlink must be refused");
        assert!(format!("{err:#}").contains("refusing to archive symlink"));
        assert!(!gtbundle_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn create_gtbundle_refuses_benign_looking_symlink() {
        // Even a symlink with no obviously deny-listed target must be refused:
        // we cannot inspect the target safely against all attack shapes, and
        // the legacy writers do not preserve symlinks anyway.
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("bundle");
        create_test_bundle(&bundle_dir);
        let benign_target = temp.path().join("benign.txt");
        fs::write(&benign_target, "benign content").unwrap();
        make_symlink(&benign_target, &bundle_dir.join("packs/link.txt"));

        let gtbundle_path = temp.path().join("any-symlink.gtbundle");
        let err = create_gtbundle_with_format(&bundle_dir, &gtbundle_path, BundleFormat::Zip)
            .expect_err("any symlink must be refused");
        assert!(format!("{err:#}").contains("refusing to archive symlink"));
    }
}
