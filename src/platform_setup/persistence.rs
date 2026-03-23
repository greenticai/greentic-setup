//! Artifact persistence for static routes policy.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::platform_setup::types::StaticRoutesPolicy;

/// Get the path to the static routes artifact file.
pub fn static_routes_artifact_path(bundle_root: &Path) -> PathBuf {
    bundle_root
        .join("state")
        .join("config")
        .join("platform")
        .join("static-routes.json")
}

/// Load static routes policy from the bundle artifact file.
pub fn load_static_routes_artifact(bundle_root: &Path) -> Result<Option<StaticRoutesPolicy>> {
    let path = static_routes_artifact_path(bundle_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let policy = serde_json::from_str(&raw)
        .or_else(|_| serde_yaml_bw::from_str(&raw))
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(policy))
}

#[derive(Debug, Deserialize)]
struct RuntimeEndpoints {
    #[allow(dead_code)]
    tenant: Option<String>,
    #[allow(dead_code)]
    team: Option<String>,
    public_base_url: Option<String>,
}

/// Load public base URL from runtime endpoints file.
pub fn load_runtime_public_base_url(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
) -> Result<Option<String>> {
    let team = team.unwrap_or("default");
    let path = bundle_root
        .join("state")
        .join("runtime")
        .join(format!("{tenant}.{team}"))
        .join("endpoints.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let endpoints: RuntimeEndpoints = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(endpoints
        .public_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string))
}

/// Load effective static routes defaults, merging artifact and runtime data.
pub fn load_effective_static_routes_defaults(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
) -> Result<Option<StaticRoutesPolicy>> {
    let mut policy = load_static_routes_artifact(bundle_root)?.unwrap_or_default();
    if policy.public_base_url.is_none()
        && let Some(runtime_url) = load_runtime_public_base_url(bundle_root, tenant, team)?
    {
        policy.public_base_url = Some(runtime_url);
    }
    if policy == StaticRoutesPolicy::disabled() {
        return Ok(None);
    }
    Ok(Some(policy))
}

/// Persist static routes policy to the bundle artifact file.
pub fn persist_static_routes_artifact(
    bundle_root: &Path,
    policy: &StaticRoutesPolicy,
) -> Result<PathBuf> {
    let path = static_routes_artifact_path(bundle_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(policy).context("serialize static routes policy")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}
