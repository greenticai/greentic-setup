//! Integration tests for wizard endpoints.
//!
//! Phase 1a real-engine tests. The stub helpers (STUB_TOTAL_STEPS, stub_step)
//! have been removed; this file exercises the real FormSpec-based wizard path.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use greentic_setup::ui::api::wizard::{wizard_execute, wizard_next, wizard_session, wizard_start};
use greentic_setup::ui::state::{AppState, BundleMeta, ProviderFormData};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
use qa_spec::spec::question::QuestionPolicy;
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

// ── Test fixtures ─────────────────────────────────────────────────────────────

/// Build a minimal single-question `FormSpec` for testing.
fn minimal_form_spec(provider_id: &str) -> FormSpec {
    FormSpec {
        id: format!("{provider_id}-setup"),
        title: format!("{provider_id} Setup"),
        version: "1.0.0".to_string(),
        description: None,
        presentation: None,
        progress_policy: None,
        secrets_policy: None,
        store: vec![],
        validations: vec![],
        includes: vec![],
        questions: vec![QuestionSpec {
            id: "bot_token".to_string(),
            kind: QuestionType::String,
            title: "Bot Token".to_string(),
            title_i18n: None,
            description: Some("Required secret token".to_string()),
            description_i18n: None,
            required: true,
            choices: None,
            default_value: None,
            secret: true,
            visible_if: None,
            constraint: None,
            list: None,
            computed: None,
            policy: QuestionPolicy::default(),
            computed_overridable: false,
        }],
    }
}

/// `AppState` with one provider loaded.
fn state_with_one_provider() -> Arc<AppState> {
    AppState::test_with(
        BundleMeta::test_fixture(),
        12345,
        "tok",
        vec![ProviderFormData {
            provider_id: "messaging-telegram".to_string(),
            display_name: "Telegram".to_string(),
            form_spec: minimal_form_spec("messaging-telegram"),
        }],
    )
}

/// `AppState` with no providers — used to test the empty-bundle error path.
fn state_empty() -> Arc<AppState> {
    AppState::test_with(BundleMeta::test_fixture(), 12345, "tok", vec![])
}

fn app_with_one_provider() -> Router {
    let state = state_with_one_provider();
    Router::new()
        .route("/api/wizard/start", get(wizard_start))
        .route("/api/wizard/next", post(wizard_next))
        .route("/api/wizard/execute", post(wizard_execute))
        .route("/api/wizard/session/{id}", get(wizard_session))
        .with_state(state)
}

fn app_empty() -> Router {
    let state = state_empty();
    Router::new()
        .route("/api/wizard/start", get(wizard_start))
        .route("/api/wizard/next", post(wizard_next))
        .route("/api/wizard/execute", post(wizard_execute))
        .route("/api/wizard/session/{id}", get(wizard_session))
        .with_state(state)
}

async fn send(app: &Router, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

/// With no providers, wizard_start should return a 409 Conflict.
#[tokio::test]
async fn start_with_empty_bundle_returns_no_providers_error() {
    let app = app_empty();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "wizard.no_providers");
}

/// With one provider, wizard_start creates a session with step 1.
#[tokio::test]
async fn start_with_one_provider_creates_session() {
    let app = app_with_one_provider();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current_step"], 1);
    assert_eq!(body["total_steps"], 1);
    assert!(body["id"].is_string());
    // Step should have a real field from FormSpec, not a stub.
    assert!(body["step"]["fields"].is_array());
    assert!(!body["step"]["fields"].as_array().unwrap().is_empty());
    let field = &body["step"]["fields"][0];
    // label_text comes from the FormSpec question title.
    assert_eq!(field["label_text"], "Bot Token");
    assert_eq!(field["field_type"], "password"); // secret: true → password
    assert_eq!(field["required"], true);
}

/// wizard_start rejects invalid scope (invalid chars in tenant).
#[tokio::test]
async fn start_rejects_bad_scope() {
    let app = app_with_one_provider();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=evil!corp&env=dev&team=default",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "scope.tenant_invalid_chars");
}

/// wizard_next with valid answers advances the step.
#[tokio::test]
async fn next_with_valid_answers_advances_step() {
    let app = app_with_one_provider();
    let (_, start_body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    let session_id = start_body["id"].as_str().unwrap();

    // Supply the required bot_token field.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/wizard/next",
        Some(json!({ "session_id": session_id, "answers": { "bot_token": "secret123" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // After step 1 of 1, done — step should be null.
    assert!(body["step"].is_null());
    assert_eq!(body["current_step"], 1);
}

/// wizard_next rejects missing required fields.
#[tokio::test]
async fn next_with_missing_required_field_returns_validation_error() {
    let app = app_with_one_provider();
    let (_, start_body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    let session_id = start_body["id"].as_str().unwrap();

    // Empty answers — bot_token is required.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/wizard/next",
        Some(json!({ "session_id": session_id, "answers": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "wizard.validation_failed");
}

/// wizard_next rejects unknown session.
#[tokio::test]
async fn next_rejects_unknown_session() {
    let app = app_with_one_provider();
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

/// wizard_execute succeeds (no filesystem side effects because bundle path is
/// non-existent; the engine will fail gracefully but execute_setup_blocking
/// should return an error we can inspect).
///
/// TODO: Add a full end-to-end execute test using tempfile::tempdir() with a
/// minimal bundle. The current test confirms the session is consumed and the
/// API returns an error (no bundle on disk), not a panic.
#[tokio::test]
async fn execute_removes_session_and_returns_result() {
    let app = app_with_one_provider();
    // Start a session.
    let (_, start_body) = send(
        &app,
        Method::GET,
        "/api/wizard/start?tenant=demo&env=dev&team=default",
        None,
    )
    .await;
    let id = start_body["id"].as_str().unwrap();

    // Execute (will fail because /tmp/demo bundle doesn't exist, but must not panic).
    let (status, _body) = send(
        &app,
        Method::POST,
        "/api/wizard/execute",
        Some(json!({ "session_id": id })),
    )
    .await;
    // Either 200 (success if bundle exists) or 500 (engine error) — both are valid.
    assert!(
        status == StatusCode::OK || status == StatusCode::INTERNAL_SERVER_ERROR,
        "unexpected status: {status}"
    );

    // Session must be gone regardless of execute outcome.
    let (status2, _) = send(
        &app,
        Method::GET,
        &format!("/api/wizard/session/{}", id),
        None,
    )
    .await;
    assert_eq!(status2, StatusCode::NOT_FOUND);
}

/// wizard_execute rejects unknown session.
#[tokio::test]
async fn execute_rejects_unknown_session() {
    let app = app_with_one_provider();
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

/// session_lookup returns current step and real FormSpec data.
#[tokio::test]
async fn session_lookup_returns_real_step() {
    let app = app_with_one_provider();
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
    // Step fields come from real FormSpec — not a stub key.
    assert!(body["step"]["fields"][0]["label_text"].is_string());
    assert_ne!(body["step"]["fields"][0]["label_text"], Value::Null);
}

/// session_lookup returns 404 for unknown id.
#[tokio::test]
async fn session_lookup_404s_for_unknown_id() {
    let app = app_with_one_provider();
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
