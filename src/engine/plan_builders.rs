//! Plan builders for create/update/remove operations.
//!
//! These functions construct `SetupPlan` objects based on `SetupRequest` input.

use std::collections::BTreeSet;

use anyhow::anyhow;

use crate::plan::*;
use crate::setup_input::SetupQuestion;
use serde_json::Value;

use super::types::SetupRequest;

/// Build a plan for create mode.
pub fn apply_create(request: &SetupRequest, dry_run: bool) -> anyhow::Result<SetupPlan> {
    if request.tenants.is_empty() {
        return Err(anyhow!("at least one tenant selection is required"));
    }

    let pack_refs = dedup_sorted(&request.pack_refs);
    let tenants = normalize_tenants(&request.tenants);

    let mut steps = Vec::new();
    if !pack_refs.is_empty() {
        steps.push(step(
            SetupStepKind::ResolvePacks,
            "Resolve selected pack refs via distributor client",
            [("count", pack_refs.len().to_string())],
        ));
    } else {
        steps.push(step(
            SetupStepKind::NoOp,
            "No pack refs selected; skipping pack resolution",
            [("reason", "empty_pack_refs".to_string())],
        ));
    }
    steps.push(step(
        SetupStepKind::CreateBundle,
        "Create demo bundle scaffold using existing conventions",
        [("bundle", request.bundle.display().to_string())],
    ));
    if !pack_refs.is_empty() {
        steps.push(step(
            SetupStepKind::AddPacksToBundle,
            "Copy fetched packs into bundle/packs",
            [("count", pack_refs.len().to_string())],
        ));
        steps.push(step(
            SetupStepKind::ValidateCapabilities,
            "Validate provider packs have capabilities extension",
            [("check", "greentic.ext.capabilities.v1".to_string())],
        ));
        steps.push(step(
            SetupStepKind::ApplyPackSetup,
            "Apply pack-declared setup outputs through internal setup hooks",
            [("status", "planned".to_string())],
        ));
    } else if !request.setup_answers.is_empty() {
        // No new packs to fetch, but answers were provided for existing packs
        steps.push(step(
            SetupStepKind::ValidateCapabilities,
            "Validate provider packs have capabilities extension",
            [("check", "greentic.ext.capabilities.v1".to_string())],
        ));
        steps.push(step(
            SetupStepKind::ApplyPackSetup,
            "Apply setup answers to existing bundle packs",
            [("providers", request.setup_answers.len().to_string())],
        ));
    } else {
        steps.push(step(
            SetupStepKind::NoOp,
            "No fetched packs to add or setup",
            [("reason", "empty_pack_refs".to_string())],
        ));
    }
    steps.push(step(
        SetupStepKind::WriteGmapRules,
        "Write tenant/team allow rules to gmap",
        [("targets", tenants.len().to_string())],
    ));
    steps.push(step(
        SetupStepKind::RunResolver,
        "Run resolver pipeline (same as demo allow)",
        [("resolver", "project::sync_project".to_string())],
    ));
    steps.push(step(
        SetupStepKind::CopyResolvedManifest,
        "Copy state/resolved manifests into resolved/ for demo start",
        [("targets", tenants.len().to_string())],
    ));
    steps.push(step(
        SetupStepKind::ValidateBundle,
        "Validate bundle is loadable by internal demo pipeline",
        [("check", "resolved manifests present".to_string())],
    ));
    steps.push(step(
        SetupStepKind::BuildFlowIndex,
        "Build fast2flow routing indexes and intents.md",
        [("output", "state/indexes/".to_string())],
    ));

    Ok(SetupPlan {
        mode: "create".to_string(),
        dry_run,
        bundle: request.bundle.clone(),
        steps,
        metadata: build_metadata(request, pack_refs, tenants),
    })
}

/// Build a plan for update mode.
pub fn apply_update(request: &SetupRequest, dry_run: bool) -> anyhow::Result<SetupPlan> {
    let pack_refs = dedup_sorted(&request.pack_refs);
    let tenants = normalize_tenants(&request.tenants);

    let mut ops = request.update_ops.clone();
    if ops.is_empty() {
        infer_update_ops(&mut ops, &pack_refs, request, &tenants);
    }

    let mut steps = vec![step(
        SetupStepKind::ValidateBundle,
        "Validate target bundle exists before update",
        [("mode", "update".to_string())],
    )];

    if ops.is_empty() {
        steps.push(step(
            SetupStepKind::NoOp,
            "No update operations selected",
            [("reason", "empty_update_ops".to_string())],
        ));
    }
    if ops.contains(&UpdateOp::PacksAdd) {
        if pack_refs.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "packs_add selected without pack refs",
                [("reason", "empty_pack_refs".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::ResolvePacks,
                "Resolve selected pack refs via distributor client",
                [("count", pack_refs.len().to_string())],
            ));
            steps.push(step(
                SetupStepKind::AddPacksToBundle,
                "Copy fetched packs into bundle/packs",
                [("count", pack_refs.len().to_string())],
            ));
        }
    }
    if ops.contains(&UpdateOp::PacksRemove) {
        if request.packs_remove.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "packs_remove selected without targets",
                [("reason", "empty_packs_remove".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::AddPacksToBundle,
                "Remove pack artifacts/default links from bundle",
                [("count", request.packs_remove.len().to_string())],
            ));
        }
    }
    if ops.contains(&UpdateOp::ProvidersAdd) {
        if request.providers.is_empty() && pack_refs.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "providers_add selected without providers or new packs",
                [("reason", "empty_providers_add".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::ApplyPackSetup,
                "Enable providers in providers/providers.json",
                [("count", request.providers.len().to_string())],
            ));
        }
    }
    if ops.contains(&UpdateOp::ProvidersRemove) {
        if request.providers_remove.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "providers_remove selected without providers",
                [("reason", "empty_providers_remove".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::ApplyPackSetup,
                "Disable/remove providers in providers/providers.json",
                [("count", request.providers_remove.len().to_string())],
            ));
        }
    }
    if ops.contains(&UpdateOp::TenantsAdd) {
        if tenants.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "tenants_add selected without tenant targets",
                [("reason", "empty_tenants_add".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::WriteGmapRules,
                "Ensure tenant/team directories and allow rules",
                [("targets", tenants.len().to_string())],
            ));
        }
    }
    if ops.contains(&UpdateOp::TenantsRemove) {
        if request.tenants_remove.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "tenants_remove selected without tenant targets",
                [("reason", "empty_tenants_remove".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::WriteGmapRules,
                "Remove tenant/team directories and related rules",
                [("targets", request.tenants_remove.len().to_string())],
            ));
        }
    }
    if ops.contains(&UpdateOp::AccessChange) {
        let access_count = request.access_changes.len()
            + tenants.iter().filter(|t| !t.allow_paths.is_empty()).count();
        if access_count == 0 {
            steps.push(step(
                SetupStepKind::NoOp,
                "access_change selected without mutations",
                [("reason", "empty_access_changes".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::WriteGmapRules,
                "Apply access rule updates",
                [("changes", access_count.to_string())],
            ));
            steps.push(step(
                SetupStepKind::RunResolver,
                "Run resolver pipeline (same as demo allow/forbid)",
                [("resolver", "project::sync_project".to_string())],
            ));
            steps.push(step(
                SetupStepKind::CopyResolvedManifest,
                "Copy state/resolved manifests into resolved/ for demo start",
                [("targets", tenants.len().to_string())],
            ));
        }
    }
    steps.push(step(
        SetupStepKind::ValidateBundle,
        "Validate bundle is loadable by internal demo pipeline",
        [("check", "resolved manifests present".to_string())],
    ));
    steps.push(step(
        SetupStepKind::BuildFlowIndex,
        "Rebuild fast2flow routing indexes after update",
        [("output", "state/indexes/".to_string())],
    ));

    Ok(SetupPlan {
        mode: SetupMode::Update.as_str().to_string(),
        dry_run,
        bundle: request.bundle.clone(),
        steps,
        metadata: build_metadata_with_ops(request, pack_refs, tenants, ops),
    })
}

/// Build a plan for remove mode.
pub fn apply_remove(request: &SetupRequest, dry_run: bool) -> anyhow::Result<SetupPlan> {
    let tenants = normalize_tenants(&request.tenants);

    let mut targets = request.remove_targets.clone();
    if targets.is_empty() {
        if !request.packs_remove.is_empty() {
            targets.insert(RemoveTarget::Packs);
        }
        if !request.providers_remove.is_empty() {
            targets.insert(RemoveTarget::Providers);
        }
        if !request.tenants_remove.is_empty() {
            targets.insert(RemoveTarget::TenantsTeams);
        }
    }

    let mut steps = vec![step(
        SetupStepKind::ValidateBundle,
        "Validate target bundle exists before remove",
        [("mode", "remove".to_string())],
    )];

    if targets.is_empty() {
        steps.push(step(
            SetupStepKind::NoOp,
            "No remove targets selected",
            [("reason", "empty_remove_targets".to_string())],
        ));
    }
    if targets.contains(&RemoveTarget::Packs) {
        if request.packs_remove.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "packs target selected without pack identifiers",
                [("reason", "empty_packs_remove".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::AddPacksToBundle,
                "Delete pack files/default links from bundle",
                [("count", request.packs_remove.len().to_string())],
            ));
        }
    }
    if targets.contains(&RemoveTarget::Providers) {
        if request.providers_remove.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "providers target selected without provider ids",
                [("reason", "empty_providers_remove".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::ApplyPackSetup,
                "Remove provider entries from providers/providers.json",
                [("count", request.providers_remove.len().to_string())],
            ));
        }
    }
    if targets.contains(&RemoveTarget::TenantsTeams) {
        if request.tenants_remove.is_empty() {
            steps.push(step(
                SetupStepKind::NoOp,
                "tenants_teams target selected without tenant/team ids",
                [("reason", "empty_tenants_remove".to_string())],
            ));
        } else {
            steps.push(step(
                SetupStepKind::WriteGmapRules,
                "Delete tenant/team directories and access rules",
                [("count", request.tenants_remove.len().to_string())],
            ));
            steps.push(step(
                SetupStepKind::RunResolver,
                "Run resolver pipeline after tenant/team removals",
                [("resolver", "project::sync_project".to_string())],
            ));
            steps.push(step(
                SetupStepKind::CopyResolvedManifest,
                "Copy state/resolved manifests into resolved/ for demo start",
                [("targets", tenants.len().to_string())],
            ));
        }
    }
    steps.push(step(
        SetupStepKind::ValidateBundle,
        "Validate bundle is loadable by internal demo pipeline",
        [("check", "resolved manifests present".to_string())],
    ));

    Ok(SetupPlan {
        mode: SetupMode::Remove.as_str().to_string(),
        dry_run,
        bundle: request.bundle.clone(),
        steps,
        metadata: SetupPlanMetadata {
            bundle_name: request.bundle_name.clone(),
            pack_refs: Vec::new(),
            tenants,
            default_assignments: request.default_assignments.clone(),
            providers: request.providers.clone(),
            update_ops: request.update_ops.clone(),
            remove_targets: targets,
            packs_remove: request.packs_remove.clone(),
            providers_remove: request.providers_remove.clone(),
            tenants_remove: request.tenants_remove.clone(),
            access_changes: request.access_changes.clone(),
            static_routes: request.static_routes.clone(),
            deployment_targets: request.deployment_targets.clone(),
            tunnel: request.tunnel.clone(),
            setup_answers: request.setup_answers.clone(),
        },
    })
}

/// Print a human-readable plan summary.
pub fn print_plan_summary(plan: &SetupPlan) {
    println!("wizard plan: mode={} dry_run={}", plan.mode, plan.dry_run);
    println!("bundle: {}", plan.bundle.display());
    let noop_count = plan
        .steps
        .iter()
        .filter(|s| s.kind == SetupStepKind::NoOp)
        .count();
    if noop_count > 0 {
        println!("no-op steps: {noop_count}");
    }
    for (index, s) in plan.steps.iter().enumerate() {
        println!("{}. {}", index + 1, s.description);
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Deduplicate and sort a list of strings.
pub fn dedup_sorted(refs: &[String]) -> Vec<String> {
    let mut v: Vec<String> = refs
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Normalize tenant selections (sort and deduplicate allow_paths).
pub fn normalize_tenants(tenants: &[TenantSelection]) -> Vec<TenantSelection> {
    let mut result: Vec<TenantSelection> = tenants
        .iter()
        .map(|t| {
            let mut t = t.clone();
            t.allow_paths.sort();
            t.allow_paths.dedup();
            t
        })
        .collect();
    result.sort_by(|a, b| {
        a.tenant
            .cmp(&b.tenant)
            .then_with(|| a.team.cmp(&b.team))
            .then_with(|| a.allow_paths.cmp(&b.allow_paths))
    });
    result
}

/// Infer update operations from request content.
pub fn infer_update_ops(
    ops: &mut BTreeSet<UpdateOp>,
    pack_refs: &[String],
    request: &SetupRequest,
    tenants: &[TenantSelection],
) {
    if !pack_refs.is_empty() {
        ops.insert(UpdateOp::PacksAdd);
    }
    if !request.providers.is_empty() {
        ops.insert(UpdateOp::ProvidersAdd);
    }
    if !request.providers_remove.is_empty() {
        ops.insert(UpdateOp::ProvidersRemove);
    }
    if !request.packs_remove.is_empty() {
        ops.insert(UpdateOp::PacksRemove);
    }
    if !tenants.is_empty() {
        ops.insert(UpdateOp::TenantsAdd);
    }
    if !request.tenants_remove.is_empty() {
        ops.insert(UpdateOp::TenantsRemove);
    }
    if !request.access_changes.is_empty() || tenants.iter().any(|t| !t.allow_paths.is_empty()) {
        ops.insert(UpdateOp::AccessChange);
    }
}

/// Build metadata for a plan.
pub fn build_metadata(
    request: &SetupRequest,
    pack_refs: Vec<String>,
    tenants: Vec<TenantSelection>,
) -> SetupPlanMetadata {
    SetupPlanMetadata {
        bundle_name: request.bundle_name.clone(),
        pack_refs,
        tenants,
        default_assignments: request.default_assignments.clone(),
        providers: request.providers.clone(),
        update_ops: request.update_ops.clone(),
        remove_targets: request.remove_targets.clone(),
        packs_remove: request.packs_remove.clone(),
        providers_remove: request.providers_remove.clone(),
        tenants_remove: request.tenants_remove.clone(),
        access_changes: request.access_changes.clone(),
        static_routes: request.static_routes.clone(),
        deployment_targets: request.deployment_targets.clone(),
        tunnel: request.tunnel.clone(),
        setup_answers: request.setup_answers.clone(),
    }
}

/// Build metadata with explicit update operations.
pub fn build_metadata_with_ops(
    request: &SetupRequest,
    pack_refs: Vec<String>,
    tenants: Vec<TenantSelection>,
    ops: BTreeSet<UpdateOp>,
) -> SetupPlanMetadata {
    let mut meta = build_metadata(request, pack_refs, tenants);
    meta.update_ops = ops;
    meta
}

/// Compute a simple hash for a string (used for digest placeholders).
pub fn compute_simple_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Infer a default value for a setup question.
///
/// Priority:
/// 1. Explicit `default` field from setup.yaml
/// 2. Extract from help text pattern "(default: VALUE)"
/// 3. Return empty string
pub fn infer_default_value(question: &SetupQuestion) -> Value {
    // First, use explicit default if present
    if let Some(default) = question.default.clone() {
        return default;
    }

    // Try to extract default from help text
    // Pattern: "(default: VALUE)" or "[default: VALUE]"
    if let Some(ref help) = question.help
        && let Some(default) = extract_default_from_help(help)
    {
        return Value::String(default);
    }

    // Fallback to empty string
    Value::String(String::new())
}

/// Extract default value from help text.
///
/// Matches patterns like:
/// - "(default: <https://slack.com/api>)"
/// - "[default: true]"
/// - "Default: some_value"
pub fn extract_default_from_help(help: &str) -> Option<String> {
    use regex::Regex;

    // Pattern 1: (default: VALUE) or [default: VALUE]
    let re = Regex::new(r"(?i)[\(\[]?\s*default:\s*([^\)\]\n,]+)\s*[\)\]]?").ok()?;
    if let Some(caps) = re.captures(help) {
        let value = caps.get(1)?.as_str().trim();
        // Clean up the value - remove trailing punctuation
        let cleaned = value.trim_end_matches(|c: char| c == '.' || c == ',' || c.is_whitespace());
        if !cleaned.is_empty() {
            return Some(cleaned.to_string());
        }
    }

    None
}
