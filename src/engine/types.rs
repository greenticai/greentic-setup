//! Core types for the setup engine.
//!
//! Contains request/config types used across the engine.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::{Map as JsonMap, Value};

use crate::plan::{
    AccessChangeSelection, PackDefaultSelection, PackRemoveSelection, RemoveTarget,
    TenantSelection, UpdateOp,
};
use crate::platform_setup::{PlatformSetupAnswers, StaticRoutesPolicy, TelemetryAnswers};

fn default_true() -> bool {
    true
}

/// A declarative provider entry in the answers document.
///
/// Each entry maps to a `provider add` + `link-bundle` mutation during
/// post-setup auto-deploy.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    /// Provider kind (e.g. `telegram`, `slack`, `webex`).
    pub kind: String,
    /// Human-readable display name for the messaging endpoint.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Whether to link the provider to the deployed bundle.
    /// Defaults to `true` — matching `provider add` interactive behaviour.
    /// Set to `false` explicitly to register without linking.
    #[serde(default = "default_true")]
    pub link_bundle: bool,
    /// Provider-specific setup answers (may contain secret values).
    #[serde(default)]
    pub answers: JsonMap<String, Value>,
    /// Override the default provider instance id.
    #[serde(default)]
    pub provider_id: Option<String>,
}

/// Loaded answers from a JSON/YAML file.
#[derive(Clone, Debug, Default)]
pub struct LoadedAnswers {
    pub tenant: Option<String>,
    pub team: Option<String>,
    pub env: Option<String>,
    pub platform_setup: PlatformSetupAnswers,
    pub setup_answers: JsonMap<String, Value>,
    /// Declarative provider wiring — processed after bundle deploy.
    pub providers: Vec<ProviderEntry>,
}

/// The request object that drives plan building.
#[derive(Clone, Debug, Default)]
pub struct SetupRequest {
    pub bundle: PathBuf,
    pub bundle_name: Option<String>,
    pub pack_refs: Vec<String>,
    pub tenants: Vec<TenantSelection>,
    pub default_assignments: Vec<PackDefaultSelection>,
    pub providers: Vec<String>,
    pub update_ops: BTreeSet<UpdateOp>,
    pub remove_targets: BTreeSet<RemoveTarget>,
    pub packs_remove: Vec<PackRemoveSelection>,
    pub providers_remove: Vec<String>,
    pub tenants_remove: Vec<TenantSelection>,
    pub access_changes: Vec<AccessChangeSelection>,
    pub static_routes: StaticRoutesPolicy,
    pub deployment_targets: Vec<crate::deployment_targets::DeploymentTargetRecord>,
    pub tunnel: Option<crate::platform_setup::TunnelAnswers>,
    pub telemetry: Option<TelemetryAnswers>,
    pub setup_answers: serde_json::Map<String, serde_json::Value>,
    /// Filter by provider domain (messaging, events, secrets, oauth).
    pub domain_filter: Option<String>,
    /// Number of parallel setup operations.
    pub parallel: usize,
    /// Backup existing config before setup.
    pub backup: bool,
    /// Skip secrets initialization.
    pub skip_secrets_init: bool,
    /// Continue on error (best effort).
    pub best_effort: bool,
}

/// Configuration for the setup engine.
pub struct SetupConfig {
    pub tenant: String,
    pub team: Option<String>,
    pub env: String,
    pub offline: bool,
    pub verbose: bool,
}
