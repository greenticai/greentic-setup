//! Helper functions for the wizard API handlers.
//!
//! Extracted from wizard.rs to keep file sizes below the 500-line ceiling.
//! Contains FormSpec → WizardStep conversion and SetupEngine invocation.

use qa_spec::{FormSpec, QuestionType};
use serde_json::{Map, Value};

use crate::ui::state::{FieldOption, FieldType, ProviderFormData, WizardField, WizardStep};

// ── FormSpec → WizardStep conversion ────────────────────────────────────────

/// Map a `qa_spec::QuestionType` (and `secret` flag) to a dashboard `FieldType`.
fn question_type_to_field_type(kind: QuestionType, secret: bool) -> FieldType {
    if secret {
        return FieldType::Password;
    }
    match kind {
        QuestionType::Boolean => FieldType::Switch,
        QuestionType::Enum => FieldType::Select,
        QuestionType::String | QuestionType::Integer | QuestionType::Number | QuestionType::List => {
            FieldType::Text
        }
    }
}

/// Serialize a `qa_spec::Expr` to a compact JSON string for use in `visible_if`.
///
/// Returns `None` for expression types that cannot be meaningfully represented
/// as a simple string (the SPA does not evaluate complex expressions; only simple
/// field-equality conditions are passed through).
pub(crate) fn expr_to_string(expr: &qa_spec::Expr) -> Option<String> {
    match expr {
        qa_spec::Expr::Answer { path } => Some(path.clone()),
        qa_spec::Expr::Eq { left, right } => {
            let field = match left.as_ref() {
                qa_spec::Expr::Answer { path } => path.clone(),
                _ => return None,
            };
            let val = match right.as_ref() {
                qa_spec::Expr::Literal { value } => {
                    value.as_str().unwrap_or("true").to_string()
                }
                qa_spec::Expr::Answer { path } => path.clone(),
                _ => return None,
            };
            Some(format!("{field}=={val}"))
        }
        _ => None,
    }
}

/// Convert a single `FormSpec` question to a `WizardField`.
fn question_to_field(q: &qa_spec::QuestionSpec) -> WizardField {
    let field_type = question_type_to_field_type(q.kind, q.secret);

    let options: Vec<FieldOption> = if q.kind == QuestionType::Enum {
        q.choices
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .map(|c| FieldOption {
                value: c.clone(),
                // Use value as label_key fallback; the SPA will display value if key not found.
                label_key: c.clone(),
            })
            .collect()
    } else {
        vec![]
    };

    let visible_if = q.visible_if.as_ref().and_then(expr_to_string);

    let default_value = q
        .default_value
        .as_deref()
        .map(|s| Value::String(s.to_string()));

    WizardField {
        name: q.id.clone(),
        field_type,
        // Use a stable i18n key slot; the SPA will fall back to label_text if not found.
        label_key: format!("ui.q.{}", q.id),
        label_text: Some(q.title.clone()),
        help_key: q.description.as_ref().map(|_| format!("ui.q.{}.help", q.id)),
        placeholder_key: None,
        required: q.required,
        visible_if,
        options,
        default_value,
    }
}

/// Build a `WizardStep` from a `ProviderFormData` entry for a given step position.
pub fn build_step_view(
    form_data: &ProviderFormData,
    step_number: u32,
    total_steps: u32,
) -> WizardStep {
    let _ = (step_number, total_steps); // step numbers are for reference only

    let fields = form_data
        .form_spec
        .questions
        .iter()
        .map(question_to_field)
        .collect();

    WizardStep {
        title_key: format!("ui.wizard.provider.{}.title", form_data.provider_id),
        description_key: Some(format!(
            "ui.wizard.provider.{}.desc",
            form_data.provider_id
        )),
        fields,
    }
}

// ── SetupEngine execution ────────────────────────────────────────────────────

/// Execute setup for the given scope and answers map (keyed by provider_id).
///
/// Called from `spawn_blocking` — this function does synchronous filesystem I/O.
pub fn execute_setup_blocking(
    bundle_path: std::path::PathBuf,
    scope: crate::ui::state::ScopeKey,
    answers_by_provider: std::collections::HashMap<String, Value>,
) -> Result<u32, String> {
    use crate::engine::{SetupConfig, SetupRequest};
    use crate::plan::TenantSelection;
    use crate::platform_setup::StaticRoutesPolicy;
    use crate::{SetupEngine, SetupMode};

    let config = SetupConfig {
        tenant: scope.tenant.clone(),
        team: Some(scope.team.clone()),
        env: scope.env.clone(),
        offline: false,
        verbose: false,
    };

    let static_routes = StaticRoutesPolicy::normalize(None, &scope.env)
        .map_err(|e| format!("failed to normalize static routes: {e}"))?;

    // Merge all provider answers into a single map for SetupRequest.
    let mut setup_answers: Map<String, Value> = Map::new();
    for (provider_id, answers) in &answers_by_provider {
        setup_answers.insert(provider_id.clone(), answers.clone());
    }
    let providers_configured = answers_by_provider.len() as u32;

    let request = SetupRequest {
        bundle: bundle_path,
        tenants: vec![TenantSelection {
            tenant: scope.tenant.clone(),
            team: Some(scope.team.clone()),
            allow_paths: Vec::new(),
        }],
        static_routes,
        setup_answers,
        ..Default::default()
    };

    let engine = SetupEngine::new(config);

    let plan = engine
        .plan(SetupMode::Create, &request, false)
        .map_err(|e| format!("failed to build plan: {e}"))?;

    engine
        .execute(&plan)
        .map_err(|e| format!("execution failed: {e}"))?;

    Ok(providers_configured)
}

// ── Validation ───────────────────────────────────────────────────────────────

/// Validate answers for a single provider's `FormSpec`.
///
/// Returns `Ok(())` on success or `Err(message)` describing the first
/// validation failure.
pub fn validate_provider_answers(form_spec: &FormSpec, answers: &Value) -> Result<(), String> {
    crate::qa::wizard::validate_answers_against_form_spec(form_spec, answers)
        .map_err(|e| e.to_string())
}
