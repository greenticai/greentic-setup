//! `GET /api/bundle` — returns bundle metadata from `AppState`.
//!
//! Bundle discovery happens at server boot time (see `src/ui/server.rs`).
//! This handler is pure read-from-state: it never touches the filesystem.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;

use crate::ui::state::{AppState, BundleMeta};

/// Handler for `GET /api/bundle`.
///
/// Returns the currently loaded `BundleMeta` as JSON. Content-Type is set
/// automatically by axum's `Json` extractor.
pub async fn get_bundle(State(state): State<Arc<AppState>>) -> Json<BundleMeta> {
    Json(state.bundle.clone())
}
