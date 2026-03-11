//! .gtbundle archive format support.
//!
//! A `.gtbundle` file is an archive containing a complete Greentic bundle.
//! Supports both SquashFS (default) and ZIP formats.
//!
//! ## Format
//!
//! ```text
//! my-bundle.gtbundle (SquashFS or ZIP archive)
//! ├── greentic.demo.yaml
//! ├── packs/
//! ├── providers/
//! ├── resolved/
//! ├── state/
//! └── tenants/
//! ```

use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

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
    match format {
        #[cfg(feature = "squashfs")]
        BundleFormat::SquashFs => create_gtbundle_squashfs(bundle_dir, output_path),
        BundleFormat::Zip => create_gtbundle_zip(bundle_dir, output_path),
    }
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

    // Walk the bundle directory and add all files
    add_directory_to_squashfs(&mut writer, bundle_dir, bundle_dir)?;

    // Write the filesystem
    let mut output = File::create(output_path)
        .with_context(|| format!("failed to create archive: {}", output_path.display()))?;
    writer
        .write(&mut output)
        .context("failed to write squashfs archive")?;

    Ok(())
}

/// Add a directory and its contents to a SquashFS filesystem.
#[cfg(feature = "squashfs")]
fn add_directory_to_squashfs(
    writer: &mut backhand::FilesystemWriter,
    base_dir: &Path,
    current_dir: &Path,
) -> Result<()> {
    use backhand::NodeHeader;
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

        if path.is_dir() {
            // Add directory
            writer
                .push_dir(&name, NodeHeader::default())
                .with_context(|| format!("failed to add directory: {}", name))?;
            // Recurse
            add_directory_to_squashfs(writer, base_dir, &path)?;
        } else {
            // Add file
            let content = fs::read(&path)
                .with_context(|| format!("failed to read file: {}", path.display()))?;
            let cursor = Cursor::new(content);
            writer
                .push_file(cursor, &name, NodeHeader::default())
                .with_context(|| format!("failed to add file: {}", name))?;
        }
    }

    Ok(())
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

    // Walk the bundle directory and add all files
    add_directory_to_zip(&mut zip, bundle_dir, bundle_dir, options)?;

    zip.finish().context("failed to finalize archive")?;

    Ok(())
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

        if path.is_dir() {
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
    use std::fs;
    use tempfile::tempdir;

    fn create_test_bundle(bundle_dir: &Path) {
        fs::create_dir_all(bundle_dir).unwrap();
        fs::write(bundle_dir.join("greentic.demo.yaml"), "name: test").unwrap();
        fs::create_dir_all(bundle_dir.join("packs")).unwrap();
        fs::write(bundle_dir.join("packs/test.txt"), "hello").unwrap();
    }

    fn verify_extracted_bundle(extract_dir: &Path) {
        assert!(extract_dir.join("greentic.demo.yaml").exists());
        assert!(extract_dir.join("packs/test.txt").exists());

        let content = fs::read_to_string(extract_dir.join("packs/test.txt")).unwrap();
        assert_eq!(content, "hello");
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
}
