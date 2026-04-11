//! Router wiring + middleware layers for the dashboard web UI.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tower_http::set_header::SetResponseHeaderLayer;

use crate::ui::api::bundle::get_bundle;
use crate::ui::api::locale::{get_locale, post_shutdown};
use crate::ui::api::overview::get_overview;
use crate::ui::api::wizard::{wizard_execute, wizard_next, wizard_session, wizard_start};
use crate::ui::assets;
use crate::ui::auth::{AuthError, verify_auth};
use crate::ui::state::AppState;

/// Build the complete Axum router for the Phase 1a dashboard.
///
/// Routes:
/// - `/` → serves a placeholder HTML (Task 22 replaces with real index.html)
/// - `/api/bundle`, `/api/overview`, `/api/wizard/*`, `/api/locale/:code`,
///   `/api/shutdown` → JSON handlers, protected by auth middleware
/// - `/vendor/...`, `/styles/...`, `/js/...`, `/icons/...`,
///   `/components/...` → embedded static assets
///
/// All responses get the security headers from `security_headers()`.
pub fn build_router(state: Arc<AppState>) -> Router {
    // API sub-router with auth middleware.
    let api = Router::new()
        .route("/bundle", get(get_bundle))
        .route("/overview", get(get_overview))
        .route("/wizard/start", get(wizard_start))
        .route("/wizard/next", post(wizard_next))
        .route("/wizard/execute", post(wizard_execute))
        .route("/wizard/session/{id}", get(wizard_session))
        .route("/locale/{code}", get(get_locale))
        .route("/shutdown", post(post_shutdown))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    let mut app = Router::new()
        .nest("/api", api)
        .route("/", get(serve_index))
        .fallback(serve_static_asset)
        .with_state(state);

    // Apply security headers to everything.
    for layer in security_headers() {
        app = app.layer(layer);
    }
    app
}

/// Middleware: enforces bearer + origin on every `/api/*` request.
async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let headers = req.headers();
    match verify_auth(headers, &state.bearer_token, state.port) {
        Ok(()) => Ok(next.run(req).await),
        Err(AuthError::MissingBearer | AuthError::InvalidBearer) => Err(StatusCode::UNAUTHORIZED),
        Err(AuthError::InvalidOrigin) => Err(StatusCode::FORBIDDEN),
    }
}

/// Serve the Phase 1a SPA index.html with the real initial state injected.
///
/// Reads the embedded `index.html` at compile time and substitutes
/// `{{INITIAL_STATE}}` with a JSON object containing the bearer token, port,
/// and bundle metadata so the Alpine.js SPA can bootstrap without an extra
/// round-trip.
async fn serve_index(State(state): State<Arc<AppState>>) -> Response {
    const TEMPLATE: &str = include_str!("../../assets/setup-ui/index.html");
    let initial_state = serde_json::json!({
        "bearer_token": state.bearer_token.as_str(),
        "port": state.port,
        "bundle": {
            "id": state.bundle.id,
            "display_name": state.bundle.display_name,
            "available_tenants": state.bundle.available_tenants,
            "available_envs": state.bundle.available_envs,
            "available_teams": state.bundle.available_teams,
        },
        "locale": "en",
    });
    let html = TEMPLATE.replacen("{{INITIAL_STATE}}", &initial_state.to_string(), 1);
    (
        [(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("text/html; charset=utf-8"),
        )],
        html,
    )
        .into_response()
}

/// Fallback handler for static assets from `assets::ASSETS`.
///
/// Only accepts GET. Any other method returns 405.
async fn serve_static_asset(method: Method, uri: Uri) -> Response {
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    match assets::find(uri.path()) {
        Some(asset) => (
            [(
                HeaderName::from_static("content-type"),
                HeaderValue::from_str(asset.mime)
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            )],
            asset.body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Security headers helper from Task 11 ──

/// Build the fixed set of security headers applied to all responses.
///
/// Returns a list of tower-http layers that together set:
/// - `Cache-Control: no-store, no-cache, must-revalidate` — prevent caching
///   of sensitive UI pages or API responses
/// - `Pragma: no-cache` — legacy HTTP/1.0 cache hint
/// - `X-Content-Type-Options: nosniff` — block MIME sniffing attacks
/// - `X-Frame-Options: DENY` — disallow embedding in iframes
/// - `Referrer-Policy: no-referrer` — do not leak the dashboard URL to any
///   external site the user navigates to
///
/// Apply via `.layer(layers.0).layer(layers.1)...` on the Axum Router, or
/// collect into a `ServiceBuilder`.
pub fn security_headers() -> [SetResponseHeaderLayer<HeaderValue>; 5] {
    [
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static("cache-control"),
            HeaderValue::from_static("no-store, no-cache, must-revalidate"),
        ),
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static("pragma"),
            HeaderValue::from_static("no-cache"),
        ),
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ),
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-frame-options"),
            HeaderValue::from_static("DENY"),
        ),
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static("referrer-policy"),
            HeaderValue::from_static("no-referrer"),
        ),
    ]
}
