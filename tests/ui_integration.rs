//! End-to-end integration test: drive the full Phase 1a dashboard router
//! through a realistic flow using the oneshot test harness.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use greentic_setup::ui::routes::build_router;
use greentic_setup::ui::state::{AppState, BundleMeta};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "integration-test-token";

fn state() -> Arc<AppState> {
    Arc::new(AppState {
        bundle: BundleMeta::test_fixture(),
        port: 52341,
        bearer_token: zeroize::Zeroizing::new(TOKEN.to_string()),
        wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        shutdown_tx: tokio::sync::broadcast::channel::<()>(1).0,
    })
}

fn authed_request(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {TOKEN}"))
        .header("origin", "http://127.0.0.1:52341");
    let body = if let Some(b) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&b).unwrap())
    } else {
        Body::empty()
    };
    builder.body(body).unwrap()
}

async fn send(router: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn full_flow_bundle_overview_wizard_execute() {
    let app = build_router(state());

    // Step 1: Fetch bundle metadata.
    let (s1, bundle_body) = send(&app, authed_request(Method::GET, "/api/bundle", None)).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(bundle_body["id"], "demo");
    let tenant = bundle_body["available_tenants"][0]
        .as_str()
        .unwrap()
        .to_string();
    let env = bundle_body["available_envs"][0]
        .as_str()
        .unwrap()
        .to_string();
    let team = bundle_body["available_teams"][0]
        .as_str()
        .unwrap()
        .to_string();

    // Step 2: Fetch overview.
    let uri = format!("/api/overview?tenant={tenant}&env={env}&team={team}");
    let (s2, ov_body) = send(&app, authed_request(Method::GET, &uri, None)).await;
    assert_eq!(s2, StatusCode::OK);
    assert!(ov_body["stats"].is_object());

    // Step 3: Start a wizard.
    let wuri = format!("/api/wizard/start?tenant={tenant}&env={env}&team={team}");
    let (s3, w1) = send(&app, authed_request(Method::GET, &wuri, None)).await;
    assert_eq!(s3, StatusCode::OK);
    assert_eq!(w1["current_step"], 1);
    let session_id = w1["id"].as_str().unwrap().to_string();

    // Step 4: Advance all 3 steps.
    for expected in 2..=3 {
        let (s, body) = send(
            &app,
            authed_request(
                Method::POST,
                "/api/wizard/next",
                Some(json!({ "session_id": session_id, "answers": {} })),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["current_step"], expected);
    }

    // Step 5: Execute (finalize).
    let (s5, exec_body) = send(
        &app,
        authed_request(
            Method::POST,
            "/api/wizard/execute",
            Some(json!({ "session_id": session_id })),
        ),
    )
    .await;
    assert_eq!(s5, StatusCode::OK);
    assert_eq!(exec_body["success"], true);

    // Step 6: Session should be gone now.
    let (s6, _) = send(
        &app,
        authed_request(
            Method::GET,
            &format!("/api/wizard/session/{session_id}"),
            None,
        ),
    )
    .await;
    assert_eq!(s6, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn locale_endpoint_serves_catalog_for_english() {
    let app = build_router(state());
    let (s, body) = send(&app, authed_request(Method::GET, "/api/locale/en", None)).await;
    assert_eq!(s, StatusCode::OK);
    // Must contain at least the Phase 1a ui.* keys.
    assert!(body["ui.overview.welcome_title"].is_string());
    assert!(body["ui.brand.name"].is_string());
}

#[tokio::test]
async fn shutdown_endpoint_triggers_broadcast() {
    let state = state();
    let mut rx = state.shutdown_tx.subscribe();
    let app = build_router(state.clone());
    let (s, body) = send(&app, authed_request(Method::POST, "/api/shutdown", None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["shutdown"], true);
    let recv = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
    assert!(recv.is_ok(), "shutdown signal not received");
}

#[tokio::test]
async fn index_page_uses_friendly_brand_name_not_hyphenated() {
    let app = build_router(state());
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    // The inline placeholder index in routes.rs uses the friendly name
    // directly. The full SPA index.html uses i18n keys and will not contain
    // the literal "Greentic Setup" text — but it does contain the t('ui.brand.name')
    // call, which our integration test pass because routes.rs currently serves
    // its inline placeholder. This test guards against regressing to `greentic-setup`
    // hyphenated literal.
    assert!(
        !html.contains("greentic-setup"),
        "hyphenated name must not appear in UI copy"
    );
}
