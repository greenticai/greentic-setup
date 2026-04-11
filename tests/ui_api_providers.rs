//! Integration tests for `/api/providers` endpoints.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{delete, get, post};
use greentic_setup::ui::api::providers::{delete_provider, get_providers, post_provider};
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
        .route("/api/providers", get(get_providers))
        .route("/api/providers", post(post_provider))
        .route("/api/providers", delete(delete_provider))
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

/// GET /api/providers returns empty list for a fresh bundle.
#[tokio::test]
async fn list_returns_empty_for_fresh_bundle() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());
    let (status, body) = send(&app, Method::GET, "/api/providers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["providers"].is_array());
    assert_eq!(body["providers"].as_array().unwrap().len(), 0);
}

/// POST /api/providers adds a provider.
#[tokio::test]
async fn add_provider_appends_to_bundle_yaml() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/providers",
        Some(json!({
            "oci_ref": "oci://ghcr.io/greenticai/packs/messaging-slack:latest"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["needs_rebuild"], true);

    // Verify it appears in the list.
    let (status2, body2) = send(&app, Method::GET, "/api/providers", None).await;
    assert_eq!(status2, StatusCode::OK);
    let providers = body2["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1);
    assert_eq!(
        providers[0]["oci_ref"],
        "oci://ghcr.io/greenticai/packs/messaging-slack:latest"
    );
}

/// DELETE /api/providers removes a provider.
#[tokio::test]
async fn remove_provider_removes_from_bundle_yaml() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());

    // Add first.
    send(
        &app,
        Method::POST,
        "/api/providers",
        Some(json!({ "oci_ref": "oci://ghcr.io/foo:latest" })),
    )
    .await;

    // Remove.
    let (status, body) = send(
        &app,
        Method::DELETE,
        "/api/providers",
        Some(json!({ "oci_ref": "oci://ghcr.io/foo:latest" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], true);

    // Verify list is empty again.
    let (_, list_body) = send(&app, Method::GET, "/api/providers", None).await;
    let providers = list_body["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 0);
}

/// POST /api/providers rejects path-traversal in OCI ref.
#[tokio::test]
async fn add_provider_rejects_path_traversal() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/providers",
        Some(json!({ "oci_ref": "oci://foo/../bar:latest" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}

/// POST /api/providers is idempotent (adding the same ref twice is OK).
#[tokio::test]
async fn add_provider_idempotent() {
    let dir = tempdir().unwrap();
    let (app, _) = app(dir.path());

    send(
        &app,
        Method::POST,
        "/api/providers",
        Some(json!({ "oci_ref": "oci://foo:latest" })),
    )
    .await;
    send(
        &app,
        Method::POST,
        "/api/providers",
        Some(json!({ "oci_ref": "oci://foo:latest" })),
    )
    .await;

    let (_, list_body) = send(&app, Method::GET, "/api/providers", None).await;
    let providers = list_body["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "duplicate entry was added");
}
