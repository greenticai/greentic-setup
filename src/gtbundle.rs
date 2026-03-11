//! .gtbundle archive format support.
//!
//! A `.gtbundle` file is a ZIP archive containing a complete Greentic bundle.
//! This module provides functions to create and extract gtbundle archives.
//!
//! ## Format
//!
//! ```text
//! my-bundle.gtbundle (ZIP archive)
//! ├── greentic.demo.yaml
//! ├── packs/
//! ├── providers/
//! ├── resolved/
//! ├── state/
//! └── tenants/
//! ```

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

/// Create a .gtbundle archive from a bundle directory.
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

    let file = File::open(gtbundle_path)
        .with_context(|| format!("failed to open archive: {}", gtbundle_path.display()))?;
    let mut archive = ZipArchive::new(file).context("failed to read archive")?;

    fs::create_dir_all(output_dir).context("failed to create output directory")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("failed to read archive entry")?;
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

    #[test]
    fn test_create_and_extract_gtbundle() {
        let temp = tempdir().unwrap();
        let bundle_dir = temp.path().join("test-bundle");
        let gtbundle_path = temp.path().join("test.gtbundle");
        let extract_dir = temp.path().join("extracted");

        // Create test bundle
        fs::create_dir_all(&bundle_dir).unwrap();
        fs::write(bundle_dir.join("greentic.demo.yaml"), "name: test").unwrap();
        fs::create_dir_all(bundle_dir.join("packs")).unwrap();
        fs::write(bundle_dir.join("packs/test.txt"), "hello").unwrap();

        // Create archive
        create_gtbundle(&bundle_dir, &gtbundle_path).unwrap();
        assert!(gtbundle_path.exists());

        // Extract archive
        extract_gtbundle(&gtbundle_path, &extract_dir).unwrap();
        assert!(extract_dir.join("greentic.demo.yaml").exists());
        assert!(extract_dir.join("packs/test.txt").exists());

        let content = fs::read_to_string(extract_dir.join("packs/test.txt")).unwrap();
        assert_eq!(content, "hello");
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
}
