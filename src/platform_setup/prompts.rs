//! Interactive prompts for static routes policy configuration.

use anyhow::Result;
use dialoguer::{Confirm, Input, Select};

use crate::platform_setup::types::{
    SURFACE_DISABLED, SURFACE_ENABLED, StaticRoutesAnswers, StaticRoutesPolicy,
};

/// Prompt user for static routes policy configuration.
pub fn prompt_static_routes_policy(
    env: &str,
    current: Option<&StaticRoutesPolicy>,
) -> Result<StaticRoutesPolicy> {
    let current = current.cloned().unwrap_or_default();
    prompt_static_routes_policy_from_current(env, current)
}

/// Prompt user for static routes policy with existing answers merged.
pub fn prompt_static_routes_policy_with_answers(
    env: &str,
    current_answers: Option<&StaticRoutesAnswers>,
    existing: Option<&StaticRoutesPolicy>,
) -> Result<StaticRoutesPolicy> {
    let current = merge_prompt_seed(current_answers, existing);
    prompt_static_routes_policy_from_current(env, current)
}

/// Internal implementation for prompting static routes policy.
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

/// Merge user-provided answers with existing policy to create prompt seed.
pub(crate) fn merge_prompt_seed(
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
