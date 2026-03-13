//! Setup plan types — mode, steps, and metadata for bundle lifecycle operations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::platform_setup::StaticRoutesPolicy;

/// The operation mode for a setup plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupMode {
    Create,
    Update,
    Remove,
}

impl SetupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Remove => "remove",
        }
    }
}

/// A complete setup plan with ordered steps and metadata.
#[derive(Clone, Debug, Serialize)]
pub struct SetupPlan {
    pub mode: String,
    pub dry_run: bool,
    pub bundle: PathBuf,
    pub steps: Vec<SetupStep>,
    pub metadata: SetupPlanMetadata,
}

/// Metadata carried alongside the plan for execution.
#[derive(Clone, Debug, Serialize)]
pub struct SetupPlanMetadata {
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
    pub setup_answers: serde_json::Map<String, serde_json::Value>,
}

/// A single step in the setup plan.
#[derive(Clone, Debug, Serialize)]
pub struct SetupStep {
    pub kind: SetupStepKind,
    pub description: String,
    pub details: BTreeMap<String, String>,
}

/// The kind of operation a setup step performs.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupStepKind {
    NoOp,
    ResolvePacks,
    CreateBundle,
    AddPacksToBundle,
    ValidateCapabilities,
    ApplyPackSetup,
    WriteGmapRules,
    RunResolver,
    CopyResolvedManifest,
    ValidateBundle,
}

/// Tenant + optional team + allow-list paths.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TenantSelection {
    pub tenant: String,
    pub team: Option<String>,
    pub allow_paths: Vec<String>,
}

/// Update operations that can be combined in an update plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOp {
    PacksAdd,
    PacksRemove,
    ProvidersAdd,
    ProvidersRemove,
    TenantsAdd,
    TenantsRemove,
    AccessChange,
}

impl UpdateOp {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "packs_add" => Some(Self::PacksAdd),
            "packs_remove" => Some(Self::PacksRemove),
            "providers_add" => Some(Self::ProvidersAdd),
            "providers_remove" => Some(Self::ProvidersRemove),
            "tenants_add" => Some(Self::TenantsAdd),
            "tenants_remove" => Some(Self::TenantsRemove),
            "access_change" => Some(Self::AccessChange),
            _ => None,
        }
    }
}

impl FromStr for UpdateOp {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or(())
    }
}

/// Remove targets for a remove plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoveTarget {
    Packs,
    Providers,
    TenantsTeams,
}

impl RemoveTarget {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "packs" => Some(Self::Packs),
            "providers" => Some(Self::Providers),
            "tenants_teams" => Some(Self::TenantsTeams),
            _ => None,
        }
    }
}

impl FromStr for RemoveTarget {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value).ok_or(())
    }
}

/// Pack scope for default assignments and removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackScope {
    Bundle,
    Global,
    Tenant { tenant_id: String },
    Team { tenant_id: String, team_id: String },
}

/// Selection for removing a pack.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackRemoveSelection {
    pub pack_identifier: String,
    #[serde(default)]
    pub scope: Option<PackScope>,
}

/// Selection for setting a pack as default.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackDefaultSelection {
    pub pack_identifier: String,
    pub scope: PackScope,
}

/// Access rule change operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessOperation {
    AllowAdd,
    AllowRemove,
}

/// Access rule change selection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessChangeSelection {
    pub pack_id: String,
    pub operation: AccessOperation,
    pub tenant_id: String,
    #[serde(default)]
    pub team_id: Option<String>,
}

/// Pack catalog listing entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackListing {
    pub id: String,
    pub label: String,
    pub reference: String,
}

/// Resolved pack information after fetching from a registry.
#[derive(Clone, Debug, Serialize)]
pub struct ResolvedPackInfo {
    pub source_ref: String,
    pub mapped_ref: String,
    pub resolved_digest: String,
    pub pack_id: String,
    pub entry_flows: Vec<String>,
    pub cached_path: PathBuf,
    pub output_path: PathBuf,
}

/// Report from executing a setup plan.
#[derive(Clone, Debug, Serialize)]
pub struct SetupExecutionReport {
    pub bundle: PathBuf,
    pub resolved_packs: Vec<ResolvedPackInfo>,
    pub resolved_manifests: Vec<PathBuf>,
    pub provider_updates: usize,
    pub warnings: Vec<String>,
}

/// Build a step with a kind, description, and key-value details.
pub fn step<const N: usize>(
    kind: SetupStepKind,
    description: &str,
    details: [(&str, String); N],
) -> SetupStep {
    let mut map = BTreeMap::new();
    for (key, value) in details {
        map.insert(key.to_string(), value);
    }
    SetupStep {
        kind,
        description: description.to_string(),
        details: map,
    }
}

/// Load a pack catalog from a JSON/YAML file.
pub fn load_catalog_from_file(path: &std::path::Path) -> anyhow::Result<Vec<PackListing>> {
    use anyhow::Context;
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("read catalog file {}", path.display()))?;

    if let Ok(parsed) = serde_json::from_str::<Vec<PackListing>>(&raw)
        .or_else(|_| serde_yaml_bw::from_str::<Vec<PackListing>>(&raw))
    {
        return Ok(parsed);
    }

    let registry: ProviderRegistryFile = serde_json::from_str(&raw)
        .or_else(|_| serde_yaml_bw::from_str(&raw))
        .with_context(|| format!("parse catalog/provider registry file {}", path.display()))?;
    Ok(registry
        .items
        .into_iter()
        .map(|item| PackListing {
            id: item.id,
            label: item.label.fallback,
            reference: item.reference,
        })
        .collect())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProviderRegistryFile {
    #[serde(default)]
    registry_version: Option<String>,
    #[serde(default)]
    items: Vec<ProviderRegistryItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProviderRegistryItem {
    id: String,
    label: ProviderRegistryLabel,
    #[serde(alias = "ref")]
    reference: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProviderRegistryLabel {
    #[serde(default)]
    i18n_key: Option<String>,
    fallback: String,
}

/// QA spec returned by the wizard mode query.
#[derive(Clone, Debug, Serialize)]
pub struct QaSpec {
    pub mode: String,
    pub questions: Vec<QaQuestion>,
}

/// A question in the wizard QA spec.
#[derive(Clone, Debug, Serialize)]
pub struct QaQuestion {
    pub id: String,
    pub title: String,
    pub required: bool,
}

/// Return the QA spec (questions) for a given setup mode.
pub fn spec(mode: SetupMode) -> QaSpec {
    QaSpec {
        mode: mode.as_str().to_string(),
        questions: vec![
            QaQuestion {
                id: "operator.bundle.path".to_string(),
                title: "Bundle output path".to_string(),
                required: true,
            },
            QaQuestion {
                id: "operator.packs.refs".to_string(),
                title: "Pack refs (catalog + custom)".to_string(),
                required: false,
            },
            QaQuestion {
                id: "operator.tenants".to_string(),
                title: "Tenants and optional teams".to_string(),
                required: true,
            },
            QaQuestion {
                id: "operator.allow.paths".to_string(),
                title: "Allow rules as PACK[/FLOW[/NODE]]".to_string(),
                required: false,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_mode_roundtrip() {
        assert_eq!(SetupMode::Create.as_str(), "create");
        assert_eq!(SetupMode::Update.as_str(), "update");
        assert_eq!(SetupMode::Remove.as_str(), "remove");
    }

    #[test]
    fn update_op_parse() {
        assert_eq!(UpdateOp::parse("packs_add"), Some(UpdateOp::PacksAdd));
        assert_eq!(UpdateOp::parse("unknown"), None);
    }

    #[test]
    fn remove_target_parse() {
        assert_eq!(RemoveTarget::parse("packs"), Some(RemoveTarget::Packs));
        assert_eq!(RemoveTarget::parse("xyz"), None);
    }

    #[test]
    fn qa_spec_has_required_questions() {
        let s = spec(SetupMode::Create);
        assert!(s.questions.iter().any(|q| q.required));
    }
}
