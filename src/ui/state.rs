//! Shared application state and DTOs for the dashboard UI.
//!
//! These types are the wire format between the Axum handlers and the Alpine
//! SPA. All visible strings must be i18n keys, never raw English.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Scope triple identifying one `(tenant, env, team)` configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    pub tenant: String,
    pub env: String,
    pub team: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScopeStatus {
    Configured,
    Partial,
    NotConfigured,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct WarningMessage {
    pub key: String,
    pub params: serde_json::Value,
    pub severity: WarningSeverity,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub configured: bool,
    pub secrets_count: u32,
    pub warnings: Vec<WarningMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopeSummary {
    pub scope: ScopeKey,
    pub status: ScopeStatus,
    pub providers: Vec<ProviderStatus>,
    pub warnings: Vec<WarningMessage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderRef {
    pub oci: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleMeta {
    pub id: String,
    pub display_name: String,
    pub path: PathBuf,
    pub scopes: Vec<ScopeSummary>,
    pub available_tenants: Vec<String>,
    pub available_envs: Vec<String>,
    pub available_teams: Vec<String>,
    pub extension_providers: Vec<ProviderRef>,
}

#[cfg(any(test, feature = "test-helpers"))]
impl BundleMeta {
    /// Fixture used by unit tests — do not use in production code.
    pub fn test_fixture() -> Self {
        Self {
            id: "demo".into(),
            display_name: "Demo Bundle".into(),
            path: PathBuf::from("/tmp/demo"),
            scopes: vec![],
            available_tenants: vec!["demo".into(), "acme-corp".into()],
            available_envs: vec!["dev".into(), "prod".into()],
            available_teams: vec!["default".into()],
            extension_providers: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OverviewStats {
    pub scopes_count: u32,
    pub providers_count: u32,
    pub secrets_count: u32,
    pub warnings_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverviewResponse {
    pub scope: ScopeKey,
    pub stats: OverviewStats,
    pub scopes: Vec<ScopeSummary>,
}

/// Validation error shape used by scope validation and API handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub code: String,
    pub key: String,
}

impl ValidationError {
    pub fn new(code: &str, key: &str) -> Self {
        Self {
            code: code.into(),
            key: key.into(),
        }
    }
}

/// Validate a `ScopeKey` against the bundle's allow-list.
///
/// Rejects unknown tenant/env/team names and any component containing
/// path-traversal characters (`..`, `/`, `\`).
pub fn validate_scope(scope: &ScopeKey, bundle: &BundleMeta) -> Result<(), ValidationError> {
    // Path traversal check runs first so malicious inputs that happen to match
    // the allow-list (e.g. a team literally named "a/b") are still rejected.
    for part in [&scope.tenant, &scope.env, &scope.team] {
        if part.contains("..") || part.contains('/') || part.contains('\\') {
            return Err(ValidationError::new(
                "scope.path_traversal",
                "ui.error.scope_invalid",
            ));
        }
    }
    if !bundle.available_tenants.iter().any(|t| t == &scope.tenant) {
        return Err(ValidationError::new(
            "scope.invalid_tenant",
            "ui.error.invalid_tenant",
        ));
    }
    if !bundle.available_envs.iter().any(|e| e == &scope.env) {
        return Err(ValidationError::new(
            "scope.invalid_env",
            "ui.error.invalid_env",
        ));
    }
    if !bundle.available_teams.iter().any(|t| t == &scope.team) {
        return Err(ValidationError::new(
            "scope.invalid_team",
            "ui.error.invalid_team",
        ));
    }
    Ok(())
}

/// A `HashMap<String, String>` that zeroizes all values on drop.
///
/// `zeroize::Zeroizing` requires its inner type to implement `Zeroize`, which
/// `HashMap` does not. We wrap it and implement `Zeroize` manually so that
/// wizard answers (which may contain secrets) are scrubbed from memory.
#[derive(Debug, Default)]
pub struct ZeroizingAnswers(pub std::collections::HashMap<String, String>);

impl std::ops::Deref for ZeroizingAnswers {
    type Target = std::collections::HashMap<String, String>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ZeroizingAnswers {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl zeroize::Zeroize for ZeroizingAnswers {
    fn zeroize(&mut self) {
        for v in self.0.values_mut() {
            v.zeroize();
        }
        self.0.clear();
    }
}

impl Drop for ZeroizingAnswers {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(self);
    }
}

/// Top-level app state shared across Axum handlers.
#[derive(Debug)]
pub struct AppState {
    pub bundle: BundleMeta,
    pub port: u16,
    pub bearer_token: zeroize::Zeroizing<String>,
    pub wizard_sessions: std::sync::Mutex<std::collections::HashMap<Uuid, WizardSession>>,
    pub shutdown_tx: tokio::sync::broadcast::Sender<()>,
    pub launch_options: crate::ui::server::LaunchOptions,
}

impl AppState {
    /// Whether the first view shown to the user should be the wizard.
    ///
    /// Returns true when no scopes are configured yet OR when the user
    /// provided a `--answers` file (in which case they already know what
    /// they want and we should jump straight into the form).
    pub fn should_start_in_wizard(&self) -> bool {
        self.bundle.scopes.is_empty() || self.launch_options.prefill_answers.is_some()
    }
}

#[derive(Debug)]
pub struct WizardSession {
    pub id: Uuid,
    pub scope: ScopeKey,
    pub provider: Option<String>,
    pub current_step: u32,
    pub total_steps: u32,
    pub created_at: std::time::Instant,
    pub last_activity: std::time::Instant,
    pub answers: ZeroizingAnswers,
}

impl WizardSession {
    pub const TTL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

    pub fn new(scope: ScopeKey, provider: Option<String>, total_steps: u32) -> Self {
        let now = std::time::Instant::now();
        Self {
            id: Uuid::new_v4(),
            scope,
            provider,
            current_step: 1,
            total_steps,
            created_at: now,
            last_activity: now,
            answers: ZeroizingAnswers::default(),
        }
    }

    pub fn is_expired(&self) -> bool {
        self.last_activity.elapsed() > Self::TTL
    }
}

/// Wizard step rendered to the SPA. All labels are i18n keys.
#[derive(Debug, Clone, Serialize)]
pub struct WizardStep {
    pub title_key: String,
    pub description_key: Option<String>,
    pub fields: Vec<WizardField>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WizardField {
    pub name: String,
    pub field_type: FieldType,
    pub label_key: String,
    pub help_key: Option<String>,
    pub placeholder_key: Option<String>,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_if: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<FieldOption>,
    pub default_value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Password,
    Select,
    Switch,
    Textarea,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldOption {
    pub value: String,
    pub label_key: String,
}

/// View DTO for `GET /api/wizard/session/:id`.
#[derive(Debug, Clone, Serialize)]
pub struct WizardSessionView {
    pub id: uuid::Uuid,
    pub scope: ScopeKey,
    pub provider: Option<String>,
    pub current_step: u32,
    pub total_steps: u32,
    pub step: Option<WizardStep>, // None if done
    pub answers_so_far: serde_json::Value,
}
