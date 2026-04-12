//! Integration tests for `/api/secrets` CRUD endpoints.
//!
//! Security invariants tested:
//! - List responses never include raw values
//! - Reveal requires `confirmed: true`
//! - Rate limit (10/min) is enforced for reveal
//! - Edit/delete work correctly

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{delete, get, post, put};
use greentic_setup::ui::api::secrets::{
    delete_secret, get_secrets, post_reveal_secret, post_secret, put_secret,
};
use greentic_setup::ui::state::{AppState, BundleMeta, ProviderFormData};
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
use qa_spec::spec::question::QuestionPolicy;
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::tempdir;
use tower::ServiceExt;

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

fn make_bundle_in(dir: &std::path::Path) -> BundleMeta {
    let mut b = BundleMeta::test_fixture();
    b.path = dir.to_path_buf();
    b
}

fn state_with_bundle(bundle: BundleMeta) -> Arc<AppState> {
    AppState::test_with(
        bundle,
        12345,
        "tok",
        vec![ProviderFormData {
            provider_id: "messaging-telegram".to_string(),
            display_name: "Telegram".to_string(),
            form_spec: minimal_form_spec("messaging-telegram"),
        }],
    )
}

fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/secrets", get(get_secrets))
        .route("/api/secrets", put(put_secret))
        .route("/api/secrets", post(post_secret))
        .route("/api/secrets", delete(delete_secret))
        .route("/api/secrets/reveal", post(post_reveal_secret))
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
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn scope_query() -> &'static str {
    "/api/secrets?tenant=demo&env=dev&team=default"
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// GET /api/secrets returns a list (may be empty for fresh bundle).
#[tokio::test]
async fn list_returns_200_with_secrets_array() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state);
    let (status, body) = send(&app, Method::GET, scope_query(), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["secrets"].is_array(), "expected 'secrets' array");
}

/// GET /api/secrets never returns raw values (only masked_value field).
#[tokio::test]
async fn list_never_leaks_raw_values() {
    let dir = tempdir().unwrap();
    // First, write a secret.
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let router = app(state.clone());
    send(
        &router,
        Method::PUT,
        "/api/secrets",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token",
            "value": "super-secret-value"
        })),
    )
    .await;

    // Now list — the raw value must not appear anywhere.
    let router2 = app(state);
    let (status, body) = send(&router2, Method::GET, scope_query(), None).await;
    assert_eq!(status, StatusCode::OK);
    let body_str = serde_json::to_string(&body).unwrap();
    assert!(
        !body_str.contains("super-secret-value"),
        "raw secret value leaked in list response: {body_str}"
    );
}

/// GET /api/secrets rejects bad scope (invalid chars in tenant).
#[tokio::test]
async fn list_rejects_invalid_scope() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state);
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/secrets?tenant=evil!corp&env=dev&team=default",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["code"].as_str().unwrap().contains("scope"));
}

/// POST /api/secrets/reveal succeeds with confirmed=true.
#[tokio::test]
async fn reveal_succeeds_with_confirmed_true() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state.clone());

    // Write a secret first.
    send(
        &app,
        Method::PUT,
        "/api/secrets",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token",
            "value": "my-secret-token"
        })),
    )
    .await;

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/secrets/reveal",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token",
            "confirmed": true
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["value"], "my-secret-token");
}

/// POST /api/secrets/reveal fails without confirmed=true.
#[tokio::test]
async fn reveal_fails_without_confirmed_flag() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state);

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/secrets/reveal",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token"
            // confirmed omitted
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
}

/// POST /api/secrets/reveal fails when confirmed=false.
#[tokio::test]
async fn reveal_fails_when_confirmed_is_false() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state);

    let (status, _) = send(
        &app,
        Method::POST,
        "/api/secrets/reveal",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token",
            "confirmed": false
        })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// PUT /api/secrets writes and updates a secret.
#[tokio::test]
async fn edit_secret_succeeds() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state);

    let (status, body) = send(
        &app,
        Method::PUT,
        "/api/secrets",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token",
            "value": "new-value"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["key"], "bot_token");
}

/// DELETE /api/secrets removes a secret.
#[tokio::test]
async fn delete_secret_succeeds() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state.clone());

    // Write first.
    send(
        &app,
        Method::PUT,
        "/api/secrets",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token",
            "value": "to-delete"
        })),
    )
    .await;

    let (status, body) = send(
        &app,
        Method::DELETE,
        "/api/secrets",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], true);
}

/// POST /api/secrets (new/adhoc) writes a secret with any key.
#[tokio::test]
async fn add_adhoc_secret_succeeds() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state);

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/secrets",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "custom_adhoc_key",
            "value": "adhoc-value"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["success"], true);
}

/// Rate limit: after 10 reveals in the same minute window, the 11th is rejected.
#[tokio::test]
async fn reveal_rate_limit_enforced() {
    let dir = tempdir().unwrap();
    let state = state_with_bundle(make_bundle_in(dir.path()));
    let app = app(state.clone());

    // Write a secret so reveal has something to return.
    send(
        &app,
        Method::PUT,
        "/api/secrets",
        Some(json!({
            "tenant": "demo", "env": "dev", "team": "default",
            "provider_id": "messaging-telegram",
            "key": "bot_token",
            "value": "rate-limit-test"
        })),
    )
    .await;

    let reveal_body = json!({
        "tenant": "demo", "env": "dev", "team": "default",
        "provider_id": "messaging-telegram",
        "key": "bot_token",
        "confirmed": true
    });

    // Exhaust the quota (10 allowed).
    for _ in 0..10 {
        let (s, _) = send(
            &app,
            Method::POST,
            "/api/secrets/reveal",
            Some(reveal_body.clone()),
        )
        .await;
        // Each one should succeed or fail for non-rate-limit reasons.
        assert_ne!(s, StatusCode::TOO_MANY_REQUESTS, "rate limit hit too early");
    }

    // 11th should be rate-limited.
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/secrets/reveal",
        Some(reveal_body),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "expected 429 after 10 reveals, body: {body}"
    );
}
