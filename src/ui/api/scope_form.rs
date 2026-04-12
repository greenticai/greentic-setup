//! Unified scope form endpoints: GET/POST `/api/scope/form`.
//!
//! These endpoints replace the separate wizard and secrets views in the SPA.
//! A single GET returns every provider's FormSpec + current values pre-filled
//! from the secrets store. A single POST validates all answers and persists
//! them directly via `persist_qa_secrets`.
//!
//! Security invariants:
//! - GET returns raw current secret values because the user is editing them.
//!   This is acceptable because the endpoint is bearer-auth + origin-checked.
//!   Values are wrapped in `Zeroizing<String>` server-side and never logged.
//! - POST accepts plain values, validates per FormSpec, persists via
//!   `persist_qa_secrets`. `pending_mutations` is NOT set because this is a
//!   direct persist — no rebuild is needed.
//! - No raw values appear in any log entry. Audit entries record only
//!   identity fields (provider_id, key, scope).

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use greentic_secrets_lib::SecretsStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;
use zeroize::Zeroizing;

use crate::ui::api::error::{ApiError, FieldError};
use crate::ui::state::{AppState, ScopeKey, validate_scope};

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    pub tenant: String,
    pub env: String,
    pub team: String,
}

/// Response payload for `GET /api/scope/form`.
#[derive(Debug, Serialize)]
pub struct ScopeFormResponse {
    pub scope: ScopeKey,
    pub providers: Vec<ProviderFormEntry>,
}

/// Extended metadata for a single question, sourced from `assets/setup.yaml`.
///
/// These fields are not part of the `qa_spec::FormSpec` schema; they come from
/// the provider's legacy setup spec. Missing fields are omitted from JSON.
#[derive(Debug, Clone, Serialize, Default)]
pub struct QuestionExtras {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_if: Option<String>,
}

/// One provider's FormSpec + current values (raw, for editing).
#[derive(Debug, Serialize)]
pub struct ProviderFormEntry {
    pub id: String,
    pub display_name: String,
    pub form_spec: qa_spec::FormSpec,
    /// Current values from the secrets store for each question id.
    /// Only keys with an existing value are included (no empty placeholders).
    /// Values are raw strings — this is intentional for the edit form.
    pub current_values: HashMap<String, String>,
    /// Extra per-question metadata from `assets/setup.yaml` (placeholder,
    /// docs_url, group). Keyed by question id. Empty when setup.yaml is absent.
    pub question_extras: HashMap<String, QuestionExtras>,
}

/// Request body for `POST /api/scope/form`.
#[derive(Debug, Deserialize)]
pub struct PostScopeFormBody {
    pub scope: ScopeKey,
    /// Answers keyed by provider_id → { field_key: value }.
    pub by_provider: HashMap<String, Value>,
}

/// Success response for `POST /api/scope/form`.
#[derive(Debug, Serialize)]
pub struct PostScopeFormResponse {
    pub success: bool,
    pub providers_saved: usize,
}

// ── GET /api/scope/form ───────────────────────────────────────────────────────

/// Return the unified scope-form payload: all providers with their FormSpec
/// and current values pre-filled from the secrets store.
pub async fn get_scope_form(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ScopeQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = ScopeKey {
        tenant: q.tenant.clone(),
        env: q.env.clone(),
        team: q.team.clone(),
    };
    validate_scope(&scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;

    info!(
        action = "get_scope_form",
        tenant = %scope.tenant,
        env = %scope.env,
        team = %scope.team,
        providers = state.provider_forms.len(),
        "scope form requested"
    );

    // Open the secrets store once and read all relevant values.
    // We do this in a blocking task because the dev store does sync I/O under
    // the hood (even though the API is async).
    let bundle_path = state.bundle.path.clone();
    let provider_forms = state.provider_forms.clone();
    let scope_clone = scope.clone();

    let providers: Vec<ProviderFormEntry> =
        tokio::task::spawn_blocking(move || read_provider_form_entries(&bundle_path, &scope_clone, &provider_forms))
            .await
            .map_err(|e| {
                ApiError::internal("scope_form.task_panic", "ui.error.internal")
                    .with_params(json!({ "message": e.to_string() }))
            })?
            .map_err(|_| ApiError::internal("scope_form.read_failed", "ui.error.secrets_list_failed"))?;

    Ok(Json(ScopeFormResponse { scope, providers }))
}

/// Read FormSpec + current values for every provider (runs in blocking context).
fn read_provider_form_entries(
    bundle_path: &std::path::Path,
    scope: &ScopeKey,
    provider_forms: &[crate::ui::state::ProviderFormData],
) -> anyhow::Result<Vec<ProviderFormEntry>> {
    // Build a synchronous runtime for the async store reads.
    let rt = tokio::runtime::Handle::try_current()
        .ok()
        .map(|_| None::<tokio::runtime::Runtime>);

    // We're already inside spawn_blocking so we can create a mini rt to drive
    // the async store operations.
    let mini_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let _ = rt; // suppress warning

    let store = crate::secrets::open_dev_store(bundle_path)?;

    let mut entries = Vec::new();

    for pf in provider_forms {
        let mut current_values: HashMap<String, Zeroizing<String>> = HashMap::new();

        for q in &pf.form_spec.questions {
            let uri = crate::canonical_secret_uri(
                &scope.env,
                &scope.tenant,
                Some(scope.team.as_str()),
                &pf.provider_id,
                &q.id,
            );

            // Drive the async get() on the mini runtime.
            let bytes = mini_rt.block_on(store.get(&uri));
            if let Ok(b) = bytes
                && !b.is_empty()
                && let Ok(text) = String::from_utf8(b)
            {
                current_values.insert(q.id.clone(), Zeroizing::new(text));
            }
        }

        // Convert to plain HashMap<String, String> for JSON serialization.
        // The Zeroizing wrapper drops at end of this scope.
        let plain_values: HashMap<String, String> = current_values
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().to_owned()))
            .collect();

        // Load extended question metadata from setup.yaml inside the pack.
        // Failures are non-fatal — missing setup.yaml produces an empty map.
        let mut question_extras: HashMap<String, QuestionExtras> =
            match crate::setup_input::load_setup_spec(&pf.pack_path) {
                Ok(Some(spec)) => {
                    spec.questions
                        .into_iter()
                        .filter(|q| !q.name.is_empty())
                        .map(|q| {
                            let extras = QuestionExtras {
                                placeholder: q.placeholder,
                                docs_url: q.docs_url,
                                group: q.group,
                                visible_if: None,
                            };
                            (q.name, extras)
                        })
                        .collect()
                }
                _ => HashMap::new(),
            };

        // Merge visible_if expressions from FormSpec questions into question_extras.
        for q in &pf.form_spec.questions {
            let vis = q
                .visible_if
                .as_ref()
                .and_then(crate::ui::api::wizard_engine::expr_to_string);
            if vis.is_some() {
                question_extras
                    .entry(q.id.clone())
                    .or_default()
                    .visible_if = vis;
            }
        }

        entries.push(ProviderFormEntry {
            id: pf.provider_id.clone(),
            display_name: pf.display_name.clone(),
            form_spec: pf.form_spec.clone(),
            current_values: plain_values,
            question_extras,
        });

        // Explicitly drop to zeroize secrets from this provider before moving to next.
        drop(current_values);
    }

    Ok(entries)
}

// ── POST /api/scope/form ──────────────────────────────────────────────────────

/// Validate all provider answers against their FormSpecs and persist to the
/// secrets store via `persist_qa_secrets`.
pub async fn post_scope_form(
    State(state): State<Arc<AppState>>,
    Json(body): Json<PostScopeFormBody>,
) -> Result<impl IntoResponse, ApiError> {
    let scope = &body.scope;
    validate_scope(scope, &state.bundle).map_err(|e| ApiError::validation(&e.code, &e.key))?;

    info!(
        action = "post_scope_form",
        tenant = %scope.tenant,
        env = %scope.env,
        team = %scope.team,
        providers = body.by_provider.len(),
        "scope form save requested"
    );

    // Validate each provider's answers against FormSpec.
    let mut api_err = ApiError::validation("scope_form.validation_failed", "ui.error.validation_failed");
    let mut has_field_errors = false;

    for (provider_id, answers) in &body.by_provider {
        let answers_obj = if answers.is_object() {
            answers.clone()
        } else {
            json!({})
        };

        // Unknown provider_ids are silently skipped (no FormSpec to validate against).
        if let Some(form_data) = state
            .provider_forms
            .iter()
            .find(|pf| &pf.provider_id == provider_id)
            && let Err(msg) = crate::qa::wizard::validate_answers_against_form_spec(
                &form_data.form_spec,
                &answers_obj,
            )
        {
            // Prefix field errors with provider_id.field_key so the frontend
            // can distribute them to the correct provider section.
            let field_key = format!("{provider_id}.{msg}");
            api_err = api_err.with_field(&field_key, FieldError::new("ui.error.validation_failed"));
            has_field_errors = true;
        }
    }

    if has_field_errors {
        return Err(api_err);
    }

    // All valid — persist each provider's answers.
    let bundle_path = state.bundle.path.clone();
    let scope_clone = scope.clone();
    let provider_forms = state.provider_forms.clone();
    let by_provider = body.by_provider.clone();

    let count = tokio::task::spawn_blocking(move || {
        persist_all_providers_blocking(bundle_path, scope_clone, provider_forms, by_provider)
    })
    .await
    .map_err(|e| {
        ApiError::internal("scope_form.task_panic", "ui.error.internal")
            .with_params(json!({ "message": e.to_string() }))
    })?
    .map_err(|e| {
        ApiError::internal("scope_form.persist_failed", "ui.error.execute_failed")
            .with_params(json!({ "message": e }))
    })?;

    // This is a direct persist — no rebuild is needed, so we clear pending.
    state.pending_mutations.store(false, std::sync::atomic::Ordering::Relaxed);

    info!(
        action = "scope_form_saved",
        tenant = %scope.tenant,
        env = %scope.env,
        team = %scope.team,
        providers_saved = count,
        "scope form saved successfully"
    );

    Ok(Json(PostScopeFormResponse {
        success: true,
        providers_saved: count,
    }))
}

/// Persist answers for all providers (runs in blocking context).
fn persist_all_providers_blocking(
    bundle_path: std::path::PathBuf,
    scope: ScopeKey,
    provider_forms: Vec<crate::ui::state::ProviderFormData>,
    by_provider: HashMap<String, Value>,
) -> Result<usize, String> {
    let store = crate::secrets::open_dev_store(&bundle_path)
        .map_err(|e| format!("failed to open secrets store: {e}"))?;

    let mini_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to build runtime: {e}"))?;

    let mut saved_count = 0usize;

    for (provider_id, answers) in &by_provider {
        let answers_obj = if answers.is_object() {
            answers.clone()
        } else {
            json!({})
        };

        // Find the FormSpec for this provider (skip unknown providers).
        let Some(form_data) = provider_forms.iter().find(|pf| &pf.provider_id == provider_id)
        else {
            continue;
        };

        let saved = mini_rt
            .block_on(crate::qa::persist::persist_qa_secrets(
                &store,
                &scope.env,
                &scope.tenant,
                Some(scope.team.as_str()),
                provider_id,
                &answers_obj,
                &form_data.form_spec,
            ))
            .map_err(|e| format!("failed to persist secrets for {provider_id}: {e}"))?;

        saved_count += saved.len();
    }

    Ok(saved_count)
}
