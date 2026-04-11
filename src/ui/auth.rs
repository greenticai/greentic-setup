//! Bearer token + Origin header authentication for /api/* routes.
//!
//! Even on 127.0.0.1 this guards against malicious local processes and
//! cross-origin attacks via a malicious site opened in the same browser.

use axum::http::HeaderMap;
use rand::Rng as _;

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    MissingBearer,
    InvalidBearer,
    InvalidOrigin,
}

/// Generate a 256-bit random bearer token encoded as base64-url (no padding).
///
/// Returns a 43-character string (32 bytes → base64-url ceil(32*4/3) chars, no padding).
pub fn generate_bearer_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// CSRF-safe custom header that the dashboard SPA attaches to every API
/// request. Cross-origin scripts cannot set custom headers without triggering
/// a CORS preflight, so presence of this header is a reliable indicator that
/// the request originated from the same-origin dashboard code.
const CSRF_HEADER: &str = "x-requested-with";
const CSRF_HEADER_VALUE: &str = "GreenticSetupDashboard";

/// Verify bearer token (constant-time compare) and cross-site request
/// protection via one of three signals: a CSRF custom header, `Origin`, or
/// `Referer`.
///
/// - Returns `MissingBearer` if the `Authorization` header is absent or
///   doesn't start with `Bearer `.
/// - Returns `InvalidBearer` if the token does not match (timing-safe).
/// - Returns `InvalidOrigin` if none of the CSRF signals vouch for the
///   request origin.
///
/// The order of checks is:
/// 1. `X-Requested-With: GreenticSetupDashboard` — most robust; custom
///    headers can only be set by same-origin scripts (cross-origin fetch
///    with a custom header triggers CORS preflight, which we never allow).
/// 2. `Origin` header — present on POST / cross-origin / fetch-with-cors-mode
///    requests. Must match `http://127.0.0.1:{port}` or `http://localhost:{port}`.
/// 3. `Referer` header — browsers omit `Origin` on many same-origin GET
///    fetches, so we fall back to `Referer` per the OWASP CSRF prevention
///    cheat sheet. A valid Referer must equal our expected origin or start
///    with `{origin}/`, `{origin}?`, or `{origin}#` so that
///    `http://127.0.0.1:PORT.evil.com` does not pass the check.
pub fn verify_auth(
    headers: &HeaderMap,
    expected_token: &str,
    expected_port: u16,
) -> Result<(), AuthError> {
    let provided = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        // Case-sensitive `Bearer ` prefix is intentional: this is a closed
        // local-only dashboard API, not a general-purpose HTTP service.
        // Strict matching avoids double-space / mixed-case bypass classes.
        .and_then(|s| s.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingBearer)?;

    if !constant_time_eq::constant_time_eq(provided.as_bytes(), expected_token.as_bytes()) {
        return Err(AuthError::InvalidBearer);
    }

    // Signal 1: custom header vouching for same-origin script. Reliable
    // regardless of browser referrer-policy or request mode.
    if headers
        .get(CSRF_HEADER)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| v == CSRF_HEADER_VALUE)
    {
        return Ok(());
    }

    let ok_127 = format!("http://127.0.0.1:{expected_port}");
    let ok_local = format!("http://localhost:{expected_port}");

    // Signal 2: `Origin` header (present on POST and cross-origin fetches).
    if let Some(origin) = headers.get("origin").and_then(|h| h.to_str().ok()) {
        if origin == ok_127 || origin == ok_local {
            return Ok(());
        }
        return Err(AuthError::InvalidOrigin);
    }

    // Signal 3: `Referer` fallback.
    if let Some(referer) = headers.get("referer").and_then(|h| h.to_str().ok()) {
        for base in [&ok_127, &ok_local] {
            if referer == *base
                || referer.starts_with(&format!("{base}/"))
                || referer.starts_with(&format!("{base}?"))
                || referer.starts_with(&format!("{base}#"))
            {
                return Ok(());
            }
        }
        return Err(AuthError::InvalidOrigin);
    }

    Err(AuthError::InvalidOrigin)
}
