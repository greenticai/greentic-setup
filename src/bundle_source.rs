//! Bundle source resolution — parse and resolve bundle references from various protocols.
//!
//! Supports local paths, file:// URIs, and remote protocols via greentic-distributor-client.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};

/// A bundle source that can be resolved to a local directory path.
#[derive(Clone, Debug)]
pub enum BundleSource {
    /// Local directory path (absolute or relative).
    LocalDir(PathBuf),
    /// file:// URI pointing to a local path.
    FileUri(PathBuf),
    /// oci://registry/repo:tag — OCI registry reference.
    #[cfg(feature = "oci")]
    Oci { reference: String },
    /// repo://org/name — Pack repository reference (maps to OCI).
    #[cfg(feature = "oci")]
    Repo { reference: String },
    /// store://id — Component store reference (maps to OCI).
    #[cfg(feature = "oci")]
    Store { reference: String },
}

impl BundleSource {
    /// Parse a bundle source string into the appropriate variant.
    ///
    /// # Examples
    ///
    /// ```
    /// use greentic_setup::bundle_source::BundleSource;
    ///
    /// // Local path
    /// let source = BundleSource::parse("./my-bundle").unwrap();
    ///
    /// // file:// URI
    /// let source = BundleSource::parse("file:///home/user/bundle").unwrap();
    ///
    /// // OCI reference (requires "oci" feature)
    /// // let source = BundleSource::parse("oci://ghcr.io/org/bundle:latest").unwrap();
    /// ```
    pub fn parse(source: &str) -> anyhow::Result<Self> {
        let trimmed = source.trim();

        if trimmed.is_empty() {
            return Err(anyhow!("bundle source cannot be empty"));
        }

        // OCI protocol
        #[cfg(feature = "oci")]
        if trimmed.starts_with("oci://") {
            return Ok(Self::Oci {
                reference: trimmed.to_string(),
            });
        }

        // Repo protocol
        #[cfg(feature = "oci")]
        if trimmed.starts_with("repo://") {
            return Ok(Self::Repo {
                reference: trimmed.to_string(),
            });
        }

        // Store protocol
        #[cfg(feature = "oci")]
        if trimmed.starts_with("store://") {
            return Ok(Self::Store {
                reference: trimmed.to_string(),
            });
        }

        // file:// URI
        if trimmed.starts_with("file://") {
            let path = file_uri_to_path(trimmed)?;
            return Ok(Self::FileUri(path));
        }

        // Check for unsupported protocols
        #[cfg(not(feature = "oci"))]
        if trimmed.starts_with("oci://")
            || trimmed.starts_with("repo://")
            || trimmed.starts_with("store://")
        {
            return Err(anyhow!(
                "protocol not supported (compile with 'oci' feature): {}",
                trimmed.split("://").next().unwrap_or("unknown")
            ));
        }

        // Treat as local path
        let path = PathBuf::from(trimmed);
        Ok(Self::LocalDir(path))
    }

    /// Resolve the source to a local directory path.
    ///
    /// For local sources, validates the path exists.
    /// For remote sources, fetches and extracts to a local cache directory.
    pub fn resolve(&self) -> anyhow::Result<PathBuf> {
        match self {
            Self::LocalDir(path) => resolve_local_path(path),
            Self::FileUri(path) => resolve_local_path(path),
            #[cfg(feature = "oci")]
            Self::Oci { reference } => resolve_oci_reference(reference),
            #[cfg(feature = "oci")]
            Self::Repo { reference } => resolve_oci_reference(reference),
            #[cfg(feature = "oci")]
            Self::Store { reference } => resolve_oci_reference(reference),
        }
    }

    /// Resolve the source asynchronously.
    ///
    /// For local sources, validates the path exists.
    /// For remote sources, fetches and extracts to a local cache directory.
    pub async fn resolve_async(&self) -> anyhow::Result<PathBuf> {
        match self {
            Self::LocalDir(path) => resolve_local_path(path),
            Self::FileUri(path) => resolve_local_path(path),
            #[cfg(feature = "oci")]
            Self::Oci { reference } => resolve_oci_reference_async(reference).await,
            #[cfg(feature = "oci")]
            Self::Repo { reference } => resolve_oci_reference_async(reference).await,
            #[cfg(feature = "oci")]
            Self::Store { reference } => resolve_oci_reference_async(reference).await,
        }
    }

    /// Returns the original source string representation.
    pub fn as_str(&self) -> String {
        match self {
            Self::LocalDir(path) => path.display().to_string(),
            Self::FileUri(path) => format!("file://{}", path.display()),
            #[cfg(feature = "oci")]
            Self::Oci { reference } => reference.clone(),
            #[cfg(feature = "oci")]
            Self::Repo { reference } => reference.clone(),
            #[cfg(feature = "oci")]
            Self::Store { reference } => reference.clone(),
        }
    }

    /// Returns true if this is a local source (LocalDir or FileUri).
    pub fn is_local(&self) -> bool {
        matches!(self, Self::LocalDir(_) | Self::FileUri(_))
    }

    /// Returns true if this is a remote source (Oci, Repo, or Store).
    #[cfg(feature = "oci")]
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Oci { .. } | Self::Repo { .. } | Self::Store { .. })
    }
}

/// Convert a file:// URI to a local path.
fn file_uri_to_path(uri: &str) -> anyhow::Result<PathBuf> {
    let path_str = uri
        .strip_prefix("file://")
        .ok_or_else(|| anyhow!("invalid file URI: {}", uri))?;

    // Handle Windows paths (file:///C:/path)
    #[cfg(windows)]
    let path_str = path_str.strip_prefix('/').unwrap_or(path_str);

    let decoded = percent_decode(path_str);
    Ok(PathBuf::from(decoded))
}

/// Simple percent-decoding for file paths.
fn percent_decode(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(ch);
        }
    }

    result
}

/// Resolve a local path, validating it exists.
fn resolve_local_path(path: &Path) -> anyhow::Result<PathBuf> {
    let canonical = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to get current directory")?
            .join(path)
    };

    if !canonical.exists() {
        return Err(anyhow!("bundle path does not exist: {}", canonical.display()));
    }

    Ok(canonical)
}

/// Resolve an OCI/repo/store reference using greentic-distributor-client.
#[cfg(feature = "oci")]
fn resolve_oci_reference(reference: &str) -> anyhow::Result<PathBuf> {
    use tokio::runtime::Runtime;

    let rt = Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(resolve_oci_reference_async(reference))
}

/// Resolve an OCI/repo/store reference asynchronously.
#[cfg(feature = "oci")]
async fn resolve_oci_reference_async(reference: &str) -> anyhow::Result<PathBuf> {
    use greentic_distributor_client::oci_packs::fetch_pack_to_cache;

    let resolved = fetch_pack_to_cache(reference)
        .await
        .with_context(|| format!("failed to resolve bundle reference: {}", reference))?;

    // The resolved artifact contains a path to the cached content
    Ok(resolved.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_local_path() {
        let source = BundleSource::parse("./my-bundle").unwrap();
        assert!(matches!(source, BundleSource::LocalDir(_)));
    }

    #[test]
    fn parse_absolute_path() {
        let source = BundleSource::parse("/home/user/bundle").unwrap();
        assert!(matches!(source, BundleSource::LocalDir(_)));
    }

    #[test]
    fn parse_file_uri() {
        let source = BundleSource::parse("file:///home/user/bundle").unwrap();
        assert!(matches!(source, BundleSource::FileUri(_)));
        if let BundleSource::FileUri(path) = source {
            assert_eq!(path, PathBuf::from("/home/user/bundle"));
        }
    }

    #[cfg(feature = "oci")]
    #[test]
    fn parse_oci_reference() {
        let source = BundleSource::parse("oci://ghcr.io/org/bundle:latest").unwrap();
        assert!(matches!(source, BundleSource::Oci { .. }));
    }

    #[cfg(feature = "oci")]
    #[test]
    fn parse_repo_reference() {
        let source = BundleSource::parse("repo://greentic/messaging-telegram").unwrap();
        assert!(matches!(source, BundleSource::Repo { .. }));
    }

    #[cfg(feature = "oci")]
    #[test]
    fn parse_store_reference() {
        let source = BundleSource::parse("store://bundle-abc123").unwrap();
        assert!(matches!(source, BundleSource::Store { .. }));
    }

    #[test]
    fn empty_source_fails() {
        assert!(BundleSource::parse("").is_err());
        assert!(BundleSource::parse("   ").is_err());
    }

    #[test]
    fn file_uri_percent_decode() {
        let decoded = percent_decode("path%20with%20spaces");
        assert_eq!(decoded, "path with spaces");
    }

    #[test]
    fn is_local_checks() {
        let local = BundleSource::parse("./bundle").unwrap();
        assert!(local.is_local());

        let file_uri = BundleSource::parse("file:///path").unwrap();
        assert!(file_uri.is_local());
    }

    #[cfg(feature = "oci")]
    #[test]
    fn is_remote_checks() {
        let oci = BundleSource::parse("oci://ghcr.io/test").unwrap();
        assert!(oci.is_remote());
        assert!(!oci.is_local());
    }
}
