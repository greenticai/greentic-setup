//! Router wiring + middleware layers for the dashboard web UI.

use axum::http::{HeaderName, HeaderValue};
use tower_http::set_header::SetResponseHeaderLayer;

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
