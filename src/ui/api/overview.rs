//! `GET /api/overview?tenant=&env=&team=` — per-scope dashboard summary.
//!
//! Validates the requested scope against the bundle allow-list and
//! returns stats + the full list of configured scopes.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::ui::api::error::ApiError;
use crate::ui::state::{AppState, OverviewResponse, OverviewStats, ScopeKey, validate_scope};

#[derive(Debug, Deserialize)]
pub struct OverviewQuery {
    pub tenant: String,
    pub env: String,
    pub team: String,
}

/// Handler for `GET /api/overview`.
pub async fn get_overview(
    State(state): State<Arc<AppState>>,
    Query(q): Query<OverviewQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = ScopeKey {
        tenant: q.tenant,
        env: q.env,
        team: q.team,
    };

    // Validate against bundle allow-list + path traversal.
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;

    // Aggregate stats across all configured scopes.
    let scopes = state.bundle.scopes.clone();
    let scopes_count = scopes.len() as u32;
    let providers_count: u32 = scopes.iter().map(|s| s.providers.len() as u32).sum();
    let secrets_count: u32 = scopes
        .iter()
        .flat_map(|s| s.providers.iter().map(|p| p.secrets_count))
        .sum();
    let warnings_count: u32 = scopes
        .iter()
        .map(|s| {
            s.warnings.len() as u32
                + s.providers
                    .iter()
                    .map(|p| p.warnings.len() as u32)
                    .sum::<u32>()
        })
        .sum();

    Ok(Json(OverviewResponse {
        scope,
        stats: OverviewStats {
            scopes_count,
            providers_count,
            secrets_count,
            warnings_count,
        },
        scopes,
    }))
}
