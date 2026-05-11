//! Bundle source resolution — parse and resolve bundle references from various protocols.
//!
//! Supports local paths, file:// URIs, and remote protocols via greentic-distributor-client.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};

/// A bundle source that can be resolved to a local artifact path.
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

    /// Resolve the source to a local artifact path.
    ///
    /// For local sources, validates the path exists.
    /// For remote sources, fetches and extracts to a local cache directory.
    pub fn resolve(&self) -> anyhow::Result<PathBuf> {
        match self {
            Self::LocalDir(path) => resolve_local_path(path),
            Self::FileUri(path) => resolve_local_path(path),
            #[cfg(feature = "oci")]
            Self::Oci { reference } => resolve_oci_pack_reference(reference),
            #[cfg(feature = "oci")]
            Self::Repo { reference } => resolve_distributor_reference(reference),
            #[cfg(feature = "oci")]
            Self::Store { reference } => resolve_distributor_reference(reference),
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
            Self::Oci { reference } => resolve_oci_pack_reference_async(reference).await,
            #[cfg(feature = "oci")]
            Self::Repo { reference } => resolve_distributor_reference_async(reference).await,
            #[cfg(feature = "oci")]
            Self::Store { reference } => resolve_distributor_reference_async(reference).await,
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
        matches!(
            self,
            Self::Oci { .. } | Self::Repo { .. } | Self::Store { .. }
        )
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
            if hex.len() == 2
                && let Ok(byte) = u8::from_str_radix(&hex, 16)
            {
                result.push(byte as char);
                continue;
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
        return Err(anyhow!(
            "bundle path does not exist: {}",
            canonical.display()
        ));
    }

    Ok(canonical)
}

/// Resolve an OCI pack reference using the pack fetcher.
#[cfg(feature = "oci")]
fn resolve_oci_pack_reference(reference: &str) -> anyhow::Result<PathBuf> {
    use tokio::runtime::Runtime;

    let rt = Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(resolve_oci_pack_reference_async(reference))
}

/// Resolve an OCI pack reference asynchronously.
#[cfg(feature = "oci")]
async fn resolve_oci_pack_reference_async(reference: &str) -> anyhow::Result<PathBuf> {
    use greentic_distributor_client::oci_packs::DefaultRegistryClient;
    use greentic_distributor_client::{OciPackFetcher, PackFetchOptions};

    let oci_reference = reference.strip_prefix("oci://").unwrap_or(reference).trim();
    let options = PackFetchOptions {
        allow_tags: true,
        ..PackFetchOptions::default()
    };
    let fetched =
        if let Some((username, password)) = registry_basic_auth_for_reference(oci_reference) {
            let client = DefaultRegistryClient::with_basic_auth(username, password);
            OciPackFetcher::with_client(client, options)
                .fetch_pack_to_cache(oci_reference)
                .await
        } else {
            OciPackFetcher::<DefaultRegistryClient>::new(options)
                .fetch_pack_to_cache(oci_reference)
                .await
        }
        .with_context(|| format!("failed to fetch OCI pack reference: {}", reference))?;

    if fetched.path.exists() {
        return Ok(fetched.path);
    }

    anyhow::bail!(
        "resolved bundle reference without a local cached artifact: {}",
        reference
    );
}

#[cfg(feature = "oci")]
fn registry_basic_auth_for_reference(reference: &str) -> Option<(String, String)> {
    let registry = reference.split('/').next().unwrap_or_default();

    let generic_username = std::env::var("OCI_USERNAME")
        .ok()
        .filter(|value| !value.is_empty());
    let generic_password = std::env::var("OCI_PASSWORD")
        .ok()
        .filter(|value| !value.is_empty());
    if let (Some(username), Some(password)) = (generic_username, generic_password) {
        return Some((username, password));
    }

    if registry == "ghcr.io" {
        let password = std::env::var("GHCR_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("GITHUB_TOKEN")
                    .ok()
                    .filter(|value| !value.is_empty())
            });
        let username = std::env::var("GHCR_USERNAME")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                std::env::var("GHCR_USER")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| {
                std::env::var("GITHUB_ACTOR")
                    .ok()
                    .filter(|value| !value.is_empty())
            })
            .or_else(|| std::env::var("USER").ok().filter(|value| !value.is_empty()));

        if let (Some(username), Some(password)) = (username, password) {
            return Some((username, password));
        }
    }

    None
}

/// Resolve a repo/store reference using greentic-distributor-client.
#[cfg(feature = "oci")]
fn resolve_distributor_reference(reference: &str) -> anyhow::Result<PathBuf> {
    use tokio::runtime::Runtime;

    let rt = Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(resolve_distributor_reference_async(reference))
}

/// Resolve a repo/store reference asynchronously.
#[cfg(feature = "oci")]
async fn resolve_distributor_reference_async(reference: &str) -> anyhow::Result<PathBuf> {
    use greentic_distributor_client::{CachePolicy, DistClient, DistOptions, ResolvePolicy};

    let client = DistClient::new(DistOptions::default());
    let source = client
        .parse_source(reference)
        .with_context(|| format!("failed to parse bundle reference: {}", reference))?;
    let resolved = client
        .resolve(source, ResolvePolicy)
        .await
        .with_context(|| format!("failed to resolve bundle reference: {}", reference))?;
    let fetched = client
        .fetch(&resolved, CachePolicy)
        .await
        .with_context(|| format!("failed to fetch bundle reference: {}", reference))?;

    if fetched.local_path.exists() {
        return Ok(fetched.local_path);
    }
    if let Some(path) = fetched.wasm_path
        && path.exists()
    {
        return Ok(path);
    }
    if let Some(path) = fetched.cache_path
        && path.exists()
    {
        return Ok(path);
    }

    anyhow::bail!(
        "resolved bundle reference without a local cached artifact: {}",
        reference
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

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
    fn percent_decode_preserves_invalid_sequences() {
        let decoded = percent_decode("path%2G%tail%");
        assert_eq!(decoded, "path%2G%tail%");
    }

    #[test]
    fn as_str_preserves_local_and_file_uri_sources() {
        let local = BundleSource::parse("./bundle").unwrap();
        assert_eq!(local.as_str(), "./bundle");

        let file_uri = BundleSource::parse("file:///tmp/test%20bundle").unwrap();
        assert_eq!(file_uri.as_str(), "file:///tmp/test bundle");
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

    #[cfg(feature = "oci")]
    #[test]
    fn remote_references_preserve_original_strings() {
        let refs = [
            "oci://ghcr.io/greentic/example-pack:latest",
            "repo://greentic/example-pack",
            "store://greentic-biz/demo/example-pack:latest",
        ];

        for raw in refs {
            let parsed = BundleSource::parse(raw).unwrap();
            assert_eq!(parsed.as_str(), raw);
            assert!(parsed.is_remote());
        }
    }

    #[test]
    fn parse_trims_whitespace() {
        let source = BundleSource::parse("   ./bundle  ").unwrap();
        if let BundleSource::LocalDir(path) = source {
            assert_eq!(path, PathBuf::from("./bundle"));
        } else {
            panic!("expected LocalDir variant");
        }
    }

    #[test]
    fn as_str_local_dir_returns_path() {
        let source = BundleSource::LocalDir(PathBuf::from("/tmp/example"));
        assert_eq!(source.as_str(), "/tmp/example");
    }

    #[test]
    fn as_str_file_uri_prepends_scheme() {
        let source = BundleSource::FileUri(PathBuf::from("/tmp/example"));
        assert_eq!(source.as_str(), "file:///tmp/example");
    }

    #[test]
    fn local_dir_is_not_remote_under_oci_feature() {
        #[cfg(feature = "oci")]
        {
            let local = BundleSource::parse("./bundle").unwrap();
            assert!(!local.is_remote());
        }
    }

    #[test]
    fn percent_decode_passes_through_invalid_escapes() {
        // %ZZ is not valid hex — the literal '%' and following characters are kept.
        let decoded = percent_decode("foo%ZZbar");
        assert_eq!(decoded, "foo%ZZbar");
    }

    #[test]
    fn percent_decode_handles_trailing_percent() {
        let decoded = percent_decode("trailing%");
        assert_eq!(decoded, "trailing%");
    }

    #[test]
    fn percent_decode_handles_short_trailing_percent() {
        let decoded = percent_decode("short%2");
        // Only one hex char — should be treated as literal.
        assert_eq!(decoded, "short%2");
    }

    #[cfg(not(feature = "oci"))]
    #[test]
    fn unsupported_protocol_errors_without_oci_feature() {
        for raw in ["oci://x/y:1", "repo://x/y", "store://abc"] {
            let err = BundleSource::parse(raw).unwrap_err();
            assert!(err.to_string().contains("not supported"));
        }
    }

    #[cfg(feature = "oci")]
    #[test]
    fn registry_basic_auth_uses_generic_oci_credentials_first() {
        let _guard = env_lock();
        unsafe {
            std::env::set_var("OCI_USERNAME", "oci-user");
            std::env::set_var("OCI_PASSWORD", "oci-pass");
            std::env::remove_var("GHCR_TOKEN");
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GHCR_USERNAME");
            std::env::remove_var("GHCR_USER");
            std::env::remove_var("GITHUB_ACTOR");
            std::env::remove_var("USER");
        }

        let creds = registry_basic_auth_for_reference("ghcr.io/greentic/example-pack:latest");
        assert_eq!(
            creds,
            Some(("oci-user".to_string(), "oci-pass".to_string()))
        );

        unsafe {
            std::env::remove_var("OCI_USERNAME");
            std::env::remove_var("OCI_PASSWORD");
        }
    }

    #[cfg(feature = "oci")]
    #[test]
    fn registry_basic_auth_builds_ghcr_credentials_from_github_env() {
        let _guard = env_lock();
        unsafe {
            std::env::remove_var("OCI_USERNAME");
            std::env::remove_var("OCI_PASSWORD");
            std::env::set_var("GITHUB_TOKEN", "gh-token");
            std::env::set_var("GITHUB_ACTOR", "octocat");
            std::env::remove_var("GHCR_TOKEN");
            std::env::remove_var("GHCR_USERNAME");
            std::env::remove_var("GHCR_USER");
            std::env::remove_var("USER");
        }

        let creds = registry_basic_auth_for_reference("ghcr.io/greentic/example-pack:latest");
        assert_eq!(creds, Some(("octocat".to_string(), "gh-token".to_string())));

        unsafe {
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GITHUB_ACTOR");
        }
    }

    #[cfg(feature = "oci")]
    #[test]
    fn registry_basic_auth_returns_none_without_matching_env() {
        let _guard = env_lock();
        unsafe {
            std::env::remove_var("OCI_USERNAME");
            std::env::remove_var("OCI_PASSWORD");
            std::env::remove_var("GHCR_TOKEN");
            std::env::remove_var("GITHUB_TOKEN");
            std::env::remove_var("GHCR_USERNAME");
            std::env::remove_var("GHCR_USER");
            std::env::remove_var("GITHUB_ACTOR");
            std::env::remove_var("USER");
        }

        assert_eq!(
            registry_basic_auth_for_reference("example.com/greentic/example-pack:latest"),
            None
        );
    }

    #[test]
    fn resolve_local_dir_returns_relative_path_when_it_exists() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        fs::create_dir_all(&bundle).unwrap();
        let relative = bundle.strip_prefix(std::env::current_dir().unwrap()).ok();

        if let Some(relative) = relative {
            let source = BundleSource::LocalDir(relative.to_path_buf());
            assert_eq!(source.resolve().unwrap(), relative);
            return;
        }

        let source = BundleSource::LocalDir(bundle.clone());
        assert_eq!(source.resolve().unwrap(), bundle);
    }

    #[test]
    fn resolve_file_uri_returns_existing_absolute_path() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        fs::create_dir_all(&bundle).unwrap();

        let source = BundleSource::FileUri(bundle.clone());
        assert_eq!(source.resolve().unwrap(), bundle);
    }

    #[tokio::test]
    async fn resolve_async_supports_local_sources() {
        let temp = tempfile::tempdir().unwrap();
        let bundle = temp.path().join("bundle");
        fs::create_dir_all(&bundle).unwrap();

        let local = BundleSource::LocalDir(bundle.clone());
        assert_eq!(local.resolve_async().await.unwrap(), bundle);

        let file_uri = BundleSource::FileUri(bundle.clone());
        assert_eq!(file_uri.resolve_async().await.unwrap(), bundle);
    }

    #[test]
    fn resolve_missing_local_path_fails() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let source = BundleSource::LocalDir(missing.clone());

        let error = source.resolve().unwrap_err().to_string();
        assert!(error.contains("bundle path does not exist"));
        assert!(error.contains(&missing.display().to_string()));
    }
}
