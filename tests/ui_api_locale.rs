//! Tests for locale + shutdown endpoints.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use greentic_setup::ui::api::locale::{get_locale, post_shutdown};
use greentic_setup::ui::state::{AppState, BundleMeta};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn app() -> (Router, Arc<AppState>) {
    let (tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = Arc::new(AppState {
        bundle: BundleMeta::test_fixture(),
        port: 12345,
        bearer_token: zeroize::Zeroizing::new("tok".to_string()),
        wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        shutdown_tx: tx,
        launch_options: Default::default(),
        provider_forms: vec![],
    });
    let router = Router::new()
        .route("/api/locale/{code}", get(get_locale))
        .route("/api/shutdown", post(post_shutdown))
        .with_state(state.clone());
    (router, state)
}

async fn send(app: Router, method: Method, uri: &str) -> (StatusCode, Value) {
    let resp = app
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
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn get_locale_returns_catalog() {
    let (app, _) = app();
    let (status, body) = send(app, Method::GET, "/api/locale/en").await;
    assert_eq!(status, StatusCode::OK);
    // Must be a JSON object with at least one key-value pair from the catalog.
    assert!(body.is_object());
    assert!(!body.as_object().unwrap().is_empty(), "catalog empty");
}

#[tokio::test]
async fn get_locale_accepts_any_code_in_phase_1a() {
    // Phase 1a falls back to English for any code. Real locale resolution
    // is deferred to Task 34 (cutover).
    let (app, _) = app();
    let (status, _) = send(app, Method::GET, "/api/locale/id").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn post_shutdown_fires_broadcast() {
    let (app, state) = app();
    let mut rx = state.shutdown_tx.subscribe();
    let (status, body) = send(app, Method::POST, "/api/shutdown").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["shutdown"], true);
    // The broadcast should deliver (`tokio::time::timeout` makes it
    // hermetic even if send didn't fire).
    let recv = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
        .await
        .expect("shutdown signal not received within 100ms");
    assert!(recv.is_ok());
}
