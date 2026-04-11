//! `/api/rebuild` — trigger a setup engine re-run for all known scopes.
//!
//! Phase 1b rebuild re-runs `SetupEngine::plan(Update) + execute` for each
//! configured (tenant, env, team) scope. This re-persists secrets and
//! refreshes bundle state without invoking an external `gtc build` binary —
//! that is deferred to a later phase.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;
use tracing::info;

use crate::engine::{SetupConfig, SetupEngine, SetupRequest};
use crate::plan::SetupMode;
use crate::ui::api::error::ApiError;
use crate::ui::state::{AppState, ScopeKey};

// ── DTOs ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct RebuildResponse {
    pub success: bool,
    pub scopes_rebuilt: usize,
}

#[derive(Debug, Serialize)]
pub struct PendingResponse {
    pub pending: bool,
}

// ── GET /api/rebuild/pending ──────────────────────────────────────────────────

/// Check whether any mutations are pending (i.e. a rebuild is needed).
pub async fn get_rebuild_pending(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    Json(PendingResponse {
        pending: state.is_pending(),
    })
}

// ── POST /api/rebuild ─────────────────────────────────────────────────────────

/// Trigger a setup engine update run for every configured scope.
///
/// Runs inside `spawn_blocking` because `SetupEngine::execute` is synchronous
/// and may do filesystem I/O.
pub async fn post_rebuild(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let bundle_path: PathBuf = state.bundle.path.clone();
    let scopes: Vec<ScopeKey> = state
        .bundle
        .scopes
        .iter()
        .map(|s| s.scope.clone())
        .collect();

    if scopes.is_empty() {
        // Nothing to rebuild — still consider it a success.
        state.clear_pending();
        return Ok(Json(RebuildResponse {
            success: true,
            scopes_rebuilt: 0,
        }));
    }

    // Clone what we need for the blocking task.
    let scopes_for_task = scopes.clone();

    let result = tokio::task::spawn_blocking(move || {
        run_rebuild_sync(bundle_path.as_path(), &scopes_for_task)
    })
    .await
    .map_err(|join_err| {
        ApiError::internal(
            "rebuild.task_panic",
            "ui.error.rebuild_failed",
        )
        .with_params(serde_json::json!({ "reason": join_err.to_string() }))
    })?;

    let scopes_rebuilt = result.map_err(|err| {
        ApiError::internal("rebuild.failed", "ui.error.rebuild_failed")
            .with_params(serde_json::json!({ "reason": err.to_string() }))
    })?;

    state.clear_pending();

    Ok(Json(RebuildResponse {
        success: true,
        scopes_rebuilt,
    }))
}

// ── Synchronous rebuild logic ─────────────────────────────────────────────────

/// Run `SetupEngine::plan(Update) + execute` for each scope.
///
/// Returns the count of scopes that were successfully rebuilt.
/// Errors from individual scopes are logged but do not abort the loop —
/// the total count reflects only successes.
fn run_rebuild_sync(bundle_path: &Path, scopes: &[ScopeKey]) -> anyhow::Result<usize> {
    let mut rebuilt = 0usize;

    for scope in scopes {
        let config = SetupConfig {
            tenant: scope.tenant.clone(),
            team: Some(scope.team.clone()),
            env: scope.env.clone(),
            offline: false,
            verbose: false,
        };
        let engine = SetupEngine::new(config);

        let request = SetupRequest {
            bundle: bundle_path.to_path_buf(),
            ..Default::default()
        };

        match engine.plan(SetupMode::Update, &request, false) {
            Ok(plan) => match engine.execute(&plan) {
                Ok(_) => {
                    info!(
                        tenant = %scope.tenant,
                        env = %scope.env,
                        team = %scope.team,
                        "rebuild: scope refreshed"
                    );
                    rebuilt += 1;
                }
                Err(err) => {
                    tracing::warn!(
                        tenant = %scope.tenant,
                        env = %scope.env,
                        team = %scope.team,
                        error = %err,
                        "rebuild: execute failed for scope (skipping)"
                    );
                }
            },
            Err(err) => {
                tracing::warn!(
                    tenant = %scope.tenant,
                    env = %scope.env,
                    team = %scope.team,
                    error = %err,
                    "rebuild: plan failed for scope (skipping)"
                );
            }
        }
    }

    Ok(rebuilt)
}
