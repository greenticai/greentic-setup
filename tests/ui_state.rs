//! Unit tests for ui::state DTOs and validation.

use greentic_setup::ui::state::{BundleMeta, ScopeKey, ScopeStatus, validate_scope};

#[test]
fn scope_key_serializes_with_snake_case() {
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "default".into(),
    };
    let json = serde_json::to_string(&scope).unwrap();
    assert_eq!(json, r#"{"tenant":"demo","env":"dev","team":"default"}"#);
}

#[test]
fn scope_status_serializes_snake_case() {
    let s = ScopeStatus::NotConfigured;
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json, r#""not_configured""#);
}

/// Any clean tenant/env/team triple passes validation regardless of whether
/// the values appear in the bundle's pre-existing allow-list.
#[test]
fn validate_scope_accepts_valid_chars() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "new-tenant".into(),
        env: "staging".into(),
        team: "team_alpha".into(),
    };
    assert!(validate_scope(&scope, &bundle).is_ok());
}

/// Strings that were already in the bundle fixture still pass.
#[test]
fn validate_scope_accepts_known_fixture_scope() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "default".into(),
    };
    assert!(validate_scope(&scope, &bundle).is_ok());
}

#[test]
fn validate_scope_rejects_empty_tenant() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "".into(),
        env: "dev".into(),
        team: "default".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.empty_tenant");
}

#[test]
fn validate_scope_rejects_empty_env() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "".into(),
        team: "default".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.empty_env");
}

#[test]
fn validate_scope_rejects_empty_team() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.empty_team");
}

#[test]
fn validate_scope_rejects_tenant_too_long() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "a".repeat(65),
        env: "dev".into(),
        team: "default".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.tenant_too_long");
}

#[test]
fn validate_scope_rejects_invalid_chars() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "acme!corp".into(),
        env: "dev".into(),
        team: "default".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.tenant_invalid_chars");
}

#[test]
fn validate_scope_rejects_path_traversal_in_env() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "../etc".into(),
        team: "default".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.path_traversal");
}

#[test]
fn validate_scope_rejects_slash_in_team() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "a/b".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    // Path traversal check fires before invalid-chars check.
    assert_eq!(err.code, "scope.path_traversal");
}
