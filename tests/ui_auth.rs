//! Auth middleware tests — bearer token + Origin check.

use axum::http::{HeaderMap, HeaderValue};
use greentic_setup::ui::auth::{generate_bearer_token, verify_auth, AuthError};

const TOKEN: &str = "secret-token-abc-123";
const PORT: u16 = 52341;

fn headers(auth: Option<&str>, origin: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Some(a) = auth {
        h.insert("authorization", HeaderValue::from_str(a).unwrap());
    }
    if let Some(o) = origin {
        h.insert("origin", HeaderValue::from_str(o).unwrap());
    }
    h
}

#[test]
fn generate_bearer_token_is_exactly_43_chars() {
    // 32 random bytes → base64-url no-pad → exactly 43 characters.
    // Locking the length catches accidental encoding changes that would
    // silently reduce entropy or switch to padded / hex encoding.
    let t = generate_bearer_token();
    assert_eq!(t.len(), 43, "token wrong length: {}", t.len());
}

#[test]
fn generate_bearer_token_is_unique_per_call() {
    let a = generate_bearer_token();
    let b = generate_bearer_token();
    assert_ne!(a, b);
}

#[test]
fn verify_auth_rejects_missing_authorization_header() {
    let h = headers(None, Some("http://127.0.0.1:52341"));
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::MissingBearer));
}

#[test]
fn verify_auth_rejects_wrong_bearer_token() {
    let h = headers(Some("Bearer wrong-token"), Some("http://127.0.0.1:52341"));
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::InvalidBearer));
}

#[test]
fn verify_auth_accepts_correct_bearer_and_127_origin() {
    let h = headers(
        Some(&format!("Bearer {TOKEN}")),
        Some("http://127.0.0.1:52341"),
    );
    assert_eq!(verify_auth(&h, TOKEN, PORT), Ok(()));
}

#[test]
fn verify_auth_accepts_localhost_origin() {
    let h = headers(
        Some(&format!("Bearer {TOKEN}")),
        Some("http://localhost:52341"),
    );
    assert_eq!(verify_auth(&h, TOKEN, PORT), Ok(()));
}

#[test]
fn verify_auth_rejects_wrong_origin() {
    let h = headers(
        Some(&format!("Bearer {TOKEN}")),
        Some("http://evil.example.com"),
    );
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::InvalidOrigin));
}

#[test]
fn verify_auth_rejects_missing_origin() {
    let h = headers(Some(&format!("Bearer {TOKEN}")), None);
    assert_eq!(verify_auth(&h, TOKEN, PORT), Err(AuthError::InvalidOrigin));
}
