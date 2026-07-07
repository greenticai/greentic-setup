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
use crate::platform_setup::{
    persist_static_routes_artifact, persist_telemetry_artifact, persist_tunnel_artifact,
};

// Re-export types and functions for public API
pub use answers::{emit_answers, encrypt_secret_answers, load_answers, prompt_secret_answers};
pub use executors::{
    auto_install_provider_packs, domain_from_provider_id, execute_add_packs_to_bundle,
    execute_apply_pack_setup, execute_build_flow_index, execute_copy_resolved_manifests,
    execute_create_bundle, execute_remove_provider_artifacts, execute_resolve_packs,
    execute_validate_bundle, execute_write_gmap_rules, find_provider_pack_source,
    get_pack_target_dir, invoke_setup_component_operation,
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
            pending_setup_actions: Vec::new(),
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
                    if !plan.metadata.providers_remove.is_empty() {
                        report.provider_updates += execute_remove_provider_artifacts(
                            bundle,
                            &plan.metadata.providers_remove,
                        )?;
                    }
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
                    let setup_report =
                        execute_apply_pack_setup(bundle, &plan.metadata, &self.config)?;
                    report.provider_updates += setup_report.provider_updates;
                    report
                        .pending_setup_actions
                        .extend(setup_report.pending_setup_actions);
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
                SetupStepKind::BuildFlowIndex => {
                    execute_build_flow_index(bundle, &self.config)?;
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
        if let Some(tunnel) = plan.metadata.tunnel.as_ref() {
            let _ = persist_tunnel_artifact(bundle, tunnel);
        }
        if let Some(telemetry) = plan.metadata.telemetry.as_ref() {
            let _ = persist_telemetry_artifact(bundle, telemetry);
        }

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
    fn setup_actions_are_persisted_and_stripped_from_provider_config() {
        // Focused coverage for the setup-actions handling that
        // `execute_apply_pack_setup` performs before writing provider config:
        // an `oauth_install_button` answer is extracted into a pending action,
        // persisted to the per-provider actions state file, and removed from
        // the answers that get written as provider config.
        //
        // This deliberately exercises the `setup_actions` module directly
        // rather than the full `execute_apply_pack_setup` path: that path is
        // gated by the B12a fail-closed secret-classification contract (a pack
        // with no setup metadata is refused), which is covered by the unit
        // tests in `engine::executors`.
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().to_path_buf();

        let answers = json!({
            "bot_token": "secret",
            "setup_actions": [{
                "id": "install",
                "kind": "oauth_install_button",
                "label": "Add to Example",
                "authorize_url": "https://example.com/oauth"
            }]
        });

        let actions = crate::setup_actions::extract_setup_actions(
            "messaging-example",
            "demo",
            Some("default"),
            &answers,
        )
        .unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(
            actions[0].kind,
            crate::setup_actions::SetupActionKind::OauthInstallButton
        );

        crate::setup_actions::persist_setup_actions(&bundle_root, &actions).unwrap();
        let action_path = crate::setup_actions::setup_actions_state_path(
            &bundle_root,
            "demo",
            "default",
            "messaging-example",
        );
        assert!(action_path.exists());
        let state: crate::setup_actions::SetupActionStateFile =
            serde_json::from_str(&std::fs::read_to_string(&action_path).unwrap()).unwrap();
        assert_eq!(state.actions.len(), 1);
        assert_eq!(state.actions[0].id, "install");

        // Provider config keeps real answers but drops the setup-action payload.
        let persisted = crate::setup_actions::strip_setup_actions(&answers);
        assert!(persisted.get("setup_actions").is_none());
        assert_eq!(persisted["bot_token"], json!("secret"));
    }

    #[test]
    fn execute_apply_pack_setup_persists_pack_declared_setup_actions() {
        use std::io::Write;
        use zip::write::{FileOptions, ZipWriter};

        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        let providers_dir = bundle_root.join("providers/messaging");
        std::fs::create_dir_all(&providers_dir).unwrap();
        let pack_path = providers_dir.join("messaging-slack.gtpack");
        let file = std::fs::File::create(&pack_path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options).unwrap();
        writer
            .write_all(
                json!({
                    "pack_id": "messaging-slack",
                    "display_name": "Slack"
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        writer.start_file("assets/setup.yaml", options).unwrap();
        writer
            .write_all(
                br#"
title: Slack
questions: []
setup_actions:
  - id: add_to_slack
    label: Add to Slack
    kind: oauth_install_button
    provider_id: slack
    authorize_url: https://slack.example/install
"#,
            )
            .unwrap();
        writer.finish().unwrap();

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let request = empty_request(bundle_root.clone());
        let plan = engine.plan(SetupMode::Create, &request, false).unwrap();
        assert!(
            plan.steps
                .iter()
                .any(|step| step.kind == crate::plan::SetupStepKind::ApplyPackSetup),
            "pack-declared setup actions should schedule ApplyPackSetup"
        );
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        let report = execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();
        assert_eq!(report.pending_setup_actions.len(), 1);
        assert_eq!(report.pending_setup_actions[0].id, "add_to_slack");
        assert_eq!(report.pending_setup_actions[0].label, "Add to Slack");
        assert_eq!(
            report.pending_setup_actions[0].provider_id,
            "messaging-slack"
        );
        let action_path = crate::setup_actions::setup_actions_state_path(
            &bundle_root,
            "demo",
            "default",
            "messaging-slack",
        );
        assert!(action_path.exists());
    }

    #[test]
    fn execute_apply_pack_setup_hydrates_oauth_install_url_from_answers() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        // A pack with classifiable setup metadata so B12a can resolve a form
        // spec for `messaging-example`; the install action itself is
        // answer-provided, which is the behavior under test.
        write_registration_test_pack(
            &bundle_root,
            r#"
title: Example
questions:
  - name: workspace_name
    kind: string
"#,
            json!({"operations": {}}),
        );

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let mut request = empty_request(bundle_root.clone());
        request.setup_answers.insert(
            "messaging-example".into(),
            json!({
                "slack_client_id": "client-123",
                "setup_actions": [{
                    "id": "install",
                    "kind": "oauth_install_button",
                    "label": "Add",
                    "authorize_url": "https://slack.com/oauth/v2/authorize",
                    "client_id_field": "slack_client_id",
                    "scopes": ["chat:write", "channels:read"]
                }]
            }),
        );
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        let report = execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();
        let url = report.pending_setup_actions[0]
            .authorize_url
            .as_deref()
            .unwrap();
        assert!(url.contains("client_id=client-123"), "{url}");
        assert!(
            url.contains("scope=chat%3Awrite%2Cchannels%3Aread"),
            "{url}"
        );
    }

    #[test]
    fn execute_apply_pack_setup_runs_pack_declared_registration_before_oauth_hydration() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        write_registration_test_pack(
            &bundle_root,
            r#"
title: Example
questions:
  - name: workspace_name
    kind: string
setup_actions:
  - id: install
    label: Add
    kind: oauth_install_button
    authorize_url: https://example.com/oauth
    client_id_source: registration
    client_id_field: oauth_client_id
    registration:
      component_ref: components/registration.json
      op: register
      app_name_field: app_name
      client_id_output: registered_client_id
      client_secret_output: registered_client_secret
      app_id_output: registered_app_id
"#,
            json!({
                "operations": {
                    "register": {
                        "result": {
                            "registered_client_id": "client-from-registration",
                            "registered_client_secret": "secret-from-registration",
                            "registered_app_id": "app-from-registration"
                        }
                    }
                }
            }),
        );

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let mut request = empty_request(bundle_root.clone());
        request
            .setup_answers
            .insert("messaging-example".into(), json!({"app_name": "Demo App"}));
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        let report = execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();
        let url = report.pending_setup_actions[0]
            .authorize_url
            .as_deref()
            .unwrap();
        // The hydrated `client_id` proves the pack-declared registration ran
        // and produced the OAuth client id BEFORE the install URL was built —
        // the unique coverage of this test. Where those registration outputs
        // land (setup-answers.json vs the dev secrets store) is the B12a
        // redaction concern, covered by the `engine::executors` unit tests, so
        // we don't re-assert it here.
        assert!(url.contains("client_id=client-from-registration"), "{url}");
    }

    #[test]
    fn execute_apply_pack_setup_resolves_open_url_action_even_when_already_registered() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        write_registration_test_pack(
            &bundle_root,
            r#"
title: Example
questions:
  - name: workspace_name
    kind: string
setup_actions:
  - id: install
    label: Install
    kind: open_url
    url_template: "https://example.com/apps/{app_id}/install-on-team?"
    registration:
      component_ref: components/registration.json
      op: register
      app_id_output: registered_app_id
"#,
            json!({
                "operations": {
                    "register": {
                        // If registration re-ran on this pass it would return
                        // this app id instead of the pre-existing one below —
                        // asserting on the pre-existing value proves the op
                        // was NOT re-invoked.
                        "result": {
                            "registered_app_id": "app-from-a-fresh-registration-call"
                        }
                    }
                }
            }),
        );

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let mut request = empty_request(bundle_root.clone());
        // Simulate a prior successful setup run: the registration output is
        // already present in the persisted answers (as both the registration
        // mapping's source key and the generic `app_id` key merge_registration_output
        // writes), so a re-run should skip invoking `register` again.
        request.setup_answers.insert(
            "messaging-example".into(),
            json!({
                "workspace_name": "Demo App",
                "app_id": "preexisting-app-id",
                "registered_app_id": "preexisting-app-id"
            }),
        );
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        let report = execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();
        let url = report.pending_setup_actions[0]
            .extra
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap();
        assert_eq!(
            url,
            "https://example.com/apps/preexisting-app-id/install-on-team?"
        );
    }

    #[test]
    fn execute_apply_pack_setup_skips_actions_for_disabled_provider() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        write_registration_test_pack(
            &bundle_root,
            r#"
title: Example
questions: []
setup_actions:
  - id: install
    label: Add
    kind: oauth_install_button
    authorize_url: https://example.com/oauth
    client_id_source: registration
    client_id_field: oauth_client_id
    registration:
      component_ref: components/registration.json
      op: register
      client_id_output: registered_client_id
"#,
            json!({
                "operations": {
                    "register": {
                        "result": {
                            "registered_client_id": "client-from-registration"
                        }
                    }
                }
            }),
        );

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let mut request = empty_request(bundle_root.clone());
        request
            .setup_answers
            .insert("messaging-example".into(), json!({"enabled": false}));
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        let report = execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();

        assert!(report.pending_setup_actions.is_empty());
        assert!(
            !bundle_root
                .join("state/config/setup-actions/demo/default/messaging-example.json")
                .exists()
        );
        let setup_answers_path =
            bundle_root.join("state/config/messaging-example/setup-answers.json");
        let stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(setup_answers_path).unwrap()).unwrap();
        assert_eq!(stored["enabled"], json!(false));
    }

    #[test]
    fn execute_apply_pack_setup_uses_bundle_name_for_registration_app_name_template() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        write_registration_test_pack(
            &bundle_root,
            r#"
title: Example
questions:
  - name: workspace_name
    kind: string
setup_actions:
  - id: install
    label: Add
    kind: oauth_install_button
    authorize_url: https://example.com/oauth
    client_id_source: registration
    client_id_field: oauth_client_id
    app_name_template: "{{ bundle_name }} Slack"
    default_app_name: "Greentic Slack"
    registration:
      component_ref: components/registration.json
      op: register
      app_name_field: slack_app_name
      config_access_token_field: access_token
      client_id_output: app_name
      app_id_output: slack_app_name
"#,
            json!({
                "operations": {
                    "register": {
                        "echo_request": true
                    }
                }
            }),
        );

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let mut request = empty_request(bundle_root.clone());
        request.bundle_name = Some("Acme Support".into());
        request
            .setup_answers
            .insert("messaging-example".into(), json!({"access_token": "token"}));
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();

        // The registration echoes the templated app name into both
        // `app_name` and `slack_app_name`. Post-B12a these registration
        // outputs are persisted to the dev secrets store (every config value
        // is readable via the secrets API), not written back into
        // setup-answers.json, so assert against the store. `canonical_secret_uri`
        // collapses the literal "default" team into the `_` wildcard segment.
        use greentic_secrets_lib::SecretsStore as _;
        let store = crate::secrets::open_dev_store(&bundle_root).expect("open dev store");
        let rt = tokio::runtime::Runtime::new().unwrap();
        // setup uses the A4b `dev` -> `local` env alias for the secrets URI.
        let env = crate::resolve_env(Some("dev"));
        let read = |key: &str| -> String {
            let uri = crate::canonical_secret_uri(
                &env,
                "demo",
                Some("default"),
                "messaging-example",
                key,
            );
            let bytes = rt
                .block_on(async { store.get(&uri).await })
                .unwrap_or_else(|_| panic!("missing dev-store key: {key}"));
            String::from_utf8(bytes).expect("utf8")
        };
        assert_eq!(read("slack_app_name"), "Acme Support Slack");
        assert_eq!(read("app_name"), "Acme Support Slack");
    }

    #[test]
    fn execute_apply_pack_setup_registration_failure_does_not_persist_broken_action() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        write_registration_test_pack(
            &bundle_root,
            r#"
title: Example
questions: []
setup_actions:
  - id: install
    label: Add
    kind: oauth_install_button
    authorize_url: https://example.com/oauth
    client_id_source: registration
    registration:
      component_ref: components/registration.json
      op: register
      config_access_token_field: config_token
      client_id_output: client_id
"#,
            json!({"operations": {}}),
        );

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let mut request = empty_request(bundle_root.clone());
        request
            .setup_answers
            .insert("messaging-example".into(), json!({"config_token": "token"}));
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        let err = execute_apply_pack_setup(&bundle_root, &metadata, engine.config())
            .expect_err("registration failure should fail setup");
        assert!(
            err.to_string()
                .contains("failed to run setup action registration"),
            "{err:#}"
        );
        let action_path = crate::setup_actions::setup_actions_state_path(
            &bundle_root,
            "demo",
            "default",
            "messaging-example",
        );
        assert!(!action_path.exists());
    }

    #[test]
    fn execute_apply_pack_setup_registration_passes_original_input_field_names() {
        let temp = tempfile::tempdir().unwrap();
        let bundle_root = temp.path().join("bundle");
        bundle::create_demo_bundle_structure(&bundle_root, Some("demo")).unwrap();
        write_registration_test_pack(
            &bundle_root,
            r#"
title: Example
questions:
  - name: workspace_name
    kind: string
setup_actions:
  - id: install
    label: Add
    kind: oauth_install_button
    authorize_url: https://example.com/oauth
    client_id_source: registration
    registration:
      component_ref: components/registration.json
      op: register
      config_access_token_field: provider_specific_token
      client_id_output: provider_specific_token
"#,
            json!({
                "operations": {
                    "register": {
                        "echo_request": true
                    }
                }
            }),
        );

        let engine = SetupEngine::new(SetupConfig {
            tenant: "demo".into(),
            team: Some("default".into()),
            env: "dev".into(),
            offline: false,
            verbose: false,
        });
        let mut request = empty_request(bundle_root.clone());
        request.setup_answers.insert(
            "messaging-example".into(),
            json!({"provider_specific_token": "client-from-original-field"}),
        );
        let metadata = build_metadata(&request, Vec::new(), vec![]);

        let report = execute_apply_pack_setup(&bundle_root, &metadata, engine.config()).unwrap();
        let url = report.pending_setup_actions[0]
            .authorize_url
            .as_deref()
            .unwrap();
        assert!(
            url.contains("client_id=client-from-original-field"),
            "{url}"
        );
    }

    fn write_registration_test_pack(
        bundle_root: &std::path::Path,
        setup_yaml: &str,
        registration_component: serde_json::Value,
    ) {
        use std::io::Write;
        use zip::write::{FileOptions, ZipWriter};

        let providers_dir = bundle_root.join("providers/messaging");
        std::fs::create_dir_all(&providers_dir).unwrap();
        let pack_path = providers_dir.join("messaging-example.gtpack");
        let file = std::fs::File::create(&pack_path).unwrap();
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options).unwrap();
        writer
            .write_all(
                json!({
                    "pack_id": "messaging-example",
                    "display_name": "Example"
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        writer.start_file("assets/setup.yaml", options).unwrap();
        writer.write_all(setup_yaml.as_bytes()).unwrap();
        writer
            .start_file("components/registration.json", options)
            .unwrap();
        writer
            .write_all(registration_component.to_string().as_bytes())
            .unwrap();
        writer.finish().unwrap();
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
