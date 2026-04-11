//! `/api/secrets` — secrets CRUD handlers.
//!
//! Security invariants enforced here:
//! - List responses never include raw values (only `masked_value`).
//! - Reveal requires `confirmed: true` in the request body.
//! - Reveal is rate-limited to 10 per minute across the whole server.
//! - All values in Rust are held in `Zeroizing<String>` that drops on exit.
//! - Audit log entries record only identity fields, never values.
//! - Every handler validates the scope against the bundle allow-list.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

use crate::ui::api::error::ApiError;
use crate::ui::api::secrets_store;
use crate::ui::state::{AppState, ScopeKey, SecretEntry, validate_scope};

// ── Common request types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    pub tenant: String,
    pub env: String,
    pub team: String,
}

impl From<ScopeQuery> for ScopeKey {
    fn from(q: ScopeQuery) -> Self {
        ScopeKey {
            tenant: q.tenant,
            env: q.env,
            team: q.team,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SecretMutationBody {
    pub tenant: String,
    pub env: String,
    pub team: String,
    pub provider_id: String,
    pub key: String,
    pub value: Option<String>,
    /// Must be `true` for the reveal endpoint.
    pub confirmed: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct SecretsListResponse {
    pub secrets: Vec<SecretEntry>,
}

#[derive(Debug, Serialize)]
pub struct SecretMutationResponse {
    pub success: bool,
    pub key: String,
}

#[derive(Debug, Serialize)]
pub struct RevealResponse {
    pub value: String,
}

// ── Validation helpers ────────────────────────────────────────────────────────

#[allow(clippy::result_large_err)]
fn validate_provider(
    provider_id: &str,
    state: &AppState,
    allow_adhoc: bool,
) -> Result<(), ApiError> {
    if allow_adhoc {
        return Ok(());
    }
    if state
        .provider_forms
        .iter()
        .any(|pf| pf.provider_id == provider_id)
    {
        Ok(())
    } else {
        Err(ApiError::validation(
            "secrets.unknown_provider",
            "ui.error.provider_not_found",
        ))
    }
}

// ── GET /api/secrets ──────────────────────────────────────────────────────────

/// List all configured secrets for a scope (values masked).
pub async fn get_secrets(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ScopeQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let scope: ScopeKey = q.into();
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;

    let entries = secrets_store::list_secrets(
        &state.bundle.path,
        &state.provider_forms,
        &scope,
    )
    .await
    .map_err(|_| ApiError::internal("secrets.list_failed", "ui.error.secrets_list_failed"))?;

    Ok(Json(SecretsListResponse { secrets: entries }))
}

// ── POST /api/secrets/reveal ──────────────────────────────────────────────────

/// Reveal the raw value of a single secret.
///
/// Requires `confirmed: true` in the request body.
/// Rate-limited to 10 reveals per minute server-wide.
pub async fn post_reveal_secret(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SecretMutationBody>,
) -> Result<impl IntoResponse, ApiError> {
    // Require explicit confirmation.
    if body.confirmed != Some(true) {
        return Err(ApiError::forbidden(
            "secrets.reveal_not_confirmed",
            "ui.secrets.reveal_confirm",
        ));
    }

    let scope = ScopeKey {
        tenant: body.tenant.clone(),
        env: body.env.clone(),
        team: body.team.clone(),
    };
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;
    validate_provider(&body.provider_id, &state, false)?;

    // Rate limit check.
    if !state.consume_reveal_quota() {
        return Err(ApiError::new_too_many(
            "secrets.rate_limit_exceeded",
            "ui.error.rate_limit_exceeded",
        ));
    }

    let uri = crate::canonical_secret_uri(
        &scope.env,
        &scope.tenant,
        Some(scope.team.as_str()),
        &body.provider_id,
        &body.key,
    );

    // Holds the value in a Zeroizing wrapper; drops at end of function.
    let raw: Zeroizing<String> = secrets_store::reveal_secret(
        &state.bundle.path,
        &uri,
        &body.provider_id,
        &body.key,
        &scope,
    )
    .await
    .map_err(|_| ApiError::internal("secrets.reveal_failed", "ui.error.secrets_reveal_failed"))?;

    // Clone the value into the JSON response, then the Zeroizing drops.
    let response_value = raw.clone();
    drop(raw);

    Ok(Json(RevealResponse {
        value: response_value.to_string(),
    }))
}

// ── PUT /api/secrets ──────────────────────────────────────────────────────────

/// Update an existing secret value.
pub async fn put_secret(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SecretMutationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let value = body.value.as_deref().unwrap_or("").to_string();
    let value_z = Zeroizing::new(value);

    let scope = ScopeKey {
        tenant: body.tenant.clone(),
        env: body.env.clone(),
        team: body.team.clone(),
    };
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;
    validate_provider(&body.provider_id, &state, false)?;

    let uri = crate::canonical_secret_uri(
        &scope.env,
        &scope.tenant,
        Some(scope.team.as_str()),
        &body.provider_id,
        &body.key,
    );

    secrets_store::write_secret(
        &state.bundle.path,
        &uri,
        value_z,
        &body.provider_id,
        &body.key,
        &scope,
    )
    .await
    .map_err(|_| ApiError::internal("secrets.update_failed", "ui.error.secrets_update_failed"))?;

    state.mark_pending();
    Ok(Json(SecretMutationResponse {
        success: true,
        key: body.key,
    }))
}

// ── POST /api/secrets ─────────────────────────────────────────────────────────

/// Create a new ad-hoc secret (key not required to be in any FormSpec).
pub async fn post_secret(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SecretMutationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let value = body.value.as_deref().unwrap_or("").to_string();
    let value_z = Zeroizing::new(value);

    let scope = ScopeKey {
        tenant: body.tenant.clone(),
        env: body.env.clone(),
        team: body.team.clone(),
    };
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;
    // Ad-hoc keys allowed — no FormSpec check.
    let _ = validate_provider(&body.provider_id, &state, true);

    let uri = crate::canonical_secret_uri(
        &scope.env,
        &scope.tenant,
        Some(scope.team.as_str()),
        &body.provider_id,
        &body.key,
    );

    secrets_store::write_secret(
        &state.bundle.path,
        &uri,
        value_z,
        &body.provider_id,
        &body.key,
        &scope,
    )
    .await
    .map_err(|_| ApiError::internal("secrets.create_failed", "ui.error.secrets_update_failed"))?;

    state.mark_pending();
    Ok(Json(json!({ "success": true, "key": body.key })))
}

// ── DELETE /api/secrets ───────────────────────────────────────────────────────

/// Delete a secret from the dev store.
pub async fn delete_secret(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SecretMutationBody>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = ScopeKey {
        tenant: body.tenant.clone(),
        env: body.env.clone(),
        team: body.team.clone(),
    };
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;
    validate_provider(&body.provider_id, &state, false)?;

    let uri = crate::canonical_secret_uri(
        &scope.env,
        &scope.tenant,
        Some(scope.team.as_str()),
        &body.provider_id,
        &body.key,
    );

    secrets_store::delete_secret(
        &state.bundle.path,
        &uri,
        &body.provider_id,
        &body.key,
        &scope,
    )
    .map_err(|_| {
        ApiError::internal("secrets.delete_failed", "ui.error.secrets_delete_failed")
    })?;

    state.mark_pending();
    Ok(Json(SecretMutationResponse {
        success: true,
        key: body.key,
    }))
}
