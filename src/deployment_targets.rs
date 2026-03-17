use std::path::{Path, PathBuf};

use anyhow::Context;
use dialoguer::{Confirm, Select};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DeploymentTargetsDocument {
    pub version: String,
    pub targets: Vec<DeploymentTargetRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeploymentTargetRecord {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_pack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
}

pub fn persist_explicit_deployment_targets(
    bundle_root: &Path,
    targets: &[DeploymentTargetRecord],
) -> anyhow::Result<Option<PathBuf>> {
    if targets.is_empty() {
        return Ok(None);
    }

    let doc = DeploymentTargetsDocument {
        version: "1".to_string(),
        targets: targets.to_vec(),
    };

    let path = bundle_root
        .join(".greentic")
        .join("deployment-targets.json");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(&doc).context("serialize deployment targets")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(Some(path))
}

pub fn prompt_deployment_targets(
    candidates: &[PathBuf],
) -> anyhow::Result<Vec<DeploymentTargetRecord>> {
    let mut targets = Vec::new();
    for candidate in candidates {
        let label = candidate.display().to_string();
        let should_include = Confirm::new()
            .with_prompt(format!(
                "Use deployer pack {label} for gtc start deployment?"
            ))
            .default(true)
            .interact()?;
        if !should_include {
            continue;
        }
        let choices = ["aws", "gcp", "azure", "single-vm"];
        let index = Select::new()
            .with_prompt(format!("Which deployment target does {label} implement?"))
            .items(choices)
            .default(0)
            .interact()?;
        targets.push(DeploymentTargetRecord {
            target: choices[index].to_string(),
            provider_pack: Some(label),
            default: None,
        });
    }
    if targets.len() == 1
        && let Some(first) = targets.first_mut()
    {
        first.default = Some(true);
    }
    Ok(targets)
}

pub fn discover_deployer_pack_candidates(bundle_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    for search_dir in [
        bundle_root.join("packs"),
        bundle_root.join("providers").join("deployer"),
    ] {
        if !search_dir.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&search_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("gtpack") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if [
                "terraform",
                "aws",
                "gcp",
                "azure",
                "single-vm",
                "single_vm",
                "helm",
                "operator",
                "serverless",
                "snap",
                "juju",
                "k8s",
            ]
            .iter()
            .any(|needle| name.contains(needle))
                && let Ok(relative) = path.strip_prefix(bundle_root)
            {
                candidates.push(relative.to_path_buf());
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_explicit_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = persist_explicit_deployment_targets(
            temp.path(),
            &[DeploymentTargetRecord {
                target: "aws".into(),
                provider_pack: Some("packs/terraform.gtpack".into()),
                default: Some(true),
            }],
        )
        .expect("persist")
        .expect("path");
        let written = std::fs::read_to_string(path).expect("read");
        assert!(written.contains("\"target\": \"aws\""));
        assert!(written.contains("\"provider_pack\": \"packs/terraform.gtpack\""));
        assert!(written.contains("\"default\": true"));
    }
}
