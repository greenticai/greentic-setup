//! `GET /api/locale/{code}` — return i18n catalog as JSON.
//! `POST /api/shutdown` — trigger graceful shutdown.
//!
//! Catalogs are embedded at compile time via `ui::locales` (which wraps
//! `include_dir!`). The handler filters to `ui.*` keys only so `cli.*`
//! copy (which contains the hyphenated crate name) never leaks to the
//! dashboard SPA.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde_json::{Value, json};

use crate::ui::api::error::ApiError;
use crate::ui::locales;
use crate::ui::state::AppState;

pub async fn get_locale(
    State(_state): State<Arc<AppState>>,
    Path(code): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let catalog: Value = locales::catalog_for(&code).ok_or_else(|| {
        ApiError::not_found("locale.not_found", "ui.error.locale_not_found")
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
