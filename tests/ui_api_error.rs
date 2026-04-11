//! Tests for the ApiError JSON envelope.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use greentic_setup::ui::api::error::{ApiError, FieldError};
use serde_json::{json, Value};

async fn body_json(resp: axum::response::Response) -> Value {
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body read");
    serde_json::from_slice(&body).expect("json parse")
}

#[tokio::test]
async fn not_found_error_has_correct_shape() {
    let err = ApiError::not_found("bundle.not_found", "ui.error.bundle_not_found")
        .with_params(json!({ "path": "/tmp/missing" }));
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "bundle.not_found");
    assert_eq!(body["error"]["key"], "ui.error.bundle_not_found");
    assert_eq!(body["error"]["params"]["path"], "/tmp/missing");
    assert!(body["error"]["fields"].is_null() || !body["error"].as_object().unwrap().contains_key("fields"));
}

#[tokio::test]
async fn validation_error_has_fields() {
    let err = ApiError::validation("wizard.validation_failed", "ui.error.validation_failed")
        .with_field(
            "slack_token",
            FieldError::new("ui.error.invalid_token_format")
                .with_params(json!({ "pattern": "xoxb-*" })),
        );
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "wizard.validation_failed");
    assert_eq!(body["error"]["fields"]["slack_token"]["key"], "ui.error.invalid_token_format");
    assert_eq!(body["error"]["fields"]["slack_token"]["params"]["pattern"], "xoxb-*");
}

#[tokio::test]
async fn unauthorized_maps_to_401() {
    let err = ApiError::unauthorized("auth.missing_bearer", "ui.error.auth_missing");
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "auth.missing_bearer");
}

#[tokio::test]
async fn forbidden_maps_to_403() {
    let err = ApiError::forbidden("auth.bad_origin", "ui.error.auth_bad_origin");
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn conflict_maps_to_409() {
    let err = ApiError::conflict("scope.already_exists", "ui.error.scope_already_exists");
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn internal_maps_to_500() {
    let err = ApiError::internal("engine.plan_failed", "ui.error.internal");
    let resp = err.into_response();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn default_params_is_empty_object() {
    // When no params are set, the JSON must still have `params: {}` so the
    // client can always read `error.params.*` without a null check.
    let err = ApiError::not_found("bundle.not_found", "ui.error.bundle_not_found");
    let resp = err.into_response();
    let body = body_json(resp).await;
    assert_eq!(body["error"]["params"], json!({}));
}
