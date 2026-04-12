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

/// Validate a `ScopeKey` for well-formedness.
///
/// Enforces:
/// - Non-empty tenant/env/team.
/// - Max 64 characters per component.
/// - Path-traversal rejection (`..`, `/`, `\`) — security-critical.
/// - Only ASCII alphanumerics, hyphens, and underscores (keeps URIs clean).
///
/// The allow-list check has been removed: scope components no longer need to
/// match the bundle's pre-existing tenant/env/team lists, enabling users to
/// create new scopes directly from the UI.
pub fn validate_scope(scope: &ScopeKey, _bundle: &BundleMeta) -> Result<(), ValidationError> {
    for (part, name) in [
        (&scope.tenant, "tenant"),
        (&scope.env, "env"),
        (&scope.team, "team"),
    ] {
        if part.is_empty() {
            return Err(ValidationError::new(
                &format!("scope.empty_{name}"),
                &format!("ui.error.scope_{name}_empty"),
            ));
        }
        if part.len() > 64 {
            return Err(ValidationError::new(
                &format!("scope.{name}_too_long"),
                "ui.error.scope_too_long",
            ));
        }
        // Path traversal check — security-critical, always enforced.
        if part.contains("..") || part.contains('/') || part.contains('\\') {
            return Err(ValidationError::new(
                "scope.path_traversal",
                "ui.error.scope_invalid",
            ));
        }
        // Only allow alphanumeric + hyphen + underscore to keep URIs clean.
        if !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(ValidationError::new(
                &format!("scope.{name}_invalid_chars"),
                "ui.error.scope_invalid",
            ));
        }
    }
    Ok(())
}

/// Test-only constructor helper to build an `AppState` without the new atomic
/// fields requiring explicit initialization in every test.
#[cfg(any(test, feature = "test-helpers"))]
impl AppState {
    /// Build a minimal `AppState` suitable for unit tests.
    ///
    /// Sets `pending_mutations`, `reveal_count`, `reveal_window_start` to
    /// safe zero values. Production code must initialize them explicitly via
    /// `Arc::new(AppState { ..., pending_mutations: AtomicBool::new(false), ... })`.
    pub fn test_with(
        bundle: BundleMeta,
        port: u16,
        bearer_token: &str,
        provider_forms: Vec<ProviderFormData>,
    ) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            bundle,
            port,
            bearer_token: zeroize::Zeroizing::new(bearer_token.to_string()),
            wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
            shutdown_tx: tokio::sync::broadcast::channel::<()>(1).0,
            launch_options: Default::default(),
            provider_forms,
            pending_mutations: std::sync::atomic::AtomicBool::new(false),
            reveal_count: std::sync::atomic::AtomicU32::new(0),
            reveal_window_start: std::sync::atomic::AtomicU64::new(0),
        })
    }
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

/// Provider form data loaded from a discovered pack.
#[derive(Debug, Clone)]
pub struct ProviderFormData {
    pub provider_id: String,
    pub display_name: String,
    pub form_spec: qa_spec::FormSpec,
    /// Path to the `.gtpack` file, used to load extended question metadata
    /// (placeholder, docs_url, group) from `assets/setup.yaml`.
    pub pack_path: std::path::PathBuf,
}

/// A single secret entry returned by `GET /api/secrets`.
///
/// Values are always masked — raw secret data is never included in list responses.
#[derive(Debug, Clone, Serialize)]
pub struct SecretEntry {
    pub provider_id: String,
    pub key: String,
    pub uri: String,
    pub masked_value: String,
    pub has_value: bool,
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
    /// FormSpecs loaded from discovered provider packs at startup.
    pub provider_forms: Vec<ProviderFormData>,
    /// Set to `true` when any mutation (secrets/providers/capabilities) occurs
    /// since the last successful rebuild. Cleared on `POST /api/rebuild`.
    pub pending_mutations: std::sync::atomic::AtomicBool,
    /// Rate-limit counter for the reveal endpoint (requests per minute window).
    pub reveal_count: std::sync::atomic::AtomicU32,
    /// Timestamp of the start of the current rate-limit window (Unix seconds).
    pub reveal_window_start: std::sync::atomic::AtomicU64,
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

    /// Mark that a mutation has occurred and a rebuild is pending.
    pub fn mark_pending(&self) {
        self.pending_mutations
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Clear the pending-mutations flag after a successful rebuild.
    pub fn clear_pending(&self) {
        self.pending_mutations
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Check whether any mutations are pending.
    pub fn is_pending(&self) -> bool {
        self.pending_mutations
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Attempt to consume a reveal quota slot (max 10 per minute).
    ///
    /// Returns `true` if the request is within rate limits, `false` if it
    /// should be rejected. Thread-safe via atomics.
    pub fn consume_reveal_quota(&self) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let window_start = self.reveal_window_start.load(Relaxed);
        // If we're in a new minute window, reset counter.
        if now_secs.saturating_sub(window_start) >= 60 {
            self.reveal_window_start.store(now_secs, Relaxed);
            self.reveal_count.store(1, Relaxed);
            return true;
        }
        let prev = self.reveal_count.fetch_add(1, Relaxed);
        prev < 10
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
    /// Ordered list of provider IDs this session covers.
    /// Length equals `total_steps` (or 1 for single-provider sessions).
    pub provider_sequence: Vec<String>,
    /// Answers collected per provider, keyed by provider_id.
    pub answers_by_provider: std::collections::HashMap<String, serde_json::Value>,
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
            provider_sequence: Vec::new(),
            answers_by_provider: std::collections::HashMap::new(),
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
    /// Raw label text from FormSpec. When set, the SPA uses this directly
    /// instead of calling `t(label_key)`, so real question titles come through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_text: Option<String>,
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
