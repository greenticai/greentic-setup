//! Integration tests for the unified scope-form endpoints.
//!
//! Tests cover:
//! - GET /api/scope/form — returns providers with FormSpec + current_values
//! - GET /api/scope/form — rejects invalid scope with 400
//! - POST /api/scope/form — validates required fields (missing → 400 with fields)
//! - POST /api/scope/form — persists valid answers to secrets store
//! - POST /api/scope/form — empty by_provider is a no-op (200 success)

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use greentic_setup::ui::api::scope_form::{get_scope_form, post_scope_form};
use greentic_setup::ui::state::{AppState, BundleMeta, ProviderFormData};
use qa_spec::spec::question::QuestionPolicy;
use qa_spec::{FormSpec, QuestionSpec, QuestionType};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

// ── Test fixtures ─────────────────────────────────────────────────────────────

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

fn state_empty() -> Arc<AppState> {
    AppState::test_with(BundleMeta::test_fixture(), 12345, "tok", vec![])
}

fn app_with_one_provider() -> Router {
    let state = state_with_one_provider();
    Router::new()
        .route("/api/scope/form", get(get_scope_form))
        .route("/api/scope/form", post(post_scope_form))
        .with_state(state)
}

fn app_empty() -> Router {
    let state = state_empty();
    Router::new()
        .route("/api/scope/form", get(get_scope_form))
        .route("/api/scope/form", post(post_scope_form))
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

/// GET /api/scope/form returns providers array with id, display_name,
/// form_spec, and current_values for each provider.
#[tokio::test]
async fn get_scope_form_returns_providers_with_form_spec() {
    let app = app_with_one_provider();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/scope/form?tenant=demo&env=dev&team=default",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "expected 200, got {status}: {body}");

    assert!(body["providers"].is_array(), "providers must be an array");
    let providers = body["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 1, "should have one provider");

    let p = &providers[0];
    assert_eq!(p["id"], "messaging-telegram");
    assert_eq!(p["display_name"], "Telegram");
    assert!(p["form_spec"].is_object(), "form_spec must be an object");
    assert!(
        p["form_spec"]["questions"].is_array(),
        "form_spec.questions must be an array"
    );
    assert!(
        p["current_values"].is_object(),
        "current_values must be an object"
    );

    // Scope echo is present.
    assert_eq!(body["scope"]["tenant"], "demo");
    assert_eq!(body["scope"]["env"], "dev");
    assert_eq!(body["scope"]["team"], "default");
}

/// GET /api/scope/form with empty providers list returns an empty providers array.
#[tokio::test]
async fn get_scope_form_with_no_providers_returns_empty_array() {
    let app = app_empty();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/scope/form?tenant=demo&env=dev&team=default",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let providers = body["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 0, "expected empty providers array");
}

/// GET /api/scope/form rejects an invalid scope with 400.
#[tokio::test]
async fn get_scope_form_rejects_invalid_scope() {
    let app = app_with_one_provider();
    let (status, body) = send(
        &app,
        Method::GET,
        "/api/scope/form?tenant=evil!corp&env=dev&team=default",
        None,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "scope.tenant_invalid_chars");
}

/// POST /api/scope/form with a missing required field returns 400 with
/// `fields` populated in the error envelope.
#[tokio::test]
async fn post_scope_form_validates_required_fields() {
    let app = app_with_one_provider();
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/scope/form",
        Some(json!({
            "scope": { "tenant": "demo", "env": "dev", "team": "default" },
            "by_provider": {
                "messaging-telegram": {}   // bot_token is required
            }
        })),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "expected 400, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"], "scope_form.validation_failed");
    // The fields map should be non-empty.
    assert!(
        body["error"]["fields"].is_object(),
        "expected fields map in error"
    );
}

/// POST /api/scope/form with valid answers persists them to the secrets store.
#[tokio::test]
async fn post_scope_form_with_valid_answers_persists() {
    let temp = tempfile::tempdir().expect("tempdir");
    let bundle_meta = {
        let mut m = BundleMeta::test_fixture();
        m.path = temp.path().to_path_buf();
        m
    };
    let state = AppState::test_with(
        bundle_meta,
        12345,
        "tok",
        vec![ProviderFormData {
            provider_id: "messaging-telegram".to_string(),
            display_name: "Telegram".to_string(),
            form_spec: minimal_form_spec("messaging-telegram"),
        }],
    );

    let app = Router::new()
        .route("/api/scope/form", get(get_scope_form))
        .route("/api/scope/form", post(post_scope_form))
        .with_state(state);

    let (status, body) = send(
        &app,
        Method::POST,
        "/api/scope/form",
        Some(json!({
            "scope": { "tenant": "demo", "env": "dev", "team": "default" },
            "by_provider": {
                "messaging-telegram": { "bot_token": "test-token-12345" }
            }
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "expected 200, got {status}: {body}");
    assert_eq!(body["success"], true);
    assert!(
        body["providers_saved"].as_u64().unwrap_or(0) > 0,
        "at least one key should be saved"
    );

    // Verify the secret was actually persisted.
    let store = greentic_setup::secrets::open_dev_store(temp.path()).expect("open dev store");
    let uri = greentic_setup::canonical_secret_uri(
        "dev",
        "demo",
        Some("default"),
        "messaging-telegram",
        "bot_token",
    );
    use greentic_secrets_lib::SecretsStore;
    let bytes = store.get(&uri).await.expect("get secret");
    let value = String::from_utf8(bytes).expect("utf8");
    assert_eq!(value, "test-token-12345");
}

/// POST /api/scope/form with an empty by_provider map is a no-op
/// (returns 200 success, providers_saved = 0).
#[tokio::test]
async fn post_scope_form_with_empty_provider_map_succeeds() {
    let app = app_with_one_provider();
    let (status, body) = send(
        &app,
        Method::POST,
        "/api/scope/form",
        Some(json!({
            "scope": { "tenant": "demo", "env": "dev", "team": "default" },
            "by_provider": {}
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "expected 200, got {status}: {body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["providers_saved"], 0);
}
