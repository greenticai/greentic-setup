//! Artifact persistence for static routes policy.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::platform_setup::types::{
    StaticRoutesPolicy, TelemetryAnswers, TunnelAnswers, TunnelHandoff,
};

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
    gateway_listen_addr: Option<String>,
    gateway_port: Option<u16>,
}

fn load_runtime_endpoints(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
) -> Result<Option<RuntimeEndpoints>> {
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
    Ok(Some(endpoints))
}

/// Load public base URL from runtime endpoints file.
pub fn load_runtime_public_base_url(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
) -> Result<Option<String>> {
    let Some(endpoints) = load_runtime_endpoints(bundle_root, tenant, team)? else {
        return Ok(None);
    };
    Ok(endpoints
        .public_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string))
}

/// Load local runtime base URL from runtime endpoints file.
pub fn load_runtime_local_base_url(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
) -> Result<Option<String>> {
    let Some(endpoints) = load_runtime_endpoints(bundle_root, tenant, team)? else {
        return Ok(None);
    };
    let host = endpoints
        .gateway_listen_addr
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("127.0.0.1");
    let Some(port) = endpoints.gateway_port else {
        return Ok(None);
    };
    Ok(Some(format!("http://{host}:{port}")))
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

/// Get the path to the tunnel configuration artifact file.
pub fn tunnel_artifact_path(bundle_root: &Path) -> PathBuf {
    bundle_root.join(".greentic").join("tunnel.json")
}

/// Load tunnel answers from the bundle artifact file.
pub fn load_tunnel_artifact(bundle_root: &Path) -> Result<Option<TunnelAnswers>> {
    let path = tunnel_artifact_path(bundle_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let answers: TunnelAnswers = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(answers))
}

/// Persist tunnel answers to the bundle artifact file.
pub fn persist_tunnel_artifact(bundle_root: &Path, answers: &TunnelAnswers) -> Result<PathBuf> {
    let path = tunnel_artifact_path(bundle_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(answers).context("serialize tunnel answers")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Get the path to the setup tunnel handoff artifact file.
///
/// Lives alongside `static-routes.json` in `state/config/platform/` — the
/// same bundle-scoped directory `greentic-start` already reads config from
/// (see `configured_public_base_url_from_static_routes` there). This is a
/// cross-repo protocol file, like the machine-wide shared tunnel record
/// (`shared_tunnel.rs`): it hands off the *local port* setup's speculative
/// tunnel targets, so `greentic-start` can bind its gateway to that same
/// port on first boot and adopt the already-running tunnel via the shared
/// record, instead of picking its own default/fallback port and minting a
/// second, disconnected tunnel.
pub fn tunnel_handoff_artifact_path(bundle_root: &Path) -> PathBuf {
    bundle_root
        .join("state")
        .join("config")
        .join("platform")
        .join("tunnel-handoff.json")
}

/// Load the setup tunnel handoff from the bundle artifact file.
pub fn load_tunnel_handoff_artifact(bundle_root: &Path) -> Result<Option<TunnelHandoff>> {
    let path = tunnel_handoff_artifact_path(bundle_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let handoff: TunnelHandoff = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(handoff))
}

/// Persist the setup tunnel handoff to the bundle artifact file.
pub fn persist_tunnel_handoff_artifact(
    bundle_root: &Path,
    handoff: &TunnelHandoff,
) -> Result<PathBuf> {
    let path = tunnel_handoff_artifact_path(bundle_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(handoff).context("serialize tunnel handoff")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
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

/// Get the path to the telemetry artifact file (sidecar to bundle.yaml).
pub fn telemetry_artifact_path(bundle_root: &Path) -> PathBuf {
    bundle_root
        .join("state")
        .join("config")
        .join("platform")
        .join("telemetry.json")
}

/// Load telemetry answers from the bundle artifact file.
pub fn load_telemetry_artifact(bundle_root: &Path) -> Result<Option<TelemetryAnswers>> {
    let path = telemetry_artifact_path(bundle_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let answers: TelemetryAnswers = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(answers))
}

/// Persist telemetry answers to the bundle artifact file.
pub fn persist_telemetry_artifact(
    bundle_root: &Path,
    answers: &TelemetryAnswers,
) -> Result<PathBuf> {
    let path = telemetry_artifact_path(bundle_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(answers).context("serialize telemetry answers")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}
