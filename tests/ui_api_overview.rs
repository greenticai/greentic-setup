//! Tests for the GET /api/overview endpoint.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use greentic_setup::ui::api::overview::get_overview;
use greentic_setup::ui::state::{
    AppState, BundleMeta, ProviderStatus, ScopeKey, ScopeStatus, ScopeSummary,
};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

fn fixture_bundle_with_scopes() -> BundleMeta {
    let mut bundle = BundleMeta::test_fixture();
    bundle.scopes = vec![
        ScopeSummary {
            scope: ScopeKey {
                tenant: "demo".into(),
                env: "dev".into(),
                team: "default".into(),
            },
            status: ScopeStatus::Configured,
            providers: vec![
                ProviderStatus {
                    id: "slack".into(),
                    display_name: "Slack".into(),
                    configured: true,
                    secrets_count: 2,
                    warnings: vec![],
                },
                ProviderStatus {
                    id: "telegram".into(),
                    display_name: "Telegram".into(),
                    configured: true,
                    secrets_count: 1,
                    warnings: vec![],
                },
            ],
            warnings: vec![],
        },
        ScopeSummary {
            scope: ScopeKey {
                tenant: "demo".into(),
                env: "prod".into(),
                team: "default".into(),
            },
            status: ScopeStatus::Partial,
            providers: vec![ProviderStatus {
                id: "slack".into(),
                display_name: "Slack".into(),
                configured: true,
                secrets_count: 1,
                warnings: vec![greentic_setup::ui::state::WarningMessage {
                    key: "ui.warn.missing_token".into(),
                    params: serde_json::json!({}),
                    severity: greentic_setup::ui::state::WarningSeverity::Warning,
                }],
            }],
            warnings: vec![],
        },
    ];
    bundle
}

fn app() -> Router {
    let state = Arc::new(AppState {
        bundle: fixture_bundle_with_scopes(),
        port: 12345,
        bearer_token: zeroize::Zeroizing::new("tok".to_string()),
        wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        shutdown_tx: tokio::sync::broadcast::channel::<()>(1).0,
    });
    Router::new()
        .route("/api/overview", get(get_overview))
        .with_state(state)
}

async fn get_body(uri: &str) -> (StatusCode, Value) {
    let resp = app()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
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
async fn returns_200_for_allowed_scope() {
    let (status, _) = get_body("/api/overview?tenant=demo&env=dev&team=default").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn response_includes_stats() {
    let (_, body) = get_body("/api/overview?tenant=demo&env=dev&team=default").await;
    // 2 scopes fixture, 3 total providers (2 + 1), 4 total secrets (2 + 1 + 1), 1 warning
    assert_eq!(body["stats"]["scopes_count"], 2);
    assert_eq!(body["stats"]["providers_count"], 3);
    assert_eq!(body["stats"]["secrets_count"], 4);
    assert_eq!(body["stats"]["warnings_count"], 1);
}

#[tokio::test]
async fn response_includes_scopes_list() {
    let (_, body) = get_body("/api/overview?tenant=demo&env=dev&team=default").await;
    assert!(body["scopes"].is_array());
    assert_eq!(body["scopes"].as_array().unwrap().len(), 2);
    assert_eq!(body["scopes"][0]["scope"]["tenant"], "demo");
    assert_eq!(body["scopes"][0]["status"], "configured");
    assert_eq!(body["scopes"][1]["status"], "partial");
}

#[tokio::test]
async fn response_echoes_requested_scope() {
    let (_, body) = get_body("/api/overview?tenant=demo&env=dev&team=default").await;
    assert_eq!(body["scope"]["tenant"], "demo");
    assert_eq!(body["scope"]["env"], "dev");
    assert_eq!(body["scope"]["team"], "default");
}

#[tokio::test]
async fn rejects_unknown_tenant_400() {
    let (status, body) = get_body("/api/overview?tenant=evil&env=dev&team=default").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "scope.invalid_tenant");
}

#[tokio::test]
async fn rejects_path_traversal_in_env_400() {
    let (status, body) = get_body("/api/overview?tenant=demo&env=../etc&team=default").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "scope.path_traversal");
}

#[tokio::test]
async fn rejects_missing_query_params_400() {
    let (status, _) = get_body("/api/overview").await;
    // Axum Query extractor returns 400 automatically when required fields missing
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
