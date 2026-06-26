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
    let targets = reconcile_deployment_targets(bundle_root, targets)?;
    if targets.is_empty() {
        return Ok(None);
    }

    let doc = DeploymentTargetsDocument {
        version: "1".to_string(),
        targets,
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

pub fn reconcile_deployment_targets(
    bundle_root: &Path,
    explicit_targets: &[DeploymentTargetRecord],
) -> anyhow::Result<Vec<DeploymentTargetRecord>> {
    let mut targets = explicit_targets.to_vec();
    if !targets.iter().any(|record| record.target == "local") {
        targets.push(DeploymentTargetRecord {
            target: "local".to_string(),
            provider_pack: None,
            default: None,
        });
    }

    for candidate in discover_deployer_pack_candidates(bundle_root)? {
        let provider_pack = candidate.display().to_string();
        if let Some(existing) = targets
            .iter_mut()
            .find(|record| record.provider_pack.as_deref() == Some(provider_pack.as_str()))
        {
            if existing.target.trim().is_empty()
                && let Some(inferred) = infer_deployment_target_from_pack(&candidate)
            {
                existing.target = inferred.to_string();
            }
            continue;
        }

        let Some(inferred_target) = infer_deployment_target_from_pack(&candidate) else {
            continue;
        };

        if let Some(existing) = targets.iter_mut().find(|record| {
            record.target == inferred_target
                && record
                    .provider_pack
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
        }) {
            existing.provider_pack = Some(provider_pack);
            continue;
        }

        targets.push(DeploymentTargetRecord {
            target: inferred_target.to_string(),
            provider_pack: Some(provider_pack),
            default: None,
        });
    }

    Ok(targets)
}

fn infer_deployment_target_from_pack(path: &Path) -> Option<&'static str> {
    let normalized = path
        .file_stem()
        .or_else(|| path.file_name())
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace('_', "-");
    if normalized.contains("aws") {
        Some("aws")
    } else if normalized.contains("gcp") || normalized.contains("google-cloud") {
        Some("gcp")
    } else if normalized.contains("azure") {
        Some("azure")
    } else if normalized.contains("single-vm") {
        Some("single-vm")
    } else {
        None
    }
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
            if is_deployer_pack_name(&name)
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

pub fn is_deployer_pack_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|value| value.to_str())
        .map(|name| is_deployer_pack_name(&name.to_ascii_lowercase()))
        .unwrap_or(false)
}

fn is_deployer_pack_name(name: &str) -> bool {
    [
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
        assert!(written.contains("\"target\": \"local\""));
    }

    #[test]
    fn reconciles_explicit_targets_with_installed_deployer_packs_and_local() {
        let temp = tempfile::tempdir().expect("tempdir");
        let deployer_dir = temp.path().join("providers").join("deployer");
        std::fs::create_dir_all(&deployer_dir).expect("deployer dir");
        std::fs::write(deployer_dir.join("aws.gtpack"), "").expect("aws pack");
        std::fs::write(deployer_dir.join("gcp.gtpack"), "").expect("gcp pack");
        std::fs::write(deployer_dir.join("azure.gtpack"), "").expect("azure pack");

        let targets = reconcile_deployment_targets(
            temp.path(),
            &[
                DeploymentTargetRecord {
                    target: "runtime".into(),
                    provider_pack: None,
                    default: Some(true),
                },
                DeploymentTargetRecord {
                    target: "aws".into(),
                    provider_pack: None,
                    default: None,
                },
            ],
        )
        .expect("reconcile");

        assert_eq!(targets[0].target, "runtime");
        assert_eq!(targets[0].default, Some(true));
        assert!(targets.iter().any(|record| record.target == "local"));
        assert!(targets.iter().any(|record| {
            record.target == "aws"
                && record.provider_pack.as_deref() == Some("providers/deployer/aws.gtpack")
        }));
        assert!(targets.iter().any(|record| {
            record.target == "gcp"
                && record.provider_pack.as_deref() == Some("providers/deployer/gcp.gtpack")
        }));
        assert!(targets.iter().any(|record| {
            record.target == "azure"
                && record.provider_pack.as_deref() == Some("providers/deployer/azure.gtpack")
        }));
    }

    #[test]
    fn detects_deployer_pack_paths() {
        assert!(is_deployer_pack_path(Path::new("packs/aws.gtpack")));
        assert!(is_deployer_pack_path(Path::new(
            "providers/deployer/gcp.gtpack"
        )));
        assert!(!is_deployer_pack_path(Path::new(
            "packs/weather-app.gtpack"
        )));
    }
}
