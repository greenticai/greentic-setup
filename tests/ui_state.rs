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

#[test]
fn validate_scope_accepts_allowed_tenant() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "default".into(),
    };
    assert!(validate_scope(&scope, &bundle).is_ok());
}

#[test]
fn validate_scope_rejects_unknown_tenant() {
    let bundle = BundleMeta::test_fixture();
    let scope = ScopeKey {
        tenant: "evil".into(),
        env: "dev".into(),
        team: "default".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.invalid_tenant");
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
    // Add "a/b" to the allow-list so the path-traversal check runs, not the unknown-team check.
    let mut bundle = BundleMeta::test_fixture();
    bundle.available_teams.push("a/b".into());
    let scope = ScopeKey {
        tenant: "demo".into(),
        env: "dev".into(),
        team: "a/b".into(),
    };
    let err = validate_scope(&scope, &bundle).unwrap_err();
    assert_eq!(err.code, "scope.path_traversal");
}
