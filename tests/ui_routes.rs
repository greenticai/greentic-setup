//! Integration tests for the full Phase 1a router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use greentic_setup::ui::routes::build_router;
use greentic_setup::ui::state::{AppState, BundleMeta};
use std::sync::Arc;
use tower::ServiceExt;

fn state_with_token(token: &str) -> Arc<AppState> {
    Arc::new(AppState {
        bundle: BundleMeta::test_fixture(),
        port: 52341,
        bearer_token: zeroize::Zeroizing::new(token.to_string()),
        wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        shutdown_tx: tokio::sync::broadcast::channel::<()>(1).0,
    })
}

#[tokio::test]
async fn root_serves_html() {
    let app = build_router(state_with_token("t"));
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/html"));
}

#[tokio::test]
async fn security_headers_applied_to_root() {
    let app = build_router(state_with_token("t"));
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("x-content-type-options").unwrap(),
        "nosniff"
    );
    assert_eq!(resp.headers().get("x-frame-options").unwrap(), "DENY");
    assert!(
        resp.headers()
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("no-store")
    );
}

#[tokio::test]
async fn static_asset_served_from_manifest() {
    let app = build_router(state_with_token("t"));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/styles/tokens.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/css"));
}

#[tokio::test]
async fn unknown_static_asset_404s() {
    let app = build_router(state_with_token("t"));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/does/not/exist.txt")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_route_without_bearer_returns_401() {
    let app = build_router(state_with_token("correct-token"));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/bundle")
                .header("origin", "http://127.0.0.1:52341")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_route_with_wrong_origin_returns_403() {
    let app = build_router(state_with_token("correct-token"));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/bundle")
                .header("authorization", "Bearer correct-token")
                .header("origin", "http://evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn api_route_with_valid_auth_succeeds() {
    let app = build_router(state_with_token("correct-token"));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/bundle")
                .header("authorization", "Bearer correct-token")
                .header("origin", "http://127.0.0.1:52341")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
