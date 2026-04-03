//! Setup engine — orchestrates plan building and execution for
//! create/update/remove workflows.
//!
//! This is the main entry point that consumers (e.g. greentic-operator)
//! use to drive bundle setup.

mod answers;
mod executors;
mod plan_builders;
mod types;

use std::path::Path;

use anyhow::anyhow;

use crate::plan::*;
use crate::platform_setup::persist_static_routes_artifact;

// Re-export types and functions for public API
pub use answers::{emit_answers, encrypt_secret_answers, load_answers, prompt_secret_answers};
pub use executors::{
    auto_install_provider_packs, domain_from_provider_id, execute_add_packs_to_bundle,
    execute_apply_pack_setup, execute_copy_resolved_manifests, execute_create_bundle,
    execute_remove_provider_artifacts, execute_resolve_packs, execute_validate_bundle,
    execute_write_gmap_rules, find_provider_pack_source, get_pack_target_dir,
};
pub use plan_builders::{
    apply_create, apply_remove, apply_update, build_metadata, build_metadata_with_ops,
    compute_simple_hash, dedup_sorted, extract_default_from_help, infer_default_value,
    infer_update_ops, normalize_tenants, print_plan_summary,
};
pub use types::{LoadedAnswers, SetupConfig, SetupRequest};

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
                    execute_create_bundle(bundle, &plan.metadata)?;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::ResolvePacks => {
                    let resolved = execute_resolve_packs(bundle, &plan.metadata)?;
                    report.resolved_packs.extend(resolved);
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::AddPacksToBundle => {
                    execute_add_packs_to_bundle(bundle, &report.resolved_packs)?;
                    let _ = crate::deployment_targets::persist_explicit_deployment_targets(
                        bundle,
                        &plan.metadata.deployment_targets,
                    );
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::ValidateCapabilities => {
                    let cap_report = crate::capabilities::validate_and_upgrade_packs(bundle)?;
                    for warn in &cap_report.warnings {
                        report.warnings.push(warn.message.clone());
                    }
                    if self.config.verbose {
                        println!(
                            "  [done] {} (checked={}, upgraded={})",
                            step.description,
                            cap_report.checked,
                            cap_report.upgraded.len()
                        );
                    }
                }
                SetupStepKind::ApplyPackSetup => {
                    let count = execute_apply_pack_setup(bundle, &plan.metadata, &self.config)?;
                    report.provider_updates += count;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::WriteGmapRules => {
                    execute_write_gmap_rules(bundle, &plan.metadata)?;
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
                    let manifests = execute_copy_resolved_manifests(bundle, &plan.metadata)?;
                    report.resolved_manifests.extend(manifests);
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
                SetupStepKind::ValidateBundle => {
                    execute_validate_bundle(bundle)?;
                    if self.config.verbose {
                        println!("  [done] {}", step.description);
                    }
                }
            }
        }

        // Persist bundle-level platform metadata even when no provider pack setup
        // steps ran, so create-only flows still materialize runtime/deployment config.
        persist_static_routes_artifact(bundle, &plan.metadata.static_routes)?;
        let _ = crate::deployment_targets::persist_explicit_deployment_targets(
            bundle,
            &plan.metadata.deployment_targets,
        );

        Ok(report)
    }

    /// Emit an answers template JSON file.
    ///
    /// Discovers all packs in the bundle and generates a template with all
    /// setup questions. Users fill this in and pass it via `--answers`.
    pub fn emit_answers(
        &self,
        plan: &SetupPlan,
        output_path: &Path,
        key: Option<&str>,
        interactive: bool,
    ) -> anyhow::Result<()> {
        emit_answers(&self.config, plan, output_path, key, interactive)
    }

    /// Load answers from a JSON/YAML file.
    pub fn load_answers(
        &self,
        answers_path: &Path,
        key: Option<&str>,
        interactive: bool,
    ) -> anyhow::Result<LoadedAnswers> {
        load_answers(answers_path, key, interactive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle;
    use crate::platform_setup::{StaticRoutesPolicy, static_routes_artifact_path};
    use serde_json::json;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

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
            static_routes: StaticRoutesPolicy::default(),
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
            static_routes: StaticRoutesPolicy::default(),
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

    #[test]
    fn load_answers_reads_platform_setup_and_provider_answers() {
        let temp = tempfile::tempdir().unwrap();
        let answers_path = temp.path().join("answers.yaml");
        std::fs::write(
            &answers_path,
            r#"
bundle_source: ./bundle
tenant: acme
team: core
env: prod
platform_setup:
  static_routes:
    public_web_enabled: true
    public_base_url: https://example.com/base/
  deployment_targets:
    - target: aws
      provider_pack: packs/aws.gtpack
      default: true
setup_answers:
  messaging-webchat:
    jwt_signing_key: abc
"#,
        )
        .unwrap();

        let loaded = load_answers(&answers_path, None, false).unwrap();
        assert_eq!(
            loaded
                .platform_setup
                .static_routes
                .as_ref()
                .and_then(|v| v.public_base_url.as_deref()),
            Some("https://example.com/base/")
        );
        assert_eq!(
            loaded
                .setup_answers
                .get("messaging-webchat")
                .and_then(|v| v.get("jwt_signing_key"))
                .and_then(serde_json::Value::as_str),
            Some("abc")
        );
        assert_eq!(loaded.tenant.as_deref(), Some("acme"));
        assert_eq!(loaded.team.as_deref(), Some("core"));
        assert_eq!(loaded.env.as_deref(), Some("prod"));
        assert_eq!(loaded.platform_setup.deployment_targets.len(), 1);
        assert_eq!(loaded.platform_setup.deployment_targets[0].target, "aws");
    }

    #[test]
    fn emit_answers_includes_platform_setup() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: None,
            env: "prod".into(),
            offline: false,
            verbose: false,
        });
        let request = SetupRequest {
            bundle: bundle_root.clone(),
            tenants: vec![TenantSelection {
                tenant: "demo".into(),
                team: None,
                allow_paths: Vec::new(),
            }],
            static_routes: StaticRoutesPolicy {
                public_web_enabled: true,
                public_base_url: Some("https://example.com".into()),
                public_surface_policy: "enabled".into(),
                default_route_prefix_policy: "pack_declared".into(),
                tenant_path_policy: "pack_declared".into(),
                ..StaticRoutesPolicy::default()
            },
            ..Default::default()
        };
        let plan = engine.plan(SetupMode::Create, &request, true).unwrap();
        let output = temp.path().join("answers.json");
        engine.emit_answers(&plan, &output, None, false).unwrap();
        let emitted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(output).unwrap()).unwrap();
        assert_eq!(
            emitted["platform_setup"]["static_routes"]["public_base_url"],
            json!("https://example.com")
        );
        assert_eq!(emitted["platform_setup"]["deployment_targets"], json!([]));
    }

    #[test]
    fn emit_answers_falls_back_to_runtime_public_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        let runtime_dir = bundle_root
            .join("state")
            .join("runtime")
            .join("demo.default");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::write(
            runtime_dir.join("endpoints.json"),
            r#"{"tenant":"demo","team":"default","public_base_url":"https://runtime.example.com"}"#,
        )
        .unwrap();

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "prod".into(),
            offline: false,
            verbose: false,
        });
        let request = SetupRequest {
            bundle: bundle_root.clone(),
            tenants: vec![TenantSelection {
                tenant: "demo".into(),
                team: Some("default".into()),
                allow_paths: Vec::new(),
            }],
            ..Default::default()
        };
        let plan = engine.plan(SetupMode::Create, &request, true).unwrap();
        let output = temp.path().join("answers-runtime.json");
        engine.emit_answers(&plan, &output, None, false).unwrap();
        let emitted: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(output).unwrap()).unwrap();
        assert_eq!(
            emitted["platform_setup"]["static_routes"]["public_base_url"],
            json!("https://runtime.example.com")
        );
    }

    #[test]
    fn execute_persists_static_routes_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: None,
            env: "prod".into(),
            offline: false,
            verbose: false,
        });
        let mut metadata = build_metadata(&empty_request(bundle_root.clone()), Vec::new(), vec![]);
        metadata.static_routes = StaticRoutesPolicy {
            public_web_enabled: true,
            public_base_url: Some("https://example.com".into()),
            public_surface_policy: "enabled".into(),
            default_route_prefix_policy: "pack_declared".into(),
            tenant_path_policy: "pack_declared".into(),
            ..StaticRoutesPolicy::default()
        };

        execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();
        let artifact = static_routes_artifact_path(&bundle_root);
        assert!(artifact.exists());
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(artifact).unwrap()).unwrap();
        assert_eq!(stored["public_web_enabled"], json!(true));
    }

    #[test]
    fn execute_create_persists_platform_metadata_without_provider_steps() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "prod".into(),
            offline: false,
            verbose: false,
        });
        let request = SetupRequest {
            bundle: bundle_root.clone(),
            static_routes: StaticRoutesPolicy {
                public_web_enabled: true,
                public_base_url: Some("https://example.com".into()),
                public_surface_policy: "enabled".into(),
                default_route_prefix_policy: "pack_declared".into(),
                tenant_path_policy: "pack_declared".into(),
                ..StaticRoutesPolicy::default()
            },
            deployment_targets: vec![crate::deployment_targets::DeploymentTargetRecord {
                target: "runtime".into(),
                provider_pack: None,
                default: Some(true),
            }],
            ..empty_request(bundle_root.clone())
        };

        let plan = engine.plan(SetupMode::Create, &request, false).unwrap();
        engine.execute(&plan).unwrap();

        let routes_artifact = static_routes_artifact_path(&bundle_root);
        assert!(routes_artifact.exists());

        let targets_artifact = bundle_root
            .join(".greentic")
            .join("deployment-targets.json");
        assert!(targets_artifact.exists());
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(targets_artifact).unwrap()).unwrap();
        assert_eq!(stored["targets"][0]["target"], json!("runtime"));
        assert_eq!(stored["targets"][0]["default"], json!(true));
    }

    #[test]
    fn remove_execute_deletes_provider_artifact_and_config_dir() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        let provider_dir = bundle_root.join("providers").join("messaging");
        std::fs::create_dir_all(&provider_dir).unwrap();
        let provider_pack = provider_dir.join("messaging-webchat.gtpack");
        std::fs::copy(
            bundle_root.join("packs").join("default.gtpack"),
            &provider_pack,
        )
        .unwrap();
        let config_dir = bundle_root
            .join("state")
            .join("config")
            .join("messaging-webchat");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("setup-answers.json"), "{}").unwrap();

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: None,
            env: "prod".into(),
            offline: false,
            verbose: false,
        });
        let request = SetupRequest {
            bundle: bundle_root.clone(),
            providers_remove: vec!["messaging-webchat".into()],
            ..Default::default()
        };
        let plan = engine.plan(SetupMode::Remove, &request, false).unwrap();
        engine.execute(&plan).unwrap();

        assert!(!provider_pack.exists());
        assert!(!config_dir.exists());
    }

    #[test]
    fn update_plan_preserves_static_routes_policy() {
        let req = SetupRequest {
            bundle: PathBuf::from("bundle"),
            tenants: vec![TenantSelection {
                tenant: "demo".into(),
                team: None,
                allow_paths: Vec::new(),
            }],
            static_routes: StaticRoutesPolicy {
                public_web_enabled: true,
                public_base_url: Some("https://example.com/new".into()),
                public_surface_policy: "enabled".into(),
                default_route_prefix_policy: "pack_declared".into(),
                tenant_path_policy: "pack_declared".into(),
                ..StaticRoutesPolicy::default()
            },
            ..Default::default()
        };
        let plan = apply_update(&req, true).unwrap();
        assert_eq!(
            plan.metadata.static_routes.public_base_url.as_deref(),
            Some("https://example.com/new")
        );
    }

    #[test]
    fn extract_default_from_help_parses_parenthesized() {
        let help = "Slack API base URL (default: https://slack.com/api)";
        let result = extract_default_from_help(help);
        assert_eq!(result, Some("https://slack.com/api".to_string()));
    }

    #[test]
    fn extract_default_from_help_parses_bracketed() {
        let help = "Enable feature [default: true]";
        let result = extract_default_from_help(help);
        assert_eq!(result, Some("true".to_string()));
    }

    #[test]
    fn extract_default_from_help_case_insensitive() {
        let help = "Some setting (Default: custom_value)";
        let result = extract_default_from_help(help);
        assert_eq!(result, Some("custom_value".to_string()));
    }

    #[test]
    fn extract_default_from_help_returns_none_without_default() {
        let help = "Just a plain help text with no default";
        let result = extract_default_from_help(help);
        assert_eq!(result, None);
    }

    #[test]
    fn infer_default_value_uses_explicit_default() {
        use crate::setup_input::SetupQuestion;
        let question = SetupQuestion {
            name: "api_base_url".to_string(),
            kind: "string".to_string(),
            required: true,
            help: Some("Some help (default: wrong_value)".to_string()),
            choices: vec![],
            default: Some(json!("https://explicit.com")),
            secret: false,
            title: None,
            visible_if: None,
            ..Default::default()
        };
        let result = infer_default_value(&question);
        assert_eq!(result, json!("https://explicit.com"));
    }

    #[test]
    fn infer_default_value_extracts_from_help() {
        use crate::setup_input::SetupQuestion;
        let question = SetupQuestion {
            name: "api_base_url".to_string(),
            kind: "string".to_string(),
            required: true,
            help: Some("Slack API base URL (default: https://slack.com/api)".to_string()),
            choices: vec![],
            default: None,
            secret: false,
            title: None,
            visible_if: None,
            ..Default::default()
        };
        let result = infer_default_value(&question);
        assert_eq!(result, json!("https://slack.com/api"));
    }

    #[test]
    fn infer_default_value_returns_empty_without_default() {
        use crate::setup_input::SetupQuestion;
        let question = SetupQuestion {
            name: "bot_token".to_string(),
            kind: "string".to_string(),
            required: true,
            help: Some("Your bot token".to_string()),
            choices: vec![],
            default: None,
            secret: true,
            title: None,
            visible_if: None,
            ..Default::default()
        };
        let result = infer_default_value(&question);
        assert_eq!(result, json!(""));
    }
}
