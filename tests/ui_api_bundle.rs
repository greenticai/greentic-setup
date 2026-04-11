//! Tests for the GET /api/bundle endpoint.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use greentic_setup::ui::api::bundle::get_bundle;
use greentic_setup::ui::state::{AppState, BundleMeta};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn app() -> Router {
    let state = Arc::new(AppState {
        bundle: BundleMeta::test_fixture(),
        port: 12345,
        bearer_token: zeroize::Zeroizing::new("test-token".to_string()),
        wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        shutdown_tx: tokio::sync::broadcast::channel::<()>(1).0,
    });
    Router::new()
        .route("/api/bundle", get(get_bundle))
        .with_state(state)
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn returns_bundle_metadata_200() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/api/bundle")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn response_has_expected_bundle_shape() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/api/bundle")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body["id"], "demo");
    assert_eq!(body["display_name"], "Demo Bundle");
    assert!(body["available_tenants"].is_array());
    assert_eq!(body["available_tenants"][0], "demo");
    assert_eq!(body["available_tenants"][1], "acme-corp");
    assert_eq!(body["available_envs"][0], "dev");
    assert_eq!(body["available_teams"][0], "default");
}

#[tokio::test]
async fn response_is_json_content_type() {
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/api/bundle")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("application/json"));
}
