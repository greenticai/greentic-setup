//! Bundle-level platform setup types and static routes policy handling.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use dialoguer::{Confirm, Input, Select};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::deployment_targets::DeploymentTargetRecord;

const STATIC_ROUTES_VERSION: u32 = 1;
const PACK_DECLARED_POLICY: &str = "pack_declared";
const SURFACE_ENABLED: &str = "enabled";
const SURFACE_DISABLED: &str = "disabled";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformSetupAnswers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_routes: Option<StaticRoutesAnswers>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deployment_targets: Vec<DeploymentTargetRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaticRoutesAnswers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_web_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_surface_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_route_prefix_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_path_policy: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StaticRoutesPolicy {
    pub version: u32,
    pub public_web_enabled: bool,
    pub public_base_url: Option<String>,
    pub public_surface_policy: String,
    pub default_route_prefix_policy: String,
    pub tenant_path_policy: String,
}

impl Default for StaticRoutesPolicy {
    fn default() -> Self {
        Self::disabled()
    }
}

impl StaticRoutesPolicy {
    pub fn disabled() -> Self {
        Self {
            version: STATIC_ROUTES_VERSION,
            public_web_enabled: false,
            public_base_url: None,
            public_surface_policy: SURFACE_DISABLED.to_string(),
            default_route_prefix_policy: PACK_DECLARED_POLICY.to_string(),
            tenant_path_policy: PACK_DECLARED_POLICY.to_string(),
        }
    }

    pub fn to_answers(&self) -> StaticRoutesAnswers {
        StaticRoutesAnswers {
            public_web_enabled: Some(self.public_web_enabled),
            public_base_url: self.public_base_url.clone(),
            public_surface_policy: Some(self.public_surface_policy.clone()),
            default_route_prefix_policy: Some(self.default_route_prefix_policy.clone()),
            tenant_path_policy: Some(self.tenant_path_policy.clone()),
        }
    }

    pub fn normalize(input: Option<&StaticRoutesAnswers>, env: &str) -> Result<Self> {
        let Some(input) = input else {
            return Ok(Self::disabled());
        };

        let public_web_enabled = input.public_web_enabled.unwrap_or(false);
        let public_surface_policy = input
            .public_surface_policy
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                if public_web_enabled {
                    SURFACE_ENABLED.to_string()
                } else {
                    SURFACE_DISABLED.to_string()
                }
            });

        if public_surface_policy != SURFACE_ENABLED && public_surface_policy != SURFACE_DISABLED {
            bail!(
                "public_surface_policy must be one of: {}, {}",
                SURFACE_ENABLED,
                SURFACE_DISABLED
            );
        }

        let default_route_prefix_policy = normalize_pack_declared_policy(
            "default_route_prefix_policy",
            input.default_route_prefix_policy.as_deref(),
        )?;
        let tenant_path_policy = normalize_pack_declared_policy(
            "tenant_path_policy",
            input.tenant_path_policy.as_deref(),
        )?;

        let public_base_url = match input.public_base_url.as_deref().map(str::trim) {
            Some("") | None => None,
            Some(url) => Some(normalize_public_base_url(url, env)?),
        };

        if public_web_enabled && public_base_url.is_none() {
            bail!("public_base_url is required when public_web_enabled=true");
        }

        if public_web_enabled && public_surface_policy == SURFACE_DISABLED {
            bail!("public_surface_policy=disabled is incompatible with public_web_enabled=true");
        }

        Ok(Self {
            version: STATIC_ROUTES_VERSION,
            public_web_enabled,
            public_base_url,
            public_surface_policy,
            default_route_prefix_policy,
            tenant_path_policy,
        })
    }
}

pub fn prompt_static_routes_policy(
    env: &str,
    current: Option<&StaticRoutesPolicy>,
) -> Result<StaticRoutesPolicy> {
    let current = current.cloned().unwrap_or_default();
    prompt_static_routes_policy_from_current(env, current)
}

pub fn prompt_static_routes_policy_with_answers(
    env: &str,
    current_answers: Option<&StaticRoutesAnswers>,
    existing: Option<&StaticRoutesPolicy>,
) -> Result<StaticRoutesPolicy> {
    let current = merge_prompt_seed(current_answers, existing);
    prompt_static_routes_policy_from_current(env, current)
}

fn prompt_static_routes_policy_from_current(
    env: &str,
    current: StaticRoutesPolicy,
) -> Result<StaticRoutesPolicy> {
    let public_web_enabled = Confirm::new()
        .with_prompt("Enable public web/static hosting for this bundle?")
        .default(current.public_web_enabled)
        .interact()?;

    if !public_web_enabled {
        return Ok(StaticRoutesPolicy::disabled());
    }

    let base_default = current.public_base_url.unwrap_or_default();
    let public_base_url: String = Input::new()
        .with_prompt("Public base URL")
        .with_initial_text(base_default)
        .interact_text()?;

    let policies = [SURFACE_ENABLED, SURFACE_DISABLED];
    let surface_index = policies
        .iter()
        .position(|value| *value == current.public_surface_policy)
        .unwrap_or(0);
    let public_surface_policy = policies[Select::new()
        .with_prompt("Public surface policy")
        .items(policies)
        .default(surface_index)
        .interact()?]
    .to_string();

    StaticRoutesPolicy::normalize(
        Some(&StaticRoutesAnswers {
            public_web_enabled: Some(public_web_enabled),
            public_base_url: Some(public_base_url),
            public_surface_policy: Some(public_surface_policy),
            default_route_prefix_policy: Some(current.default_route_prefix_policy),
            tenant_path_policy: Some(current.tenant_path_policy),
        }),
        env,
    )
}

fn merge_prompt_seed(
    current_answers: Option<&StaticRoutesAnswers>,
    existing: Option<&StaticRoutesPolicy>,
) -> StaticRoutesPolicy {
    let mut current = existing.cloned().unwrap_or_default();
    let Some(answers) = current_answers else {
        return current;
    };

    if let Some(enabled) = answers.public_web_enabled {
        current.public_web_enabled = enabled;
    }
    if let Some(url) = answers
        .public_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        current.public_base_url = Some(url.to_string());
    }
    if let Some(policy) = answers
        .public_surface_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        current.public_surface_policy = policy.to_string();
    }
    if let Some(policy) = answers
        .default_route_prefix_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        current.default_route_prefix_policy = policy.to_string();
    }
    if let Some(policy) = answers
        .tenant_path_policy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        current.tenant_path_policy = policy.to_string();
    }

    current
}

pub fn static_routes_artifact_path(bundle_root: &Path) -> PathBuf {
    bundle_root
        .join("state")
        .join("config")
        .join("platform")
        .join("static-routes.json")
}

pub fn load_static_routes_artifact(bundle_root: &Path) -> Result<Option<StaticRoutesPolicy>> {
    let path = static_routes_artifact_path(bundle_root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let policy = serde_json::from_str(&raw)
        .or_else(|_| serde_yaml_bw::from_str(&raw))
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(policy))
}

#[derive(Debug, Deserialize)]
struct RuntimeEndpoints {
    #[allow(dead_code)]
    tenant: Option<String>,
    #[allow(dead_code)]
    team: Option<String>,
    public_base_url: Option<String>,
}

pub fn load_runtime_public_base_url(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
) -> Result<Option<String>> {
    let team = team.unwrap_or("default");
    let path = bundle_root
        .join("state")
        .join("runtime")
        .join(format!("{tenant}.{team}"))
        .join("endpoints.json");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let endpoints: RuntimeEndpoints = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(endpoints
        .public_base_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string))
}

pub fn load_effective_static_routes_defaults(
    bundle_root: &Path,
    tenant: &str,
    team: Option<&str>,
) -> Result<Option<StaticRoutesPolicy>> {
    let mut policy = load_static_routes_artifact(bundle_root)?.unwrap_or_default();
    if policy.public_base_url.is_none()
        && let Some(runtime_url) = load_runtime_public_base_url(bundle_root, tenant, team)?
    {
        policy.public_base_url = Some(runtime_url);
    }
    if policy == StaticRoutesPolicy::disabled() {
        return Ok(None);
    }
    Ok(Some(policy))
}

pub fn persist_static_routes_artifact(
    bundle_root: &Path,
    policy: &StaticRoutesPolicy,
) -> Result<PathBuf> {
    let path = static_routes_artifact_path(bundle_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(policy).context("serialize static routes policy")?;
    std::fs::write(&path, payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn normalize_pack_declared_policy(field: &str, value: Option<&str>) -> Result<String> {
    let value = value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(PACK_DECLARED_POLICY);
    if value != PACK_DECLARED_POLICY {
        bail!("{field} must be '{}'", PACK_DECLARED_POLICY);
    }
    Ok(value.to_string())
}

fn normalize_public_base_url(value: &str, env: &str) -> Result<String> {
    let url = Url::parse(value).map_err(|err| anyhow!("invalid public_base_url: {err}"))?;
    match url.scheme() {
        "https" => {}
        "http" if is_local_http_origin(&url) => {}
        "http" => bail!("public_base_url must use https unless it targets localhost/loopback"),
        _ => bail!("public_base_url must use http or https"),
    }

    if url.host_str().is_none() {
        bail!("public_base_url must include a host");
    }
    if url.query().is_some() {
        bail!("public_base_url must not include a query string");
    }
    if url.fragment().is_some() {
        bail!("public_base_url must not include a fragment");
    }
    if env != "dev" && url.scheme() == "http" {
        bail!("public_base_url may only use http for localhost/loopback origins in dev");
    }

    let mut normalized = url.to_string();
    while normalized.ends_with('/') && normalized.len() > scheme_host_floor(&url) {
        normalized.pop();
    }
    if normalized.ends_with('/') && url.path() == "/" {
        normalized.pop();
    }
    Ok(normalized)
}

fn scheme_host_floor(url: &Url) -> usize {
    let host = url.host_str().unwrap_or_default();
    let mut floor = url.scheme().len() + 3 + host.len();
    if let Some(port) = url.port() {
        floor += 1 + port.to_string().len();
    }
    floor
}

fn is_local_http_origin(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|addr| addr.is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_default() {
        let policy = StaticRoutesPolicy::normalize(None, "dev").unwrap();
        assert_eq!(policy, StaticRoutesPolicy::disabled());
    }

    #[test]
    fn enabled_requires_base_url() {
        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                ..Default::default()
            }),
            "dev",
        )
        .unwrap_err();
        assert!(err.to_string().contains("public_base_url is required"));
    }

    #[test]
    fn normalizes_public_base_url() {
        let policy = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com/base/".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap();
        assert_eq!(
            policy.public_base_url.as_deref(),
            Some("https://example.com/base")
        );
        assert_eq!(policy.public_surface_policy, SURFACE_ENABLED);
        assert_eq!(policy.default_route_prefix_policy, PACK_DECLARED_POLICY);
        assert_eq!(policy.tenant_path_policy, PACK_DECLARED_POLICY);
    }

    #[test]
    fn rejects_query_and_fragment() {
        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com?x=1".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("query string"));

        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com#frag".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("fragment"));
    }

    #[test]
    fn allows_http_loopback_in_dev_only() {
        let policy = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("http://127.0.0.1:3000/".into()),
                ..Default::default()
            }),
            "dev",
        )
        .unwrap();
        assert_eq!(
            policy.public_base_url.as_deref(),
            Some("http://127.0.0.1:3000")
        );

        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("http://127.0.0.1:3000".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("dev"));
    }

    #[test]
    fn rejects_enabled_with_disabled_surface_policy() {
        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com".into()),
                public_surface_policy: Some("disabled".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("incompatible"));
    }

    #[test]
    fn persists_and_loads_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let policy = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap();
        let path = persist_static_routes_artifact(temp.path(), &policy).unwrap();
        assert!(path.exists());
        let loaded = load_static_routes_artifact(temp.path()).unwrap().unwrap();
        assert_eq!(loaded, policy);
    }

    #[test]
    fn effective_defaults_fall_back_to_runtime_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_dir = temp
            .path()
            .join("state")
            .join("runtime")
            .join("demo.default");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::write(
            runtime_dir.join("endpoints.json"),
            r#"{"tenant":"demo","team":"default","public_base_url":"https://runtime.example.com"}"#,
        )
        .unwrap();

        let loaded =
            load_effective_static_routes_defaults(temp.path(), "demo", Some("default")).unwrap();
        assert_eq!(
            loaded.and_then(|policy| policy.public_base_url),
            Some("https://runtime.example.com".to_string())
        );
    }

    #[test]
    fn merge_prompt_seed_overlays_partial_answers_on_existing_policy() {
        let existing = StaticRoutesPolicy {
            version: STATIC_ROUTES_VERSION,
            public_web_enabled: false,
            public_base_url: Some("https://existing.example.com".into()),
            public_surface_policy: SURFACE_DISABLED.into(),
            default_route_prefix_policy: PACK_DECLARED_POLICY.into(),
            tenant_path_policy: PACK_DECLARED_POLICY.into(),
        };
        let answers = StaticRoutesAnswers {
            public_web_enabled: Some(true),
            public_base_url: None,
            public_surface_policy: Some(SURFACE_ENABLED.into()),
            default_route_prefix_policy: None,
            tenant_path_policy: None,
        };

        let merged = merge_prompt_seed(Some(&answers), Some(&existing));
        assert!(merged.public_web_enabled);
        assert_eq!(
            merged.public_base_url.as_deref(),
            Some("https://existing.example.com")
        );
        assert_eq!(merged.public_surface_policy, SURFACE_ENABLED);
    }
}
