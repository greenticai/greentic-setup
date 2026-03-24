//! Admin API request/response types for bundle lifecycle management.
//!
//! These types define the contract between the admin API and consumers.
//! The actual HTTP routing is implemented in the consuming crate
//! (e.g. greentic-operator), which maps these to Axum handlers.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plan::{PackRemoveSelection, TenantSelection};

// ── Bundle deployment ───────────────────────────────────────────────────────

/// Request to deploy a new bundle or upgrade an existing one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleDeployRequest {
    /// Target bundle path on the server.
    pub bundle_path: PathBuf,
    /// Optional display name for the bundle.
    #[serde(default)]
    pub bundle_name: Option<String>,
    /// Pack references to resolve and install.
    #[serde(default)]
    pub pack_refs: Vec<String>,
    /// Tenant selections with allow rules.
    #[serde(default)]
    pub tenants: Vec<TenantSelection>,
    /// Pre-collected QA answers (provider_id → answers map).
    #[serde(default)]
    pub answers: Value,
    /// If true, only plan without executing.
    #[serde(default)]
    pub dry_run: bool,
}

/// Request to update an existing bundle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleUpdateRequest {
    /// Target bundle path on the server.
    pub bundle_path: PathBuf,
    /// Optional display name for the bundle.
    #[serde(default)]
    pub bundle_name: Option<String>,
    /// Pack references to resolve and install.
    #[serde(default)]
    pub pack_refs: Vec<String>,
    /// Tenant selections with allow rules.
    #[serde(default)]
    pub tenants: Vec<TenantSelection>,
    /// Pre-collected QA answers (provider_id → answers map).
    #[serde(default)]
    pub answers: Value,
    /// If true, only plan without executing.
    #[serde(default)]
    pub dry_run: bool,
}

/// Request to start a managed bundle runtime.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleStartRequest {
    /// Target bundle path.
    pub bundle_path: PathBuf,
}

/// Request to stop a managed bundle runtime.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleStopRequest {
    /// Target bundle path.
    pub bundle_path: PathBuf,
}

/// Request to add an admin client CN to the runtime allowlist.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminClientAddRequest {
    /// Target bundle path.
    pub bundle_path: PathBuf,
    /// Client CN to allow.
    pub client_cn: String,
}

/// Request to remove an admin client CN from the runtime allowlist.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminClientRemoveRequest {
    /// Target bundle path.
    pub bundle_path: PathBuf,
    /// Client CN to remove.
    pub client_cn: String,
}

/// Request to remove components from a bundle.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleRemoveRequest {
    /// Target bundle path.
    pub bundle_path: PathBuf,
    /// Packs to remove.
    #[serde(default)]
    pub packs: Vec<PackRemoveSelection>,
    /// Provider IDs to remove.
    #[serde(default)]
    pub providers: Vec<String>,
    /// Tenants/teams to remove.
    #[serde(default)]
    pub tenants: Vec<TenantSelection>,
    /// If true, only plan without executing.
    #[serde(default)]
    pub dry_run: bool,
}

// ── QA setup ────────────────────────────────────────────────────────────────

/// Request to get the QA FormSpec for a pack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QaSpecRequest {
    /// Bundle path.
    pub bundle_path: PathBuf,
    /// Provider ID to get spec for.
    pub provider_id: String,
    /// Locale for i18n resolution.
    #[serde(default = "default_locale")]
    pub locale: String,
}

/// Request to validate QA answers against a FormSpec.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QaValidateRequest {
    /// Bundle path.
    pub bundle_path: PathBuf,
    /// Provider ID.
    pub provider_id: String,
    /// Answers to validate.
    pub answers: Value,
}

/// Request to submit and persist QA answers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QaSubmitRequest {
    /// Bundle path.
    pub bundle_path: PathBuf,
    /// Provider ID.
    pub provider_id: String,
    /// Tenant ID.
    pub tenant: String,
    /// Team ID.
    #[serde(default)]
    pub team: Option<String>,
    /// Answers to persist.
    pub answers: Value,
    /// Whether to trigger a hot reload after persisting.
    #[serde(default)]
    pub reload: bool,
}

// ── Responses ───────────────────────────────────────────────────────────────

/// Generic admin API response wrapper.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> AdminResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message.into()),
        }
    }
}

/// Bundle status information.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleStatusResponse {
    pub bundle_path: PathBuf,
    pub status: BundleStatus,
    pub pack_count: usize,
    pub tenant_count: usize,
    pub provider_count: usize,
}

/// Bundle inventory listing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BundleListResponse {
    pub bundles: Vec<BundleStatusResponse>,
}

/// One admin client entry in the runtime allowlist registry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminClientEntry {
    pub client_cn: String,
}

/// Runtime admin allowlist inventory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminClientListResponse {
    pub admins: Vec<AdminClientEntry>,
}

/// Bundle lifecycle status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BundleStatus {
    Inactive,
    Active,
    Deploying,
    Updating,
    Stopping,
    Stopped,
    Removing,
    Error,
}

/// Unified admin request type for routing.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AdminRequest {
    Deploy(BundleDeployRequest),
    Update(BundleUpdateRequest),
    Remove(BundleRemoveRequest),
    Start(BundleStartRequest),
    Stop(BundleStopRequest),
    AddAdminClient(AdminClientAddRequest),
    RemoveAdminClient(AdminClientRemoveRequest),
    QaSpec(QaSpecRequest),
    QaValidate(QaValidateRequest),
    QaSubmit(QaSubmitRequest),
    Status { bundle_path: PathBuf },
    List,
}

fn default_locale() -> String {
    "en".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_response_ok() {
        let resp = AdminResponse::ok("hello");
        assert!(resp.success);
        assert_eq!(resp.data.unwrap(), "hello");
        assert!(resp.error.is_none());
    }

    #[test]
    fn admin_response_err() {
        let resp = AdminResponse::<()>::err("bad request");
        assert!(!resp.success);
        assert!(resp.data.is_none());
        assert_eq!(resp.error.unwrap(), "bad request");
    }

    #[test]
    fn deploy_request_serde_roundtrip() {
        let req = BundleDeployRequest {
            bundle_path: PathBuf::from("/tmp/bundle"),
            bundle_name: Some("test".into()),
            pack_refs: vec!["oci://test:latest".into()],
            tenants: vec![],
            answers: Value::Object(Default::default()),
            dry_run: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: BundleDeployRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bundle_path, PathBuf::from("/tmp/bundle"));
    }

    #[test]
    fn update_request_serde_roundtrip() {
        let req = BundleUpdateRequest {
            bundle_path: PathBuf::from("/tmp/bundle"),
            bundle_name: Some("test".into()),
            pack_refs: vec!["oci://test:latest".into()],
            tenants: vec![],
            answers: Value::Object(Default::default()),
            dry_run: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: BundleUpdateRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.bundle_path, PathBuf::from("/tmp/bundle"));
        assert!(parsed.dry_run);
    }

    #[test]
    fn admin_request_tagged_enum() {
        let json = r#"{"action":"list"}"#;
        let req: AdminRequest = serde_json::from_str(json).unwrap();
        assert!(matches!(req, AdminRequest::List));
    }

    #[test]
    fn bundle_status_serde() {
        let status = BundleStatus::Active;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"active\"");
    }

    #[test]
    fn bundle_status_stopped_serde() {
        let status = BundleStatus::Stopped;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"stopped\"");
    }
}
