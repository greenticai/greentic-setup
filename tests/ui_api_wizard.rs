//! Integration tests for wizard endpoints (Tasks 14-17).

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use greentic_setup::ui::api::wizard::{
    wizard_execute, wizard_next, wizard_session, wizard_start,
};
use greentic_setup::ui::state::{AppState, BundleMeta};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

fn app() -> Router {
    let state = Arc::new(AppState {
        bundle: BundleMeta::test_fixture(),
        port: 12345,
        bearer_token: zeroize::Zeroizing::new("tok".to_string()),
        wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        shutdown_tx: tokio::sync::broadcast::channel::<()>(1).0,
    });
    Router::new()
        .route("/api/wizard/start", get(wizard_start))
        .route("/api/wizard/next", post(wizard_next))
        .route("/api/wizard/execute", post(wizard_execute))
        .route("/api/wizard/session/{id}", get(wizard_session))
        .with_state(state)
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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn start_creates_session_with_first_step() {
    let app = app();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_step"], 1);
    assert_eq!(body["total_steps"], 3);
    assert!(body["id"].is_string());
    assert_eq!(body["step"]["title_key"], "ui.wizard.stub.step1.title");
    assert_eq!(body["scope"]["tenant"], "demo");
}

#[tokio::test]
async fn start_rejects_bad_scope() {
    let app = app();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=evil&env=dev&team=default",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "scope.invalid_tenant");
}

#[tokio::test]
async fn next_advances_step_counter() {
    let app = app();
    let (_, start_body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    let session_id = start_body["id"].as_str().unwrap();

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/wizard/next",
        Some(json!({ "session_id": session_id, "answers": { "field_1": "value" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_step"], 2);
    assert_eq!(body["step"]["title_key"], "ui.wizard.stub.step2.title");
}

#[tokio::test]
async fn next_returns_done_after_last_step() {
    let app = app();
    let (_, start_body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    let id = start_body["id"].as_str().unwrap();

    // Advance through all 3 steps
    for _ in 0..3 {
        send(
            &app,
            Method::POST,
            "/api/wizard/next",
            Some(json!({ "session_id": id, "answers": {} })),
        )
        .await;
    }
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/wizard/next",
        Some(json!({ "session_id": id, "answers": {} })),
    )
    .await;
    // After exceeding total_steps, step is null (done).
    assert_eq!(status, StatusCode::OK);
    assert!(body["step"].is_null());
}

#[tokio::test]
async fn next_rejects_unknown_session() {
    let app = app();
    let bogus = uuid::Uuid::new_v4();
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/wizard/next",
        Some(json!({ "session_id": bogus, "answers": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "wizard.session_not_found");
}

#[tokio::test]
async fn execute_finalizes_and_drops_session() {
    let app = app();
    let (_, start_body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    let id = start_body["id"].as_str().unwrap();

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/wizard/execute",
        Some(json!({ "session_id": id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["success"], true);
    assert_eq!(body["message_key"], "ui.wizard.execute.success");

    // Session is gone — next lookup returns 404.
    let (status2, _) = send(
        &app,
        Method::GET,
        &format!("/api/wizard/session/{}", id),
        None,
    )
    .await;
    assert_eq!(status2, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn execute_rejects_unknown_session() {
    let app = app();
    let bogus = uuid::Uuid::new_v4();
    let (status, _) = send(
        &app,
        Method::POST,
        "/api/wizard/execute",
        Some(json!({ "session_id": bogus })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn session_lookup_returns_state() {
    let app = app();
    let (_, start_body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    let id = start_body["id"].as_str().unwrap();
    let (status, body) = send(
        &app,
        Method::GET,
        &format!("/api/wizard/session/{}", id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_step"], 1);
    assert_eq!(body["step"]["title_key"], "ui.wizard.stub.step1.title");
}

#[tokio::test]
async fn session_lookup_404s_for_unknown_id() {
    let app = app();
    let bogus = uuid::Uuid::new_v4();
    let (status, _) = send(
        &app,
        Method::GET,
        &format!("/api/wizard/session/{}", bogus),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
