//! Wizard endpoints: start / next / execute / session.
//!
//! Phase 1a uses a STUB wizard plan — real FormSpec integration lands in
//! Task 34 (cutover) when the new UI replaces the legacy one. Until then
//! every step comes from `stub_step()` with fixed i18n keys.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ui::api::error::ApiError;
use crate::ui::state::{
    AppState, FieldOption, FieldType, ScopeKey, WizardField, WizardSession,
    WizardSessionView, WizardStep, validate_scope,
};

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

const STUB_TOTAL_STEPS: u32 = 3;

/// Return the stub step for a given 1-based step number.
///
/// TODO(task-34): Replace with real FormSpec step lookup from the bundle.
fn stub_step(step: u32) -> WizardStep {
    WizardStep {
        title_key: format!("ui.wizard.stub.step{step}.title"),
        description_key: Some(format!("ui.wizard.stub.step{step}.desc")),
        fields: vec![WizardField {
            name: format!("field_{step}"),
            field_type: FieldType::Text,
            label_key: format!("ui.wizard.stub.step{step}.field_label"),
            help_key: None,
            placeholder_key: None,
            required: true,
            visible_if: None,
            options: Vec::<FieldOption>::new(),
            default_value: None,
        }],
    }
}

pub async fn wizard_start(
    State(state): State<Arc<AppState>>,
    Query(q): Query<StartQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = ScopeKey { tenant: q.tenant, env: q.env, team: q.team };
    validate_scope(&scope, &state.bundle)
        .map_err(|e| ApiError::validation(&e.code, &e.key))?;

    let session = WizardSession::new(scope.clone(), q.provider.clone(), STUB_TOTAL_STEPS);
    let id = session.id;

    let mut sessions = state.wizard_sessions.lock().map_err(|_| {
        ApiError::internal("wizard.lock_poisoned", "ui.error.internal")
    })?;
    sessions.insert(id, session);

    Ok(Json(WizardSessionView {
        id,
        scope,
        provider: q.provider,
        current_step: 1,
        total_steps: STUB_TOTAL_STEPS,
        step: Some(stub_step(1)),
        answers_so_far: json!({}),
    }))
}

pub async fn wizard_next(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<NextPayload>,
) -> Result<impl IntoResponse, ApiError> {
    let mut sessions = state.wizard_sessions.lock().map_err(|_| {
        ApiError::internal("wizard.lock_poisoned", "ui.error.internal")
    })?;

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

    // TODO(task-34): Validate answers against real FormSpec. For now we
    // accept any JSON object and merge it into the session answers.
    if let Value::Object(obj) = &payload.answers {
        for (k, v) in obj.iter() {
            session.answers.insert(k.clone(), v.to_string());
        }
    }

    session.current_step += 1;
    session.last_activity = std::time::Instant::now();

    let done = session.current_step > session.total_steps;
    let step = if done { None } else { Some(stub_step(session.current_step)) };
    let view = WizardSessionView {
        id: session.id,
        scope: session.scope.clone(),
        provider: session.provider.clone(),
        current_step: if done { session.total_steps } else { session.current_step },
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
    let mut sessions = state.wizard_sessions.lock().map_err(|_| {
        ApiError::internal("wizard.lock_poisoned", "ui.error.internal")
    })?;

    let session = sessions.remove(&payload.session_id).ok_or_else(|| {
        ApiError::not_found("wizard.session_not_found", "ui.error.session_not_found")
    })?;

    // TODO(task-34): Invoke the real SetupEngine to persist answers as
    // secrets + config, then refresh `state.bundle` via an ArcSwap.
    // The session drop at end-of-scope zeroizes the in-memory answers.
    drop(session);

    Ok(Json(json!({
        "success": true,
        "message_key": "ui.wizard.execute.success",
    })))
}

pub async fn wizard_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    let sessions = state.wizard_sessions.lock().map_err(|_| {
        ApiError::internal("wizard.lock_poisoned", "ui.error.internal")
    })?;

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
    let step = if done { None } else { Some(stub_step(session.current_step)) };
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
