//! Security headers test — smoke-test the tower-http layer by running it
//! against a minimal Axum app and inspecting the response headers.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use greentic_setup::ui::routes::security_headers;
use tower::ServiceExt;

async fn hello() -> &'static str {
    "hello"
}

fn app() -> Router {
    let layers = security_headers();
    let mut app = Router::new().route("/", get(hello));
    for layer in layers {
        app = app.layer(layer);
    }
    app
}

async fn get_response() -> axum::response::Response {
    app()
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn sets_cache_control_no_store() {
    let resp = get_response().await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = resp.headers().get("cache-control").unwrap().to_str().unwrap();
    assert_eq!(v, "no-store, no-cache, must-revalidate");
}

#[tokio::test]
async fn sets_pragma_no_cache() {
    let resp = get_response().await;
    assert_eq!(resp.headers().get("pragma").unwrap().to_str().unwrap(), "no-cache");
}

#[tokio::test]
async fn sets_x_content_type_options_nosniff() {
    let resp = get_response().await;
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .unwrap()
            .to_str()
            .unwrap(),
        "nosniff"
    );
}

#[tokio::test]
async fn sets_x_frame_options_deny() {
    let resp = get_response().await;
    assert_eq!(
        resp.headers().get("x-frame-options").unwrap().to_str().unwrap(),
        "DENY"
    );
}

#[tokio::test]
async fn sets_referrer_policy_no_referrer() {
    let resp = get_response().await;
    assert_eq!(
        resp.headers().get("referrer-policy").unwrap().to_str().unwrap(),
        "no-referrer"
    );
}
