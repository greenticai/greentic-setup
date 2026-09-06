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
///
/// MERGES field-by-field into whatever is already recorded: a `None` field
/// leaves the persisted value alone. The two fields are written by different
/// code paths at different times — `mode` comes from the wizard/UI tunnel
/// selection (and is re-sent by the UI's debounced draft autosave, by
/// `/api/setup-action` and by `/api/execute`), while `tunnel_id` is written by
/// the managed-tunnel resolver at the point the tunnel is acquired. With
/// whole-value overwrite, any of those mode-only writes landing after the id was
/// recorded silently reduced the artifact back to `{"mode":"gtunnel"}`, and
/// greentic-start then re-derived an id that nothing had been registered
/// against.
pub fn persist_tunnel_artifact(bundle_root: &Path, answers: &TunnelAnswers) -> Result<PathBuf> {
    let path = tunnel_artifact_path(bundle_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A tunnel.json we cannot parse carries nothing worth preserving, so an
    // unreadable/corrupt artifact is simply replaced rather than failing here.
    let merged = merge_tunnel_answers(load_tunnel_artifact(bundle_root).ok().flatten(), answers);
    let payload = serde_json::to_string_pretty(&merged).context("serialize tunnel answers")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Overlay the `Some` fields of `incoming` onto `existing`. Split out so the
/// merge rule is testable without touching the filesystem.
fn merge_tunnel_answers(
    existing: Option<TunnelAnswers>,
    incoming: &TunnelAnswers,
) -> TunnelAnswers {
    let mut merged = existing.unwrap_or_default();
    if incoming.mode.is_some() {
        merged.mode = incoming.mode.clone();
    }
    if incoming.tunnel_id.is_some() {
        merged.tunnel_id = incoming.tunnel_id.clone();
    }
    merged
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_only(mode: &str) -> TunnelAnswers {
        TunnelAnswers {
            mode: Some(mode.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn persisting_tunnel_mode_preserves_a_recorded_tunnel_id() {
        // Reproduces the wipe: setup records the resolved gtunnel id, then a
        // later mode-only write in the SAME run (UI draft autosave /
        // setup-action / execute) reduced the artifact to {"mode":"gtunnel"} and
        // stranded every provider URL registered against that id.
        let bundle = tempfile::tempdir().expect("tempdir");
        persist_tunnel_artifact(
            bundle.path(),
            &TunnelAnswers {
                mode: Some("gtunnel".to_string()),
                tunnel_id: Some("stating-81015".to_string()),
            },
        )
        .expect("seed both fields");

        persist_tunnel_artifact(bundle.path(), &mode_only("gtunnel")).expect("re-persist mode");

        let saved = load_tunnel_artifact(bundle.path())
            .expect("load")
            .expect("artifact present");
        assert_eq!(saved.mode.as_deref(), Some("gtunnel"));
        assert_eq!(
            saved.tunnel_id.as_deref(),
            Some("stating-81015"),
            "a mode-only write must not drop the recorded tunnel id"
        );
    }

    #[test]
    fn persisting_a_tunnel_id_preserves_the_recorded_mode() {
        let bundle = tempfile::tempdir().expect("tempdir");
        persist_tunnel_artifact(bundle.path(), &mode_only("gtunnel")).expect("seed mode");

        persist_tunnel_artifact(
            bundle.path(),
            &TunnelAnswers {
                mode: None,
                tunnel_id: Some("acme-0a1b2".to_string()),
            },
        )
        .expect("record id");

        let saved = load_tunnel_artifact(bundle.path())
            .expect("load")
            .expect("artifact present");
        assert_eq!(saved.mode.as_deref(), Some("gtunnel"));
        assert_eq!(saved.tunnel_id.as_deref(), Some("acme-0a1b2"));
    }

    #[test]
    fn persisting_tunnel_answers_still_replaces_explicit_values() {
        // Merge must not turn into "append only": a NEW value for a field the
        // caller actually set has to win, or switching tunnel mode would be
        // impossible.
        let bundle = tempfile::tempdir().expect("tempdir");
        persist_tunnel_artifact(
            bundle.path(),
            &TunnelAnswers {
                mode: Some("gtunnel".to_string()),
                tunnel_id: Some("old-11111".to_string()),
            },
        )
        .expect("seed");

        persist_tunnel_artifact(
            bundle.path(),
            &TunnelAnswers {
                mode: Some("off".to_string()),
                tunnel_id: Some("new-22222".to_string()),
            },
        )
        .expect("replace");

        let saved = load_tunnel_artifact(bundle.path())
            .expect("load")
            .expect("artifact present");
        assert_eq!(saved.mode.as_deref(), Some("off"));
        assert_eq!(saved.tunnel_id.as_deref(), Some("new-22222"));
    }

    #[test]
    fn persisting_over_a_corrupt_tunnel_artifact_replaces_it() {
        let bundle = tempfile::tempdir().expect("tempdir");
        let path = tunnel_artifact_path(bundle.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "{").expect("write corrupt");

        persist_tunnel_artifact(bundle.path(), &mode_only("ngrok")).expect("persist");

        let saved = load_tunnel_artifact(bundle.path())
            .expect("load")
            .expect("artifact present");
        assert_eq!(saved.mode.as_deref(), Some("ngrok"));
        assert_eq!(saved.tunnel_id, None);
    }
}
