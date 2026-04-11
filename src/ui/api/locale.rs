//! `GET /api/locale/{code}` — return i18n catalog as JSON.
//! `POST /api/shutdown` — trigger graceful shutdown.
//!
//! Phase 1a embeds the English catalog via `include_str!` and returns it
//! for any locale code. Real locale resolution lands in Task 34 (cutover).

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::ui::api::error::ApiError;
use crate::ui::state::AppState;

/// English catalog embedded at build time. Used as the only catalog in
/// Phase 1a — real multi-locale loading is Task 34 (cutover).
const EN_CATALOG_JSON: &str = include_str!("../../../i18n/en.json");

pub async fn get_locale(
    State(_state): State<Arc<AppState>>,
    Path(_code): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    // TODO(task-34): Resolve the actual locale from `greentic-i18n` instead
    // of always returning the embedded English catalog.
    let catalog: Value = serde_json::from_str(EN_CATALOG_JSON).map_err(|_| {
        ApiError::internal("locale.catalog_parse_failed", "ui.error.internal")
    })?;
    Ok(Json(catalog))
}

pub async fn post_shutdown(
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    // Best-effort: send ignores the error when no receivers are listening.
    let _ = state.shutdown_tx.send(());
    Ok(Json(json!({ "shutdown": true })))
}
