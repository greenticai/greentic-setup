//! Integration tests for `/api/capabilities` endpoints.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, put};
use greentic_setup::ui::api::capabilities::{get_capabilities, put_toggle_capability};
use greentic_setup::ui::state::{AppState, BundleMeta};
use serde_json::{Value, json};
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
        .route("/api/capabilities", get(get_capabilities))
        .route("/api/capabilities/toggle", put(put_toggle_capability))
        .with_state(state.clone());
    (router, state)
}

async fn send(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = if let Some(b) = body {
        req = req.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&b).unwrap())
    } else {
        Body::empty()
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

/// GET /api/capabilities returns empty list for fresh bundle.
#[tokio::test]
async fn list_capabilities_empty_for_fresh_bundle() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());
    let (status, body) = send(&app, Method::GET, "/api/capabilities", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["capabilities"].is_array());
    assert_eq!(body["capabilities"].as_array().unwrap().len(), 0);
}

/// PUT /api/capabilities/toggle enables a capability.
#[tokio::test]
async fn toggle_on_adds_capability() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());

    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/capabilities/toggle",
        Some(json!({
            "id": "greentic.cap.bundle_assets.read.v1",
            "enabled": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["needs_rebuild"], true);

    // Verify it appears in the list.
    let (_, list_body) = send(&app, Method::GET, "/api/capabilities", None).await;
    let caps = list_body["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0]["id"], "greentic.cap.bundle_assets.read.v1");
    assert_eq!(caps[0]["enabled"], true);
}

/// PUT /api/capabilities/toggle disables a capability.
#[tokio::test]
async fn toggle_off_removes_capability() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());

    // Enable first.
    send(
        &app,
        Method::PUT,
        "/api/capabilities/toggle",
        Some(json!({ "id": "greentic.cap.bundle_assets.read.v1", "enabled": true })),
    )
    .await;

    // Disable.
    let (status, _) = send(
        &app,
        Method::PUT,
        "/api/capabilities/toggle",
        Some(json!({ "id": "greentic.cap.bundle_assets.read.v1", "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // List should be empty.
    let (_, list_body) = send(&app, Method::GET, "/api/capabilities", None).await;
    assert_eq!(list_body["capabilities"].as_array().unwrap().len(), 0);
}

/// PUT /api/capabilities/toggle with bundle.yaml that already has capabilities.
#[tokio::test]
async fn toggle_reads_existing_bundle_yaml() {
    let dir = tempdir().unwrap();
    // Write a bundle.yaml with a capability pre-set.
    std::fs::write(
        dir.path().join("bundle.yaml"),
        "capabilities:\n  - greentic.cap.oauth.v1\n",
    )
    .unwrap();

    let (app, _) = app(dir.path());
    let (_, list_body) = send(&app, Method::GET, "/api/capabilities", None).await;
    let caps = list_body["capabilities"].as_array().unwrap();
    assert_eq!(caps.len(), 1);
    assert_eq!(caps[0]["id"], "greentic.cap.oauth.v1");
}
