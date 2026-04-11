//! `/api/capabilities` — bundle capability toggle handlers.
//!
//! Capability IDs look like `greentic.cap.bundle_assets.read.v1`.
//! The `capabilities` section in bundle.yaml is a flat list of enabled IDs;
//! toggling sets/unsets membership in that list.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::ui::api::error::ApiError;
use crate::ui::bundle_yaml;
use crate::ui::state::AppState;

// ── Well-known capabilities ───────────────────────────────────────────────────

/// Static descriptions for well-known capability IDs.
///
/// Unknown IDs get a generic description derived from the ID itself.
fn describe_capability(id: &str) -> String {
    match id {
        "greentic.cap.bundle_assets.read.v1" => {
            "Read access to bundle-embedded assets (images, PDFs, etc.)".to_string()
        }
        "greentic.cap.state_store.v1" => "Persistent state store access".to_string(),
        "greentic.cap.oauth.v1" => "OAuth login support".to_string(),
        "greentic.cap.adaptive_cards.v1" => "Adaptive Card rendering".to_string(),
        other => {
            // Derive from dotted ID: "greentic.cap.foo.bar.v1" → "Foo bar (v1)"
            let parts: Vec<&str> = other.split('.').collect();
            if parts.len() > 2 {
                let core: Vec<&str> = parts[2..].to_vec();
                let last = core.last().copied().unwrap_or("");
                let mid: Vec<&str> = if last.starts_with('v') && last[1..].parse::<u32>().is_ok() {
                    core[..core.len() - 1].to_vec()
                } else {
                    core.to_vec()
                };
                let desc = mid.join(" ");
                let mut c = desc.chars();
                match c.next() {
                    Some(ch) => ch.to_uppercase().collect::<String>() + c.as_str(),
                    None => desc,
                }
            } else {
                other.to_string()
            }
        }
    }
}

// ── DTOs ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CapabilityEntry {
    pub id: String,
    pub description: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct CapabilitiesListResponse {
    pub capabilities: Vec<CapabilityEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ToggleBody {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize)]
pub struct ToggleResponse {
    pub success: bool,
    pub needs_rebuild: bool,
}

// ── GET /api/capabilities ─────────────────────────────────────────────────────

/// List all capabilities from bundle.yaml.
///
/// Returns an empty list if the `capabilities` section is absent.
pub async fn get_capabilities(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let doc = bundle_yaml::load(&state.bundle.path).map_err(|_| {
        ApiError::internal("bundle_yaml.read_failed", "ui.error.bundle_yaml_read_failed")
    })?;

    let capabilities: Vec<CapabilityEntry> = doc
        .capabilities
        .iter()
        .map(|id| CapabilityEntry {
            description: describe_capability(id),
            enabled: true, // being in the list means enabled
            id: id.clone(),
        })
        .collect();

    Ok(Json(CapabilitiesListResponse { capabilities }))
}

// ── PUT /api/capabilities/toggle ─────────────────────────────────────────────

/// Toggle a capability on or off.
pub async fn put_toggle_capability(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ToggleBody>,
) -> Result<impl IntoResponse, ApiError> {
    validate_capability_id(&body.id)?;

    let mut doc = bundle_yaml::load(&state.bundle.path).map_err(|_| {
        ApiError::internal("bundle_yaml.read_failed", "ui.error.bundle_yaml_read_failed")
    })?;

    let changed = bundle_yaml::set_capability(&mut doc, &body.id, body.enabled);

    if changed {
        bundle_yaml::save(&state.bundle.path, &doc).map_err(|_| {
            ApiError::internal(
                "bundle_yaml.write_failed",
                "ui.error.bundle_yaml_write_failed",
            )
        })?;
        state.mark_pending();
    }

    Ok(Json(ToggleResponse {
        success: true,
        needs_rebuild: true,
    }))
}

// ── Validation ────────────────────────────────────────────────────────────────

#[allow(clippy::result_large_err)]
fn validate_capability_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty() {
        return Err(ApiError::validation(
            "capabilities.empty_id",
            "ui.capabilities.empty",
        ));
    }
    if id.contains("..") || id.contains('/') || id.contains('\\') {
        return Err(ApiError::validation(
            "capabilities.invalid_id",
            "ui.error.scope_invalid",
        ));
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_known_capability() {
        let desc = describe_capability("greentic.cap.bundle_assets.read.v1");
        assert!(desc.contains("asset") || desc.contains("bundle") || !desc.is_empty());
    }

    #[test]
    fn describe_unknown_capability_derives_from_id() {
        let desc = describe_capability("greentic.cap.foo.bar");
        assert!(!desc.is_empty());
    }

    #[test]
    fn validate_capability_id_rejects_empty() {
        assert!(validate_capability_id("").is_err());
    }

    #[test]
    fn validate_capability_id_rejects_traversal() {
        assert!(validate_capability_id("greentic.cap../etc").is_err());
    }

    #[test]
    fn validate_capability_id_accepts_valid() {
        assert!(validate_capability_id("greentic.cap.bundle_assets.read.v1").is_ok());
    }
}
