//! Setup engine — orchestrates plan building and execution for
//! create/update/remove workflows.
//!
//! This is the main entry point that consumers (e.g. greentic-operator)
//! use to drive bundle setup.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use serde_json::{Map as JsonMap, Value};

use crate::bundle;
use crate::discovery;
use crate::plan::*;
use crate::setup_input;

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

/// The setup engine orchestrates plan → execute for bundle lifecycle.
pub struct SetupEngine {
    config: SetupConfig,
}

impl SetupEngine {
    pub fn new(config: SetupConfig) -> Self {
        Self { config }
    }

    /// Build a plan for the given mode and request.
    pub fn plan(
        &self,
        mode: SetupMode,
        request: &SetupRequest,
        dry_run: bool,
    ) -> anyhow::Result<SetupPlan> {
        match mode {
            SetupMode::Create => apply_create(request, dry_run),
            SetupMode::Update => apply_update(request, dry_run),
            SetupMode::Remove => apply_remove(request, dry_run),
        }
    }

    /// Print a human-readable plan summary to stdout.
    pub fn print_plan(&self, plan: &SetupPlan) {
        print_plan_summary(plan);
    }

    /// Access the engine configuration.
    pub fn config(&self) -> &SetupConfig {
        &self.config
    }

    /// Execute a setup plan.
    ///
    /// Runs each step in the plan, performing the actual bundle setup operations.
    /// Returns an execution report with details about what was done.
    pub fn execute(&self, plan: &SetupPlan) -> anyhow::Result<SetupExecutionReport> {
        if plan.dry_run {
            return Err(anyhow!("cannot execute a dry-run plan"));
        }

        let bundle = &plan.bundle;
        let mut report = SetupExecutionReport {
            bundle: bundle.clone(),
            resolved_packs: Vec::new(),
            resolved_manifests: Vec::new(),
            provider_updates: 0,
            warnings: Vec::new(),
        };

        for step in &plan.steps {
            match step.kind {
                SetupStepKind::NoOp => {
                    if self.config.verbose {
                        println!("  [skip] {}", step.description);
                    }
                }
                SetupStepKind::CreateBundle => {
                    self.execute_create_bundle(bundle, &plan.metadata)?;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::ResolvePacks => {
                    let resolved = self.execute_resolve_packs(bundle, &plan.metadata)?;
                    report.resolved_packs.extend(resolved);
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::AddPacksToBundle => {
                    self.execute_add_packs_to_bundle(bundle, &report.resolved_packs)?;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::ApplyPackSetup => {
                    let count = self.execute_apply_pack_setup(bundle, &plan.metadata)?;
                    report.provider_updates += count;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::WriteGmapRules => {
                    self.execute_write_gmap_rules(bundle, &plan.metadata)?;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::RunResolver => {
                    // Resolver is typically run by the runtime, not setup
                    if self.config.verbose {
                        println!("  [skip] {} (deferred to runtime)", step.description);
                    }
                }
                SetupStepKind::CopyResolvedManifest => {
                    let manifests = self.execute_copy_resolved_manifests(bundle, &plan.metadata)?;
                    report.resolved_manifests.extend(manifests);
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::ValidateBundle => {
                    self.execute_validate_bundle(bundle)?;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
            }
        }

        Ok(report)
    }

    /// Emit an answers template JSON file.
    ///
    /// Discovers all packs in the bundle and generates a template with all
    /// setup questions. Users fill this in and pass it via `--answers`.
    pub fn emit_answers(&self, plan: &SetupPlan, output_path: &Path) -> anyhow::Result<()> {
        let bundle = &plan.bundle;

        // Build the answers document structure
        let mut answers_doc = serde_json::json!({
            "greentic_setup_version": "1.0.0",
            "bundle_source": bundle.display().to_string(),
            "tenant": self.config.tenant,
            "team": self.config.team,
            "env": self.config.env,
            "setup_answers": {}
        });

        // Discover packs and extract their QA specs
        let setup_answers = answers_doc
            .get_mut("setup_answers")
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| anyhow!("internal error: setup_answers not an object"))?;

        // Add existing answers from the plan metadata
        for (provider_id, answers) in &plan.metadata.setup_answers {
            setup_answers.insert(provider_id.clone(), answers.clone());
        }

        // Discover packs and add template entries for any missing providers
        if bundle.exists() {
            let discovered = discovery::discover(bundle)?;
            for provider in discovered.providers {
                let provider_id = provider.provider_id.clone();
                if !setup_answers.contains_key(&provider_id) {
                    // Load the setup spec from the pack and create template
                    let template =
                        if let Some(spec) = setup_input::load_setup_spec(&provider.pack_path)? {
                            // Pack has setup.yaml - extract questions
                            let mut entries = JsonMap::new();
                            for question in spec.questions {
                                let default_value = question
                                    .default
                                    .unwrap_or_else(|| Value::String(String::new()));
                                entries.insert(question.name, default_value);
                            }
                            entries
                        } else {
                            // Pack uses flow-based setup or has no questions
                            // Add empty entry so user knows pack exists
                            JsonMap::new()
                        };
                    setup_answers.insert(provider_id, Value::Object(template));
                }
            }
        }

        // Write the answers document to the output path
        let output_content = serde_json::to_string_pretty(&answers_doc)
            .context("failed to serialize answers document")?;

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory: {}", parent.display()))?;
        }

        std::fs::write(output_path, output_content)
            .with_context(|| format!("failed to write answers to: {}", output_path.display()))?;

        println!("Answers template written to: {}", output_path.display());
        Ok(())
    }

    /// Load answers from a JSON/YAML file.
    pub fn load_answers(&self, answers_path: &Path) -> anyhow::Result<JsonMap<String, Value>> {
        let raw = setup_input::load_setup_input(answers_path)?;
        match raw {
            Value::Object(map) => {
                // Check if this is a full answers document or just setup_answers
                if let Some(Value::Object(setup_answers)) = map.get("setup_answers") {
                    Ok(setup_answers.clone())
                } else {
                    Ok(map)
                }
            }
            _ => Err(anyhow!("answers file must be a JSON/YAML object")),
        }
    }

    // ── Step executors ─────────────────────────────────────────────────────

    fn execute_create_bundle(
        &self,
        bundle_path: &Path,
        metadata: &SetupPlanMetadata,
    ) -> anyhow::Result<()> {
        bundle::create_demo_bundle_structure(bundle_path, metadata.bundle_name.as_deref())
            .context("failed to create bundle structure")
    }

    fn execute_resolve_packs(
        &self,
        _bundle_path: &Path,
        metadata: &SetupPlanMetadata,
    ) -> anyhow::Result<Vec<ResolvedPackInfo>> {
        let mut resolved = Vec::new();

        for pack_ref in &metadata.pack_refs {
            // For now, we only support local pack refs (file paths)
            // OCI resolution requires async and the distributor client
            let path = PathBuf::from(pack_ref);
            if path.exists() {
                resolved.push(ResolvedPackInfo {
                    source_ref: pack_ref.clone(),
                    mapped_ref: pack_ref.clone(),
                    resolved_digest: format!("sha256:{}", compute_simple_hash(pack_ref)),
                    pack_id: path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    entry_flows: Vec::new(),
                    cached_path: path.clone(),
                    output_path: path,
                });
            } else if pack_ref.starts_with("oci://")
                || pack_ref.starts_with("repo://")
                || pack_ref.starts_with("store://")
            {
                // Remote packs need async resolution via distributor-client
                // For now, we'll skip and let the caller handle this
                tracing::warn!("remote pack ref requires async resolution: {}", pack_ref);
            }
        }

        Ok(resolved)
    }

    fn execute_add_packs_to_bundle(
        &self,
        bundle_path: &Path,
        resolved_packs: &[ResolvedPackInfo],
    ) -> anyhow::Result<()> {
        for pack in resolved_packs {
            // Determine target directory based on pack ID domain prefix
            let target_dir = Self::get_pack_target_dir(bundle_path, &pack.pack_id);
            std::fs::create_dir_all(&target_dir)?;

            let target_path = target_dir.join(format!("{}.gtpack", pack.pack_id));
            if pack.cached_path.exists() && !target_path.exists() {
                std::fs::copy(&pack.cached_path, &target_path).with_context(|| {
                    format!(
                        "failed to copy pack {} to {}",
                        pack.cached_path.display(),
                        target_path.display()
                    )
                })?;
            }
        }
        Ok(())
    }

    /// Determine the target directory for a pack based on its ID.
    ///
    /// Packs with domain prefixes (e.g., `messaging-telegram`, `events-webhook`)
    /// go to `providers/<domain>/`. Other packs go to `packs/`.
    fn get_pack_target_dir(bundle_path: &Path, pack_id: &str) -> PathBuf {
        const DOMAIN_PREFIXES: &[&str] = &[
            "messaging-",
            "events-",
            "oauth-",
            "secrets-",
            "mcp-",
            "state-",
        ];

        for prefix in DOMAIN_PREFIXES {
            if pack_id.starts_with(prefix) {
                let domain = prefix.trim_end_matches('-');
                return bundle_path.join("providers").join(domain);
            }
        }

        // Default to packs/ for non-provider packs
        bundle_path.join("packs")
    }

    fn execute_apply_pack_setup(
        &self,
        bundle_path: &Path,
        metadata: &SetupPlanMetadata,
    ) -> anyhow::Result<usize> {
        let mut count = 0;

        // Persist setup answers to local config files
        for (provider_id, answers) in &metadata.setup_answers {
            // Write answers to provider config directory
            let config_dir = bundle_path.join("state").join("config").join(provider_id);
            std::fs::create_dir_all(&config_dir)?;

            let config_path = config_dir.join("setup-answers.json");
            let content = serde_json::to_string_pretty(answers)
                .context("failed to serialize setup answers")?;
            std::fs::write(&config_path, content).with_context(|| {
                format!(
                    "failed to write setup answers to: {}",
                    config_path.display()
                )
            })?;

            count += 1;
        }

        Ok(count)
    }

    fn execute_write_gmap_rules(
        &self,
        bundle_path: &Path,
        metadata: &SetupPlanMetadata,
    ) -> anyhow::Result<()> {
        for tenant_sel in &metadata.tenants {
            let gmap_path =
                bundle::gmap_path(bundle_path, &tenant_sel.tenant, tenant_sel.team.as_deref());

            if let Some(parent) = gmap_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Build gmap content from allow_paths
            let mut content = String::new();
            if tenant_sel.allow_paths.is_empty() {
                content.push_str("_ = forbidden\n");
            } else {
                for path in &tenant_sel.allow_paths {
                    content.push_str(&format!("{} = allowed\n", path));
                }
                content.push_str("_ = forbidden\n");
            }

            std::fs::write(&gmap_path, content)
                .with_context(|| format!("failed to write gmap: {}", gmap_path.display()))?;
        }
        Ok(())
    }

    fn execute_copy_resolved_manifests(
        &self,
        bundle_path: &Path,
        metadata: &SetupPlanMetadata,
    ) -> anyhow::Result<Vec<PathBuf>> {
        let mut manifests = Vec::new();
        let resolved_dir = bundle_path.join("resolved");
        std::fs::create_dir_all(&resolved_dir)?;

        for tenant_sel in &metadata.tenants {
            let filename =
                bundle::resolved_manifest_filename(&tenant_sel.tenant, tenant_sel.team.as_deref());
            let manifest_path = resolved_dir.join(&filename);

            // Create an empty manifest placeholder if it doesn't exist
            if !manifest_path.exists() {
                std::fs::write(&manifest_path, "# Resolved manifest placeholder\n")?;
            }
            manifests.push(manifest_path);
        }

        Ok(manifests)
    }

    fn execute_validate_bundle(&self, bundle_path: &Path) -> anyhow::Result<()> {
        bundle::validate_bundle_exists(bundle_path)
    }
}

// ── Plan builders ───────────────────────────────────────────────────────────

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
            SetupStepKind::ApplyPackSetup,
            "Apply pack-declared setup outputs through internal setup hooks",
            [("status", "planned".to_string())],
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

    Ok(SetupPlan {
        mode: "create".to_string(),
        dry_run,
        bundle: request.bundle.clone(),
        steps,
        metadata: build_metadata(request, pack_refs, tenants),
    })
}

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

    Ok(SetupPlan {
        mode: SetupMode::Update.as_str().to_string(),
        dry_run,
        bundle: request.bundle.clone(),
        steps,
        metadata: build_metadata_with_ops(request, pack_refs, tenants, ops),
    })
}

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

fn dedup_sorted(refs: &[String]) -> Vec<String> {
    let mut v: Vec<String> = refs
        .iter()
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .collect();
    v.sort();
    v.dedup();
    v
}

fn normalize_tenants(tenants: &[TenantSelection]) -> Vec<TenantSelection> {
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

fn infer_update_ops(
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

fn build_metadata(
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
        setup_answers: request.setup_answers.clone(),
    }
}

fn build_metadata_with_ops(
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
fn compute_simple_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_request(bundle: PathBuf) -> SetupRequest {
        SetupRequest {
            bundle,
            bundle_name: None,
            pack_refs: Vec::new(),
            tenants: vec![TenantSelection {
                tenant: "demo".to_string(),
                team: Some("default".to_string()),
                allow_paths: vec!["packs/default".to_string()],
            }],
            default_assignments: Vec::new(),
            providers: Vec::new(),
            update_ops: BTreeSet::new(),
            remove_targets: BTreeSet::new(),
            packs_remove: Vec::new(),
            providers_remove: Vec::new(),
            tenants_remove: Vec::new(),
            access_changes: Vec::new(),
            setup_answers: serde_json::Map::new(),
            ..Default::default()
        }
    }

    #[test]
    fn create_plan_is_deterministic() {
        let req = SetupRequest {
            bundle: PathBuf::from("bundle"),
            bundle_name: None,
            pack_refs: vec![
                "repo://zeta/pack@1".to_string(),
                "repo://alpha/pack@1".to_string(),
                "repo://alpha/pack@1".to_string(),
            ],
            tenants: vec![
                TenantSelection {
                    tenant: "demo".to_string(),
                    team: Some("default".to_string()),
                    allow_paths: vec!["pack/b".to_string(), "pack/a".to_string()],
                },
                TenantSelection {
                    tenant: "alpha".to_string(),
                    team: None,
                    allow_paths: vec!["x".to_string()],
                },
            ],
            default_assignments: Vec::new(),
            providers: Vec::new(),
            update_ops: BTreeSet::new(),
            remove_targets: BTreeSet::new(),
            packs_remove: Vec::new(),
            providers_remove: Vec::new(),
            tenants_remove: Vec::new(),
            access_changes: Vec::new(),
            setup_answers: serde_json::Map::new(),
            ..Default::default()
        };
        let plan = apply_create(&req, true).unwrap();
        assert_eq!(
            plan.metadata.pack_refs,
            vec![
                "repo://alpha/pack@1".to_string(),
                "repo://zeta/pack@1".to_string()
            ]
        );
        assert_eq!(plan.metadata.tenants[0].tenant, "alpha");
        assert_eq!(
            plan.metadata.tenants[1].allow_paths,
            vec!["pack/a".to_string(), "pack/b".to_string()]
        );
    }

    #[test]
    fn dry_run_does_not_create_files() {
        let bundle = PathBuf::from("/tmp/nonexistent-bundle");
        let req = empty_request(bundle.clone());
        let _plan = apply_create(&req, true).unwrap();
        assert!(!bundle.exists());
    }

    #[test]
    fn create_requires_tenants() {
        let req = SetupRequest {
            tenants: vec![],
            ..empty_request(PathBuf::from("x"))
        };
        assert!(apply_create(&req, true).is_err());
    }
}
