use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::deployment_targets::DeploymentTargetRecord;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeployInputsDocument {
    pub version: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_pack: Option<String>,
    pub env: BTreeMap<String, String>,
}

pub fn persist_setup_deploy_inputs(
    bundle_root: &Path,
    targets: &[DeploymentTargetRecord],
) -> anyhow::Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for target in targets {
        let Some(doc) = build_deploy_inputs_doc(target) else {
            continue;
        };
        let path = deploy_inputs_path(bundle_root, &target.target);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let payload =
            serde_json::to_string_pretty(&doc).context("serialize setup deploy inputs")?;
        std::fs::write(&path, payload)
            .with_context(|| format!("failed to write {}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

pub fn deploy_inputs_path(bundle_root: &Path, target: &str) -> PathBuf {
    bundle_root
        .join(".greentic")
        .join("deploy")
        .join(target)
        .join("inputs.json")
}

fn build_deploy_inputs_doc(target: &DeploymentTargetRecord) -> Option<DeployInputsDocument> {
    let backend = match target.target.as_str() {
        "aws" => Some("s3"),
        "gcp" => Some("gcs"),
        "azure" => Some("azurerm"),
        _ => None,
    }?;

    let mut env = BTreeMap::new();
    env.insert(
        "GREENTIC_DEPLOY_TERRAFORM_VAR_REMOTE_STATE_BACKEND".to_string(),
        backend.to_string(),
    );

    Some(DeployInputsDocument {
        version: "1".to_string(),
        target: target.target.clone(),
        provider_pack: target.provider_pack.clone(),
        env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_remote_state_backend_defaults_per_cloud_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let written = persist_setup_deploy_inputs(
            temp.path(),
            &[
                DeploymentTargetRecord {
                    target: "aws".into(),
                    provider_pack: Some("packs/terraform.gtpack".into()),
                    default: Some(true),
                },
                DeploymentTargetRecord {
                    target: "gcp".into(),
                    provider_pack: Some("packs/terraform.gtpack".into()),
                    default: None,
                },
            ],
        )
        .expect("persist");

        assert_eq!(written.len(), 2);
        let aws = std::fs::read_to_string(deploy_inputs_path(temp.path(), "aws")).expect("aws");
        let gcp = std::fs::read_to_string(deploy_inputs_path(temp.path(), "gcp")).expect("gcp");
        assert!(aws.contains("\"GREENTIC_DEPLOY_TERRAFORM_VAR_REMOTE_STATE_BACKEND\": \"s3\""));
        assert!(gcp.contains("\"GREENTIC_DEPLOY_TERRAFORM_VAR_REMOTE_STATE_BACKEND\": \"gcs\""));
    }
}
