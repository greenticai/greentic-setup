//! Integration tests for `/api/rebuild` endpoints.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use greentic_setup::ui::api::rebuild::{get_rebuild_pending, post_rebuild};
use greentic_setup::ui::state::{AppState, BundleMeta};
use serde_json::Value;
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

fn make_bundle_in(dir: &std::path::Path) -> BundleMeta {
    let mut b = BundleMeta::test_fixture();
    b.path = dir.to_path_buf();
    b
}

fn app(dir: &std::path::Path) -> (Router, Arc<AppState>) {
    let state = AppState::test_with(make_bundle_in(dir), 12345, "tok", vec![]);
    let router = Router::new()
        .route("/api/rebuild", post(post_rebuild))
        .route("/api/rebuild/pending", get(get_rebuild_pending))
        .with_state(state.clone());
    (router, state)
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// GET /api/rebuild/pending returns false initially.
#[tokio::test]
async fn pending_is_false_initially() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());
    let (status, body) = send(&app, Method::GET, "/api/rebuild/pending").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], false);
}

/// pending_mutations flag is set by mark_pending and cleared by clear_pending.
#[tokio::test]
async fn pending_flag_lifecycle() {
    let dir = tempdir().unwrap();
    let (app, state) = app(dir.path());

    // Initially false.
    assert!(!state.is_pending());

    // Mark pending.
    state.mark_pending();
    assert!(state.is_pending());

    // GET /api/rebuild/pending should reflect true.
    let (status, body) = send(&app, Method::GET, "/api/rebuild/pending").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], true);

    // Clear.
    state.clear_pending();
    assert!(!state.is_pending());
}

/// POST /api/rebuild with no configured scopes succeeds immediately.
#[tokio::test]
async fn rebuild_with_no_scopes_succeeds() {
    let dir = tempdir().unwrap();
    let (app, state) = app(dir.path());

    // Mark pending first so we can verify it's cleared.
    state.mark_pending();
    assert!(state.is_pending());

    let (status, body) = send(&app, Method::POST, "/api/rebuild").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["scopes_rebuilt"], 0);

    // pending should be cleared.
    assert!(!state.is_pending());
}

/// POST /api/rebuild clears pending flag after execution.
#[tokio::test]
async fn rebuild_clears_pending_after_success() {
    let dir = tempdir().unwrap();
    let (app, state) = app(dir.path());
    state.mark_pending();

    let (status, body) = send(&app, Method::POST, "/api/rebuild").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(!state.is_pending(), "pending should be cleared after rebuild");
}
