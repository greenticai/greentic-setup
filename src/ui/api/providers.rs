//! `/api/providers` — extension provider management.
//!
//! Reads and writes the `extension_providers` list in bundle.yaml.
//! All mutations set `pending_mutations = true` so the topbar Apply button
//! becomes active.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::ui::api::error::ApiError;
use crate::ui::bundle_yaml;
use crate::ui::state::AppState;

// ── DTOs ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ProviderEntry {
    pub oci_ref: String,
    pub display_name: String,
    pub configured: bool,
}

#[derive(Debug, Serialize)]
pub struct ProvidersListResponse {
    pub providers: Vec<ProviderEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderMutationBody {
    pub oci_ref: String,
}

#[derive(Debug, Serialize)]
pub struct ProviderMutationResponse {
    pub success: bool,
    pub needs_rebuild: bool,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Derive a human-readable display name from an OCI ref string.
///
/// `oci://ghcr.io/greenticai/packs/messaging-slack:latest` → `"Messaging Slack"`
fn display_name_from_oci(oci_ref: &str) -> String {
    // Strip scheme and tag, take the last path segment.
    let stripped = oci_ref
        .trim_start_matches("oci://")
        .trim_start_matches("docker://");
    let segment = stripped
        .rsplit('/')
        .next()
        .unwrap_or(stripped);
    // Remove tag.
    let name = segment.split(':').next().unwrap_or(segment);
    // Title-case kebab.
    name.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(ch) => ch.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Check whether a provider OCI ref is "configured" (has a matching FormSpec loaded).
fn is_configured(oci_ref: &str, state: &AppState) -> bool {
    // Consider a provider configured if any FormSpec provider_id appears in
    // the OCI ref string (loose match for Phase 1b).
    state
        .provider_forms
        .iter()
        .any(|pf| oci_ref.contains(&pf.provider_id))
}

// ── GET /api/providers ────────────────────────────────────────────────────────

/// List extension providers from bundle.yaml.
pub async fn get_providers(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let doc = bundle_yaml::load(&state.bundle.path).map_err(|_| {
        ApiError::internal("bundle_yaml.read_failed", "ui.error.bundle_yaml_read_failed")
    })?;

    let providers: Vec<ProviderEntry> = doc
        .extension_providers
        .iter()
        .map(|oci_ref| ProviderEntry {
            display_name: display_name_from_oci(oci_ref),
            configured: is_configured(oci_ref, &state),
            oci_ref: oci_ref.clone(),
        })
        .collect();

    Ok(Json(ProvidersListResponse { providers }))
}

// ── POST /api/providers ───────────────────────────────────────────────────────

/// Add an extension provider OCI ref to bundle.yaml.
pub async fn post_provider(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProviderMutationBody>,
) -> Result<impl IntoResponse, ApiError> {
    validate_oci_ref(&body.oci_ref)?;

    let mut doc = bundle_yaml::load(&state.bundle.path).map_err(|_| {
        ApiError::internal("bundle_yaml.read_failed", "ui.error.bundle_yaml_read_failed")
    })?;

    let added = bundle_yaml::add_extension_provider(&mut doc, &body.oci_ref);

    if added {
        bundle_yaml::save(&state.bundle.path, &doc).map_err(|_| {
            ApiError::internal(
                "bundle_yaml.write_failed",
                "ui.error.bundle_yaml_write_failed",
            )
        })?;
        state.mark_pending();
    }

    Ok(Json(ProviderMutationResponse {
        success: true,
        needs_rebuild: true,
    }))
}

// ── DELETE /api/providers ─────────────────────────────────────────────────────

/// Remove an extension provider OCI ref from bundle.yaml.
pub async fn delete_provider(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProviderMutationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let mut doc = bundle_yaml::load(&state.bundle.path).map_err(|_| {
        ApiError::internal("bundle_yaml.read_failed", "ui.error.bundle_yaml_read_failed")
    })?;

    let removed = bundle_yaml::remove_extension_provider(&mut doc, &body.oci_ref);

    if removed {
        bundle_yaml::save(&state.bundle.path, &doc).map_err(|_| {
            ApiError::internal(
                "bundle_yaml.write_failed",
                "ui.error.bundle_yaml_write_failed",
            )
        })?;
        state.mark_pending();
    }

    Ok(Json(ProviderMutationResponse {
        success: true,
        needs_rebuild: true,
    }))
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Validate an OCI ref: must start with `oci://` and not contain path
/// traversal characters.
#[allow(clippy::result_large_err)]
fn validate_oci_ref(oci_ref: &str) -> Result<(), ApiError> {
    if oci_ref.is_empty() {
        return Err(ApiError::validation(
            "providers.empty_ref",
            "ui.providers.add_placeholder",
        ));
    }
    if oci_ref.contains("..") {
        return Err(ApiError::validation(
            "providers.path_traversal",
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
    fn display_name_from_standard_oci_ref() {
        let name = display_name_from_oci(
            "oci://ghcr.io/greenticai/packs/messaging/messaging-slack:latest",
        );
        assert_eq!(name, "Messaging Slack");
    }

    #[test]
    fn display_name_simple_name() {
        let name = display_name_from_oci("oci://ghcr.io/foo/slack:v1");
        assert_eq!(name, "Slack");
    }

    #[test]
    fn validate_oci_ref_rejects_empty() {
        assert!(validate_oci_ref("").is_err());
    }

    #[test]
    fn validate_oci_ref_rejects_traversal() {
        assert!(validate_oci_ref("oci://foo/../bar").is_err());
    }

    #[test]
    fn validate_oci_ref_accepts_valid() {
        assert!(validate_oci_ref("oci://ghcr.io/greenticai/packs/slack:latest").is_ok());
    }
}
