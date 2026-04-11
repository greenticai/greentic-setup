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

/// Verify bearer token (constant-time compare) and Origin / Referer header.
///
/// - Returns `MissingBearer` if the `Authorization` header is absent or
///   doesn't start with `Bearer `.
/// - Returns `InvalidBearer` if the token does not match (timing-safe).
/// - Returns `InvalidOrigin` if neither a valid `Origin` nor `Referer`
///   header points at `http://127.0.0.1:{port}` or `http://localhost:{port}`.
///
/// Browsers do not always send `Origin` on same-origin GET fetches — in
/// that case we fall back to the `Referer` header per the OWASP CSRF
/// prevention cheat sheet. Requests with neither header are rejected.
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

    let ok_127 = format!("http://127.0.0.1:{expected_port}");
    let ok_local = format!("http://localhost:{expected_port}");

    // Prefer `Origin` (present on POST and cross-origin fetches).
    if let Some(origin) = headers.get("origin").and_then(|h| h.to_str().ok()) {
        if origin == ok_127 || origin == ok_local {
            return Ok(());
        }
        return Err(AuthError::InvalidOrigin);
    }

    // Fall back to `Referer` (browsers omit `Origin` on many same-origin
    // GETs). A valid Referer must start with our expected origin followed
    // by `/` or end-of-string so that `http://127.0.0.1:PORT.evil.com`
    // does not pass the check.
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
