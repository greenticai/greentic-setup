//! Wizard endpoints: start / next / execute / session.
//!
//! Uses real FormSpec data loaded at startup into `AppState.provider_forms`.
//! Each wizard session tracks a `provider_sequence` (ordered provider IDs)
//! and collects `answers_by_provider` keyed by provider_id.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ui::api::error::ApiError;
use crate::ui::state::{
    AppState, ScopeKey, WizardSession, WizardSessionView, validate_scope,
};
use crate::ui::api::wizard_engine::{build_step_view, execute_setup_blocking, validate_provider_answers};

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub tenant: String,
    pub env: String,
    pub team: String,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NextPayload {
    pub session_id: Uuid,
    pub answers: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ExecutePayload {
    pub session_id: Uuid,
}

pub async fn wizard_start(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StartQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = ScopeKey {
        tenant: q.tenant.clone(),
        env: q.env.clone(),
        team: q.team.clone(),
    };
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;

    // Guard: no providers → conflict error so the UI can tell the user.
    if state.provider_forms.is_empty() {
        return Err(ApiError::conflict("wizard.no_providers", "ui.error.no_providers"));
    }

    let (provider_sequence, total_steps) = if let Some(ref pid) = q.provider {
        // Single-provider session.
        if !state.provider_forms.iter().any(|pf| &pf.provider_id == pid) {
            return Err(ApiError::not_found(
                "wizard.provider_not_found",
                "ui.error.provider_not_found",
            ));
        }
        (vec![pid.clone()], 1u32)
    } else {
        // All-providers session: one step per provider.
        let seq: Vec<String> = state
            .provider_forms
            .iter()
            .map(|pf| pf.provider_id.clone())
            .collect();
        let total = seq.len() as u32;
        (seq, total)
    };

    let mut session = WizardSession::new(scope.clone(), q.provider.clone(), total_steps);
    session.provider_sequence = provider_sequence;
    let id = session.id;

    // Build first step.
    let first_provider_id = &session.provider_sequence[0];
    let first_form = state
        .provider_forms
        .iter()
        .find(|pf| &pf.provider_id == first_provider_id)
        .expect("provider sequence validated above");
    let first_step = build_step_view(first_form, 1, total_steps);

    let mut sessions = state
        .wizard_sessions
        .lock()
        .map_err(|_| ApiError::internal("wizard.lock_poisoned", "ui.error.internal"))?;
    sessions.insert(id, session);

    Ok(Json(WizardSessionView {
        id,
        scope,
        provider: q.provider,
        current_step: 1,
        total_steps,
        step: Some(first_step),
        answers_so_far: json!({}),
    }))
}

pub async fn wizard_next(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<NextPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let mut sessions = state
        .wizard_sessions
        .lock()
        .map_err(|_| ApiError::internal("wizard.lock_poisoned", "ui.error.internal"))?;

    let session = sessions.get_mut(&payload.session_id).ok_or_else(|| {
        ApiError::not_found("wizard.session_not_found", "ui.error.session_not_found")
    })?;

    if session.is_expired() {
        sessions.remove(&payload.session_id);
        return Err(ApiError::not_found(
            "wizard.session_expired",
            "ui.error.session_expired",
        ));
    }

    // Determine which provider is being answered in this step.
    let step_idx = (session.current_step as usize).saturating_sub(1);
    let current_provider_id = session
        .provider_sequence
        .get(step_idx)
        .cloned()
        .unwrap_or_default();

    // Validate answers against the current provider's FormSpec.
    if let Some(form_data) = state
        .provider_forms
        .iter()
        .find(|pf| pf.provider_id == current_provider_id)
    {
        let answers_value = if payload.answers.is_object() {
            payload.answers.clone()
        } else {
            json!({})
        };

        if let Err(msg) = validate_provider_answers(&form_data.form_spec, &answers_value) {
            return Err(ApiError::validation(
                "wizard.validation_failed",
                "ui.error.validation_failed",
            )
            .with_params(json!({ "message": msg })));
        }

        // Store validated answers for this provider (secrets stay only in memory).
        session
            .answers_by_provider
            .insert(current_provider_id.clone(), answers_value.clone());

        // Also merge flat answers for session.answers (zeroized on drop).
        if let Value::Object(obj) = &answers_value {
            for (k, v) in obj.iter() {
                session.answers.insert(k.clone(), v.to_string());
            }
        }
    }

    session.current_step += 1;
    session.last_activity = std::time::Instant::now();

    let done = session.current_step > session.total_steps;

    let step = if done {
        None
    } else {
        let next_idx = (session.current_step as usize).saturating_sub(1);
        session
            .provider_sequence
            .get(next_idx)
            .and_then(|pid| state.provider_forms.iter().find(|pf| &pf.provider_id == pid))
            .map(|form_data| build_step_view(form_data, session.current_step, session.total_steps))
    };

    let view = WizardSessionView {
        id: session.id,
        scope: session.scope.clone(),
        provider: session.provider.clone(),
        current_step: if done {
            session.total_steps
        } else {
            session.current_step
        },
        total_steps: session.total_steps,
        step,
        answers_so_far: json!({}), // Secrets never leak back to client.
    };

    Ok(Json(view))
}

pub async fn wizard_execute(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ExecutePayload>,
) -> Result<impl IntoResponse, ApiError> {
    let session = {
        let mut sessions = state
            .wizard_sessions
            .lock()
            .map_err(|_| ApiError::internal("wizard.lock_poisoned", "ui.error.internal"))?;
        sessions
            .remove(&payload.session_id)
            .ok_or_else(|| {
                ApiError::not_found("wizard.session_not_found", "ui.error.session_not_found")
            })?
    };

    let bundle_path = state.bundle.path.clone();
    let scope = session.scope.clone();
    let answers_by_provider = session.answers_by_provider.clone();
    // session drops here, zeroizing in-memory secrets.

    let result = tokio::task::spawn_blocking(move || {
        execute_setup_blocking(bundle_path, scope, answers_by_provider)
    })
    .await
    .map_err(|e| {
        ApiError::internal("wizard.execute_panic", "ui.error.execute_failed")
            .with_params(json!({ "message": e.to_string() }))
    })?;

    match result {
        Ok(count) => Ok(Json(json!({
            "success": true,
            "message_key": "ui.wizard.execute.success",
            "providers_configured": count,
        }))),
        Err(msg) => Err(
            ApiError::internal("wizard.execute_failed", "ui.error.execute_failed")
                .with_params(json!({ "message": msg })),
        ),
    }
}

pub async fn wizard_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let sessions = state
        .wizard_sessions
        .lock()
        .map_err(|_| ApiError::internal("wizard.lock_poisoned", "ui.error.internal"))?;

    let session = sessions.get(&id).ok_or_else(|| {
        ApiError::not_found("wizard.session_not_found", "ui.error.session_not_found")
    })?;

    if session.is_expired() {
        return Err(ApiError::not_found(
            "wizard.session_expired",
            "ui.error.session_expired",
        ));
    }

    let done = session.current_step > session.total_steps;
    let step = if done {
        None
    } else {
        let idx = (session.current_step as usize).saturating_sub(1);
        session
            .provider_sequence
            .get(idx)
            .and_then(|pid| state.provider_forms.iter().find(|pf| &pf.provider_id == pid))
            .map(|form_data| build_step_view(form_data, session.current_step, session.total_steps))
    };

    Ok(Json(WizardSessionView {
        id: session.id,
        scope: session.scope.clone(),
        provider: session.provider.clone(),
        current_step: session.current_step,
        total_steps: session.total_steps,
        step,
        answers_so_far: json!({}),
    }))
}
