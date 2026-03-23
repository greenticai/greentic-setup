//! Core types for platform setup and static routes policy.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::deployment_targets::DeploymentTargetRecord;
use crate::platform_setup::url::normalize_public_base_url;

pub(crate) const STATIC_ROUTES_VERSION: u32 = 1;
pub(crate) const PACK_DECLARED_POLICY: &str = "pack_declared";
pub(crate) const SURFACE_ENABLED: &str = "enabled";
pub(crate) const SURFACE_DISABLED: &str = "disabled";

/// Platform-level setup answers containing static routes and deployment targets.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlatformSetupAnswers {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_routes: Option<StaticRoutesAnswers>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deployment_targets: Vec<DeploymentTargetRecord>,
}

/// User-provided answers for static routes configuration.
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

/// Normalized static routes policy after validation.
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
    /// Create a disabled static routes policy.
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

    /// Convert policy to answers format for serialization.
    pub fn to_answers(&self) -> StaticRoutesAnswers {
        StaticRoutesAnswers {
            public_web_enabled: Some(self.public_web_enabled),
            public_base_url: self.public_base_url.clone(),
            public_surface_policy: Some(self.public_surface_policy.clone()),
            default_route_prefix_policy: Some(self.default_route_prefix_policy.clone()),
            tenant_path_policy: Some(self.tenant_path_policy.clone()),
        }
    }

    /// Normalize and validate user-provided answers into a policy.
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
