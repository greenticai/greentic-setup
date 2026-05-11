//! Read-only diagnostics for greentic-setup bundles.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use crate::bundle::{self, BUNDLE_LOCK_FILE, BUNDLE_WORKSPACE_MARKER};
use crate::{capabilities, config_envelope, discovery, platform_setup, setup_to_formspec};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoctorStage {
    Setup,
    Cache,
    Locks,
    Answers,
    Runtime,
    Routes,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticSeverity {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    pub check_id: String,
    pub severity: DiagnosticSeverity,
    pub component: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_pack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_component: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DoctorReport {
    pub bundle: String,
    pub status: String,
    pub error_count: usize,
    pub warn_count: usize,
    pub info_count: usize,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn run_doctor(bundle: &Path, stage: Option<DoctorStage>) -> DoctorReport {
    let mut ctx = DoctorContext::new(bundle.to_path_buf(), stage);
    ctx.run();
    ctx.into_report()
}

struct DoctorContext {
    bundle: PathBuf,
    stage: Option<DoctorStage>,
    diagnostics: Vec<Diagnostic>,
}

impl DoctorContext {
    fn new(bundle: PathBuf, stage: Option<DoctorStage>) -> Self {
        Self {
            bundle,
            stage,
            diagnostics: Vec::new(),
        }
    }

    fn run(&mut self) {
        self.check_bundle_root();
        if !self.bundle.is_dir() || !bundle::is_bundle_root(&self.bundle) {
            return;
        }

        if self.includes(DoctorStage::Setup) {
            self.check_workspace_manifest();
            self.check_discovery_and_packs();
            self.check_provider_registry();
        }
        if self.includes(DoctorStage::Locks) {
            self.check_bundle_lock();
        }
        if self.includes(DoctorStage::Answers) {
            self.check_setup_outputs();
        }
        if self.includes(DoctorStage::Routes) {
            self.check_route_artifacts();
            self.check_resolved_manifests();
        }
        if self.includes(DoctorStage::Runtime) {
            self.check_runtime_artifacts();
        }
        if self.includes(DoctorStage::Cache) {
            self.push(
                "setup.cache.model",
                DiagnosticSeverity::Info,
                "cache",
                "bundle lock records local bundle references and digests, but not enough OCI cache provenance to validate remote cache freshness",
            )
            .fix_hint("extend bundle.lock.json with source_ref, resolved version, OCI digest, and cache path to enable cache doctor checks")
            .finish();
        }
    }

    fn into_report(self) -> DoctorReport {
        let error_count = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == DiagnosticSeverity::Error)
            .count();
        let warn_count = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == DiagnosticSeverity::Warn)
            .count();
        let info_count = self
            .diagnostics
            .iter()
            .filter(|d| d.severity == DiagnosticSeverity::Info)
            .count();
        let status = if error_count > 0 {
            "error"
        } else if warn_count > 0 {
            "warn"
        } else {
            "ok"
        }
        .to_string();
        DoctorReport {
            bundle: self.bundle.display().to_string(),
            status,
            error_count,
            warn_count,
            info_count,
            diagnostics: self.diagnostics,
        }
    }

    fn includes(&self, stage: DoctorStage) -> bool {
        self.stage.is_none_or(|value| value == stage)
    }

    fn check_bundle_root(&mut self) {
        if !self.bundle.exists() {
            let bundle_path = self.bundle.display().to_string();
            self.push(
                "setup.bundle.exists",
                DiagnosticSeverity::Error,
                "setup",
                "bundle path does not exist",
            )
            .expected("existing bundle directory")
            .actual(bundle_path.clone())
            .fix_hint(
                "check the bundle path or extract the .gtbundle archive before running doctor",
            )
            .file(bundle_path)
            .finish();
            return;
        }
        if !self.bundle.is_dir() {
            let bundle_path = self.bundle.display().to_string();
            self.push(
                "setup.bundle.directory",
                DiagnosticSeverity::Error,
                "setup",
                "doctor currently validates extracted bundle directories",
            )
            .expected("directory containing bundle.yaml or greentic.demo.yaml")
            .actual(bundle_path.clone())
            .fix_hint("run setup on the .gtbundle first or extract it to a directory")
            .file(bundle_path)
            .finish();
            return;
        }
        if !bundle::is_bundle_root(&self.bundle) {
            let bundle_path = self.bundle.display().to_string();
            self.push(
                "setup.bundle.marker",
                DiagnosticSeverity::Error,
                "setup",
                "bundle root marker is missing",
            )
            .expected(format!(
                "{} or {}",
                BUNDLE_WORKSPACE_MARKER,
                bundle::LEGACY_BUNDLE_MARKER
            ))
            .fix_hint("run greentic-setup bundle init or point doctor at the bundle root")
            .file(bundle_path)
            .finish();
            return;
        }
        let bundle_path = self.bundle.display().to_string();
        self.push(
            "setup.bundle.marker",
            DiagnosticSeverity::Info,
            "setup",
            "bundle root marker found",
        )
        .file(bundle_path)
        .finish();
    }

    fn check_workspace_manifest(&mut self) {
        let path = self.bundle.join(BUNDLE_WORKSPACE_MARKER);
        if !path.exists() {
            self.push(
                "setup.bundle_manifest.present",
                DiagnosticSeverity::Warn,
                "setup",
                "bundle.yaml is missing; legacy bundles are accepted but cannot be fully lock-checked",
            )
            .expected(BUNDLE_WORKSPACE_MARKER)
            .fix_hint("run greentic-setup bundle build or setup to materialize normalized bundle metadata")
            .file(path.display().to_string())
            .finish();
            return;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            self.push(
                "setup.bundle_manifest.read",
                DiagnosticSeverity::Error,
                "setup",
                "failed to read bundle.yaml",
            )
            .file(path.display().to_string())
            .finish();
            return;
        };
        let parsed = serde_yaml_bw::from_str::<serde_yaml_bw::Value>(&raw);
        let Ok(doc) = parsed else {
            self.push(
                "setup.bundle_manifest.parse",
                DiagnosticSeverity::Error,
                "setup",
                "failed to parse bundle.yaml",
            )
            .file(path.display().to_string())
            .fix_hint("fix YAML syntax before rerunning setup")
            .finish();
            return;
        };
        let Some(map) = doc.as_mapping() else {
            self.push(
                "setup.bundle_manifest.schema",
                DiagnosticSeverity::Error,
                "setup",
                "bundle.yaml must be a YAML object",
            )
            .file(path.display().to_string())
            .finish();
            return;
        };
        if yaml_get(map, "schema_version").is_none() {
            self.push(
                "setup.bundle_manifest.schema_version",
                DiagnosticSeverity::Warn,
                "setup",
                "bundle.yaml does not declare schema_version",
            )
            .expected("schema_version: 1")
            .file(path.display().to_string())
            .finish();
        }
        for key in ["app_packs", "extension_providers"] {
            for reference in yaml_string_list(map, key) {
                self.check_bundle_reference_path(&reference, key, &path);
            }
        }
    }

    fn check_bundle_reference_path(&mut self, reference: &str, key: &str, manifest_path: &Path) {
        if reference.contains('\\')
            || reference.contains("..")
            || !is_remote_reference(reference) && Path::new(reference).is_absolute()
        {
            self.push(
                "setup.bundle_manifest.reference_path",
                DiagnosticSeverity::Error,
                "setup",
                "bundle reference is not a deterministic relative path",
            )
            .expected("relative path without backslashes or parent traversal")
            .actual(reference.to_string())
            .evidence(key.to_string())
            .fix_hint("rewrite bundle.yaml references relative to the bundle root")
            .file(manifest_path.display().to_string())
            .finish();
        }
        if reference.contains(":latest")
            || reference.ends_with("@latest")
            || reference.contains("/latest/")
        {
            self.push(
                "setup.bundle_manifest.latest_ref",
                DiagnosticSeverity::Warn,
                "lock",
                "bundle reference appears to use a moving latest tag",
            )
            .expected("exact version or digest-pinned reference")
            .actual(reference.to_string())
            .file(manifest_path.display().to_string())
            .finish();
        }
        if is_remote_reference(reference) {
            if materialized_pack_candidates(&self.bundle, reference)
                .iter()
                .any(|path| path.exists())
            {
                return;
            }
            self.push(
                "setup.bundle_manifest.remote_materialized",
                DiagnosticSeverity::Warn,
                "setup",
                "bundle manifest uses a remote pack reference but no matching local pack artifact was found",
            )
            .expected("resolved .gtpack copied into packs/ or providers/<domain>/")
            .actual(reference.to_string())
            .fix_hint("rerun setup or resolve the remote pack before starting the bundle")
            .file(manifest_path.display().to_string())
            .finish();
            return;
        }
        let path = self.bundle.join(reference);
        if !path.exists() {
            self.push(
                "setup.bundle_manifest.reference_exists",
                DiagnosticSeverity::Error,
                "setup",
                "bundle manifest references a missing pack",
            )
            .expected("referenced .gtpack exists")
            .actual(reference.to_string())
            .fix_hint("rerun setup or update bundle.yaml to match the files present in the bundle")
            .file(path.display().to_string())
            .finish();
        }
    }

    fn check_discovery_and_packs(&mut self) {
        let discovered = match discovery::discover(&self.bundle) {
            Ok(value) => value,
            Err(err) => {
                self.push(
                    "setup.pack_discovery",
                    DiagnosticSeverity::Error,
                    "setup",
                    "pack discovery failed",
                )
                .evidence(err.to_string())
                .fix_hint("inspect provider and packs directories for unreadable or corrupt .gtpack files")
                .finish();
                return;
            }
        };

        let targets = discovered.setup_targets();
        if targets.is_empty() {
            self.push(
                "setup.pack_discovery.empty",
                DiagnosticSeverity::Warn,
                "setup",
                "no setup-capable packs were discovered",
            )
            .fix_hint("add app packs under packs/ or provider packs under providers/<domain>/")
            .finish();
        }

        for provider in targets {
            if provider.id_source == discovery::ProviderIdSource::Filename {
                self.push(
                    "setup.pack_manifest.pack_id",
                    DiagnosticSeverity::Warn,
                    "setup",
                    "pack did not expose a readable pack_id; filename fallback was used",
                )
                .file(provider.pack_path.display().to_string())
                .pack(provider.provider_id.clone())
                .fix_hint("rebuild the pack with pack_id in manifest.cbor or pack.manifest.json")
                .finish();
            }
            if discovery::read_pack_meta(&provider.pack_path).is_err() {
                self.push(
                    "setup.pack_manifest.read",
                    DiagnosticSeverity::Error,
                    "setup",
                    "pack manifest could not be read",
                )
                .file(provider.pack_path.display().to_string())
                .pack(provider.provider_id.clone())
                .fix_hint("rebuild or replace the .gtpack")
                .finish();
            }
            if provider.kind == discovery::DetectedPackKind::Provider
                && !capabilities::has_capabilities_extension(&provider.pack_path)
            {
                self.push(
                    "setup.pack_capabilities.extension",
                    DiagnosticSeverity::Warn,
                    "setup",
                    "provider pack is missing greentic.ext.capabilities.v1",
                )
                .file(provider.pack_path.display().to_string())
                .pack(provider.provider_id.clone())
                .fix_hint("replace this provider pack with a newer build that includes the capabilities extension")
                .finish();
            }
            if setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id)
                .is_some()
            {
                self.push(
                    "setup.schema.available",
                    DiagnosticSeverity::Info,
                    "answers",
                    "setup schema is available for pack",
                )
                .file(provider.pack_path.display().to_string())
                .pack(provider.provider_id.clone())
                .finish();
            }
        }
    }

    fn check_bundle_lock(&mut self) {
        let lock_path = self.bundle.join(BUNDLE_LOCK_FILE);
        if !lock_path.exists() {
            self.push(
                "setup.lock.present",
                DiagnosticSeverity::Warn,
                "lock",
                "bundle.lock.json is missing",
            )
            .expected(BUNDLE_LOCK_FILE)
            .fix_hint("rerun greentic-setup so bundle metadata and lock state are synchronized")
            .file(lock_path.display().to_string())
            .finish();
            return;
        }
        let lock = match read_json(&lock_path) {
            Ok(value) => value,
            Err(err) => {
                self.push(
                    "setup.lock.parse",
                    DiagnosticSeverity::Error,
                    "lock",
                    "bundle.lock.json is not valid JSON",
                )
                .evidence(err.to_string())
                .file(lock_path.display().to_string())
                .finish();
                return;
            }
        };

        if lock.get("build_format_version").and_then(JsonValue::as_str) != Some("bundle-lock-v1") {
            self.push(
                "setup.lock.format_version",
                DiagnosticSeverity::Warn,
                "lock",
                "bundle lock format is missing or unexpected",
            )
            .expected("bundle-lock-v1")
            .actual(
                lock.get("build_format_version")
                    .map(JsonValue::to_string)
                    .unwrap_or_else(|| "null".to_string()),
            )
            .file(lock_path.display().to_string())
            .finish();
        }

        let refs = workspace_refs(&self.bundle).unwrap_or_default();
        let lock_refs = lock_reference_map(&lock);
        for reference in &refs {
            match lock_refs.get(reference) {
                Some(Some(expected_digest)) => {
                    let path = self.bundle.join(reference);
                    if path.exists() {
                        match sha256_file(&path) {
                            Ok(actual_digest) if &actual_digest == expected_digest => {}
                            Ok(actual_digest) => self
                                .push(
                                    "setup.lock.digest_match",
                                    DiagnosticSeverity::Error,
                                    "lock",
                                    "pack digest does not match bundle.lock.json",
                                )
                                .expected(expected_digest.clone())
                                .actual(actual_digest)
                                .file(path.display().to_string())
                                .pack(reference.clone())
                                .fix_hint("replace the pack with the locked artifact or regenerate the lock intentionally")
                                .finish(),
                            Err(err) => self
                                .push(
                                    "setup.lock.digest_read",
                                    DiagnosticSeverity::Error,
                                    "lock",
                                    "failed to compute pack digest",
                                )
                                .evidence(err.to_string())
                                .file(path.display().to_string())
                                .pack(reference.clone())
                                .finish(),
                        }
                    }
                }
                Some(None) => {
                    if is_stable_reference(reference)
                        && materialized_pack_candidates(&self.bundle, reference)
                            .iter()
                            .any(|path| path.exists())
                    {
                        continue;
                    }
                    self.push(
                        "setup.lock.digest_present",
                        DiagnosticSeverity::Warn,
                        "lock",
                        "lock entry has no digest",
                    )
                    .file(lock_path.display().to_string())
                    .pack(reference.clone())
                    .fix_hint("rerun setup with a resolver that records content digests")
                    .finish();
                }
                None => self
                    .push(
                        "setup.lock.reference_present",
                        DiagnosticSeverity::Error,
                        "lock",
                        "bundle.yaml reference is missing from bundle.lock.json",
                    )
                    .expected(reference.clone())
                    .file(lock_path.display().to_string())
                    .fix_hint("rerun setup to synchronize bundle.yaml and bundle.lock.json")
                    .finish(),
            }
        }
        for reference in lock_refs.keys() {
            if !refs.contains(reference) {
                self.push(
                    "setup.lock.stale_reference",
                    DiagnosticSeverity::Warn,
                    "lock",
                    "bundle.lock.json contains a reference not present in bundle.yaml",
                )
                .actual(reference.clone())
                .file(lock_path.display().to_string())
                .fix_hint("rerun setup or remove stale lock entries")
                .finish();
            }
        }
    }

    fn check_setup_outputs(&mut self) {
        let discovered = match discovery::discover(&self.bundle) {
            Ok(value) => value,
            Err(_) => return,
        };
        for provider in discovered.setup_targets() {
            let config_path = self
                .bundle
                .join("state")
                .join("config")
                .join(&provider.provider_id)
                .join("setup-answers.json");
            let form_spec =
                setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id);
            if !config_path.exists() {
                if form_spec
                    .as_ref()
                    .is_some_and(|spec| spec.questions.iter().any(|q| q.required))
                {
                    self.push(
                        "setup.answers.present",
                        DiagnosticSeverity::Error,
                        "answers",
                        "required setup answers have not been materialized",
                    )
                    .file(config_path.display().to_string())
                    .pack(provider.provider_id.clone())
                    .fix_hint("run greentic-setup with complete answers for this provider")
                    .finish();
                }
                continue;
            }
            let answers = match read_json(&config_path) {
                Ok(value) => value,
                Err(err) => {
                    self.push(
                        "setup.answers.parse",
                        DiagnosticSeverity::Error,
                        "answers",
                        "setup-answers.json is not valid JSON",
                    )
                    .evidence(err.to_string())
                    .file(config_path.display().to_string())
                    .pack(provider.provider_id.clone())
                    .finish();
                    continue;
                }
            };
            if let Some(spec) = form_spec {
                if let Some(answer_map) = answers.as_object() {
                    let tunnel_supplies_public_base_url =
                        tunnel_supplies_public_base_url(&self.bundle);
                    for question in spec.questions.iter().filter(|q| q.required) {
                        if question.id == "public_base_url" && tunnel_supplies_public_base_url {
                            continue;
                        }
                        let missing = answer_map
                            .get(&question.id)
                            .is_none_or(|value| value.is_null() || value.as_str() == Some(""))
                            && question
                                .default_value
                                .as_ref()
                                .is_none_or(|value| value.trim().is_empty());
                        if missing {
                            self.push(
                                "setup.answers.required",
                                DiagnosticSeverity::Error,
                                "answers",
                                "required setup answer is missing or empty",
                            )
                            .expected(question.id.clone())
                            .file(config_path.display().to_string())
                            .pack(provider.provider_id.clone())
                            .fix_hint("provide the missing answer and rerun greentic-setup")
                            .finish();
                        }
                    }
                } else {
                    self.push(
                        "setup.answers.object",
                        DiagnosticSeverity::Error,
                        "answers",
                        "setup answers must be a JSON object",
                    )
                    .file(config_path.display().to_string())
                    .pack(provider.provider_id.clone())
                    .finish();
                }
            }

            match config_envelope::read_provider_config_envelope(
                &self.bundle.join(".providers"),
                &provider.provider_id,
            ) {
                Ok(Some(_)) => {
                    let envelope_path = self
                        .bundle
                        .join(".providers")
                        .join(&provider.provider_id)
                        .join("config.envelope.cbor")
                        .display()
                        .to_string();
                    if let Err(err) = config_envelope::ensure_contract_compatible(
                        &self.bundle.join(".providers"),
                        &provider.provider_id,
                        "setup-input",
                        &provider.pack_path,
                        false,
                    ) {
                        self.push(
                            "setup.config_envelope.contract",
                            DiagnosticSeverity::Error,
                            "provider",
                            "provider config envelope no longer matches the current pack contract",
                        )
                        .evidence(err.to_string())
                        .file(envelope_path)
                        .pack(provider.provider_id.clone())
                        .fix_hint(
                            "rerun setup after intentionally accepting the pack version change",
                        )
                        .finish();
                    }
                }
                Ok(None) => {
                    let envelope_path = self
                        .bundle
                        .join(".providers")
                        .join(&provider.provider_id)
                        .join("config.envelope.cbor")
                        .display()
                        .to_string();
                    self.push(
                        "setup.config_envelope.present",
                        DiagnosticSeverity::Warn,
                        "provider",
                        "setup answers exist but provider config envelope is missing",
                    )
                    .file(envelope_path)
                    .pack(provider.provider_id.clone())
                    .fix_hint(
                        "rerun greentic-setup so runtime-readable provider config is materialized",
                    )
                    .finish();
                }
                Err(err) => self
                    .push(
                        "setup.config_envelope.parse",
                        DiagnosticSeverity::Error,
                        "provider",
                        "provider config envelope could not be read",
                    )
                    .evidence(err.to_string())
                    .pack(provider.provider_id.clone())
                    .finish(),
            }
        }
    }

    fn check_route_artifacts(&mut self) {
        let path = platform_setup::static_routes_artifact_path(&self.bundle);
        if path.exists() {
            match platform_setup::load_static_routes_artifact(&self.bundle) {
                Ok(_) => {}
                Err(err) => self
                    .push(
                        "setup.routes.static_routes_parse",
                        DiagnosticSeverity::Error,
                        "routes",
                        "static routes artifact could not be parsed",
                    )
                    .evidence(err.to_string())
                    .file(path.display().to_string())
                    .fix_hint("rerun setup with valid platform_setup.static_routes answers")
                    .finish(),
            }
        } else {
            self.push(
                "setup.routes.static_routes_present",
                DiagnosticSeverity::Warn,
                "routes",
                "static routes artifact is missing",
            )
            .file(path.display().to_string())
            .fix_hint("rerun setup; setup should persist state/config/platform/static-routes.json")
            .finish();
        }

        let tunnel_path = platform_setup::tunnel_artifact_path(&self.bundle);
        if tunnel_path.exists()
            && let Err(err) = platform_setup::load_tunnel_artifact(&self.bundle)
        {
            self.push(
                "setup.routes.tunnel_parse",
                DiagnosticSeverity::Error,
                "routes",
                "tunnel artifact could not be parsed",
            )
            .evidence(err.to_string())
            .file(tunnel_path.display().to_string())
            .finish();
        }
    }

    fn check_resolved_manifests(&mut self) {
        for (tenant, team, gmap) in discover_gmap_targets(&self.bundle) {
            if is_forbidden_only_gmap(&gmap) {
                continue;
            }
            let filename = bundle::resolved_manifest_filename(&tenant, team.as_deref());
            let path = self.bundle.join("resolved").join(filename);
            if !path.exists() {
                self.push(
                    "setup.routes.resolved_manifest_present",
                    DiagnosticSeverity::Warn,
                    "routes",
                    "tenant/team gmap exists but matching resolved manifest is missing",
                )
                .expected(path.display().to_string())
                .file(gmap.display().to_string())
                .fix_hint("rerun setup to copy or regenerate resolved manifests")
                .finish();
            } else if std::fs::read_to_string(&path)
                .is_ok_and(|raw| raw.trim() == "# Resolved manifest placeholder")
            {
                self.push(
                    "setup.routes.resolved_manifest_placeholder",
                    DiagnosticSeverity::Warn,
                    "routes",
                    "resolved manifest is still the setup placeholder",
                )
                .file(path.display().to_string())
                .fix_hint("run the resolver pipeline before start if this bundle needs concrete resolved manifests")
                .finish();
            }
        }
    }

    fn check_runtime_artifacts(&mut self) {
        let runtime = self.bundle.join("state").join("runtime");
        if !runtime.exists() {
            self.push(
                "setup.runtime.state_present",
                DiagnosticSeverity::Info,
                "runtime",
                "runtime state directory has not been created yet",
            )
            .file(runtime.display().to_string())
            .finish();
        }
    }

    fn check_provider_registry(&mut self) {
        let path = self.bundle.join("providers").join("providers.json");
        if path.exists()
            && let Err(err) = bundle::load_provider_registry(&self.bundle)
        {
            self.push(
                "setup.provider_registry.parse",
                DiagnosticSeverity::Error,
                "provider",
                "providers/providers.json could not be parsed",
            )
            .evidence(err.to_string())
            .file(path.display().to_string())
            .finish();
        }
    }

    fn push(
        &mut self,
        check_id: impl Into<String>,
        severity: DiagnosticSeverity,
        component: impl Into<String>,
        message: impl Into<String>,
    ) -> DiagnosticBuilder<'_> {
        DiagnosticBuilder {
            ctx: self,
            diagnostic: Diagnostic {
                check_id: check_id.into(),
                severity,
                component: component.into(),
                message: message.into(),
                evidence: None,
                expected: None,
                actual: None,
                fix_hint: None,
                related_file: None,
                related_pack: None,
                related_component: None,
            },
        }
    }
}

struct DiagnosticBuilder<'a> {
    ctx: &'a mut DoctorContext,
    diagnostic: Diagnostic,
}

impl DiagnosticBuilder<'_> {
    fn evidence(mut self, value: impl Into<String>) -> Self {
        self.diagnostic.evidence = Some(value.into());
        self
    }
    fn expected(mut self, value: impl Into<String>) -> Self {
        self.diagnostic.expected = Some(value.into());
        self
    }
    fn actual(mut self, value: impl Into<String>) -> Self {
        self.diagnostic.actual = Some(value.into());
        self
    }
    fn fix_hint(mut self, value: impl Into<String>) -> Self {
        self.diagnostic.fix_hint = Some(value.into());
        self
    }
    fn file(mut self, value: impl Into<String>) -> Self {
        self.diagnostic.related_file = Some(value.into());
        self
    }
    fn pack(mut self, value: impl Into<String>) -> Self {
        self.diagnostic.related_pack = Some(value.into());
        self
    }
    fn finish(self) {
        self.ctx.diagnostics.push(self.diagnostic);
    }
}

fn read_json(path: &Path) -> anyhow::Result<JsonValue> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let digest = Sha256::digest(bytes);
    let encoded = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("sha256:{encoded}"))
}

fn workspace_refs(bundle: &Path) -> anyhow::Result<BTreeSet<String>> {
    let path = bundle.join(BUNDLE_WORKSPACE_MARKER);
    let raw = std::fs::read_to_string(&path)?;
    let doc = serde_yaml_bw::from_str::<serde_yaml_bw::Value>(&raw)?;
    let mut refs = BTreeSet::new();
    if let Some(map) = doc.as_mapping() {
        for key in ["app_packs", "extension_providers"] {
            refs.extend(yaml_string_list(map, key));
        }
    }
    Ok(refs)
}

fn lock_reference_map(lock: &JsonValue) -> BTreeMap<String, Option<String>> {
    let mut refs = BTreeMap::new();
    for key in ["app_packs", "extension_providers"] {
        if let Some(entries) = lock.get(key).and_then(JsonValue::as_array) {
            for entry in entries {
                if let Some(reference) = entry.get("reference").and_then(JsonValue::as_str) {
                    refs.insert(
                        reference.to_string(),
                        entry
                            .get("digest")
                            .and_then(JsonValue::as_str)
                            .map(ToOwned::to_owned),
                    );
                }
            }
        }
    }
    refs
}

fn is_remote_reference(reference: &str) -> bool {
    reference.starts_with("http://")
        || reference.starts_with("https://")
        || reference.starts_with("oci://")
}

fn is_stable_reference(reference: &str) -> bool {
    reference.ends_with(":stable")
}

fn materialized_pack_candidates(bundle: &Path, reference: &str) -> Vec<PathBuf> {
    if reference.starts_with("oci://") {
        let Some(pack_id) = reference
            .rsplit('/')
            .next()
            .and_then(|value| value.split(':').next())
            .filter(|value| !value.is_empty())
        else {
            return Vec::new();
        };
        let filename = format!("{pack_id}.gtpack");
        return vec![
            bundle.join("packs").join(&filename),
            bundle
                .join("providers")
                .join(crate::engine::domain_from_provider_id(pack_id))
                .join(&filename),
        ];
    }

    let Some(filename) = reference
        .rsplit('/')
        .next()
        .filter(|value| value.ends_with(".gtpack"))
    else {
        return Vec::new();
    };
    vec![
        bundle.join("packs").join(filename),
        bundle.join("providers").join("messaging").join(filename),
        bundle.join("providers").join("events").join(filename),
        bundle.join("providers").join("oauth").join(filename),
        bundle.join("providers").join("secrets").join(filename),
        bundle.join("providers").join("state").join(filename),
    ]
}

fn tunnel_supplies_public_base_url(bundle: &Path) -> bool {
    platform_setup::load_tunnel_artifact(bundle)
        .ok()
        .flatten()
        .and_then(|answers| answers.mode)
        .is_some_and(|mode| matches!(mode.as_str(), "cloudflared" | "ngrok"))
}

fn is_forbidden_only_gmap(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|raw| {
            raw.lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .all(|line| line == "_ = forbidden")
        })
        .unwrap_or(false)
}

fn yaml_get<'a>(map: &'a serde_yaml_bw::Mapping, key: &str) -> Option<&'a serde_yaml_bw::Value> {
    map.get(serde_yaml_bw::Value::String(key.to_string(), None))
}

fn yaml_string_list(map: &serde_yaml_bw::Mapping, key: &str) -> Vec<String> {
    yaml_get(map, key)
        .and_then(serde_yaml_bw::Value::as_sequence)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_yaml_bw::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn discover_gmap_targets(bundle: &Path) -> Vec<(String, Option<String>, PathBuf)> {
    let tenants_dir = bundle.join("tenants");
    let mut targets = Vec::new();
    let Ok(tenants) = std::fs::read_dir(&tenants_dir) else {
        return targets;
    };
    for tenant in tenants.flatten() {
        if !tenant.path().is_dir() {
            continue;
        }
        let Some(tenant_name) = tenant.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let tenant_gmap = tenant.path().join("tenant.gmap");
        if tenant_gmap.exists() {
            targets.push((tenant_name.clone(), None, tenant_gmap));
        }
        let teams_dir = tenant.path().join("teams");
        let Ok(teams) = std::fs::read_dir(teams_dir) else {
            continue;
        };
        for team in teams.flatten() {
            if !team.path().is_dir() {
                continue;
            }
            let Some(team_name) = team.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            let team_gmap = team.path().join("team.gmap");
            if team_gmap.exists() {
                targets.push((tenant_name.clone(), Some(team_name), team_gmap));
            }
        }
    }
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn ids(report: &DoctorReport) -> BTreeSet<&str> {
        report
            .diagnostics
            .iter()
            .map(|d| d.check_id.as_str())
            .collect()
    }

    #[test]
    fn missing_bundle_reports_error() {
        let report = run_doctor(Path::new("/definitely/missing/greentic-bundle"), None);
        assert_eq!(report.error_count, 1);
        assert_eq!(report.status, "error");
    }

    #[test]
    fn new_bundle_reports_lock_and_setup_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        bundle::create_demo_bundle_structure(&root, Some("demo")).unwrap();

        let report = run_doctor(&root, None);
        assert!(report.error_count == 0, "{:#?}", report.diagnostics);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.check_id == "setup.bundle.marker")
        );
    }

    #[test]
    fn file_and_unmarked_directory_are_rejected_before_stage_checks() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("bundle.gtbundle");
        write(&file, "not a directory");
        let report = run_doctor(&file, Some(DoctorStage::Cache));
        assert_eq!(report.status, "error");
        assert!(ids(&report).contains("setup.bundle.directory"));

        let dir = temp.path().join("plain-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let report = run_doctor(&dir, Some(DoctorStage::Cache));
        assert!(ids(&report).contains("setup.bundle.marker"));
        assert_eq!(report.error_count, 1);
        assert_eq!(report.warn_count, 0);
    }

    #[test]
    fn setup_stage_reports_manifest_parse_schema_and_reference_issues() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        write(
            &root.join(BUNDLE_WORKSPACE_MARKER),
            "app_packs:\n  - ../bad.gtpack\n",
        );
        let report = run_doctor(&root, Some(DoctorStage::Setup));
        let check_ids = ids(&report);
        assert!(check_ids.contains("setup.bundle_manifest.schema_version"));
        assert!(check_ids.contains("setup.bundle_manifest.reference_path"));
        assert!(check_ids.contains("setup.bundle_manifest.reference_exists"));
        assert!(check_ids.contains("setup.pack_discovery.empty"));

        write(&root.join(BUNDLE_WORKSPACE_MARKER), ":\n");
        let report = run_doctor(&root, Some(DoctorStage::Setup));
        assert!(ids(&report).contains("setup.bundle_manifest.parse"));

        write(&root.join(BUNDLE_WORKSPACE_MARKER), "- not-an-object\n");
        let report = run_doctor(&root, Some(DoctorStage::Setup));
        assert!(ids(&report).contains("setup.bundle_manifest.schema"));
    }

    #[test]
    fn setup_stage_reports_remote_latest_registry_and_missing_materialization() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        write(
            &root.join(BUNDLE_WORKSPACE_MARKER),
            r#"
schema_version: 1
app_packs:
  - oci://example.com/apps/chat:latest
"#,
        );
        write(&root.join("providers/providers.json"), "{");

        let report = run_doctor(&root, Some(DoctorStage::Setup));
        let check_ids = ids(&report);
        assert!(check_ids.contains("setup.bundle_manifest.latest_ref"));
        assert!(check_ids.contains("setup.bundle_manifest.remote_materialized"));
        assert!(check_ids.contains("setup.provider_registry.parse"));
    }

    #[test]
    fn lock_stage_reports_parse_format_digest_missing_and_stale_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        write(
            &root.join(BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\napp_packs:\n  - packs/app.gtpack\n",
        );
        let report = run_doctor(&root, Some(DoctorStage::Locks));
        assert!(ids(&report).contains("setup.lock.present"));

        write(&root.join(BUNDLE_LOCK_FILE), "{");
        let report = run_doctor(&root, Some(DoctorStage::Locks));
        assert!(ids(&report).contains("setup.lock.parse"));

        write(
            &root.join(BUNDLE_LOCK_FILE),
            r#"{
  "build_format_version": "unexpected",
  "app_packs": [
    {"reference": "packs/app.gtpack"},
    {"reference": "packs/stale.gtpack", "digest": "sha256:stale"}
  ]
}"#,
        );
        let report = run_doctor(&root, Some(DoctorStage::Locks));
        let check_ids = ids(&report);
        assert!(check_ids.contains("setup.lock.format_version"));
        assert!(check_ids.contains("setup.lock.digest_present"));
        assert!(check_ids.contains("setup.lock.stale_reference"));
    }

    #[test]
    fn lock_stage_reports_digest_mismatch_and_reference_absence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        write(
            &root.join(BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\napp_packs:\n  - packs/app.gtpack\n  - packs/missing.gtpack\n",
        );
        write(&root.join("packs/app.gtpack"), "actual bytes");
        write(
            &root.join(BUNDLE_LOCK_FILE),
            r#"{
  "build_format_version": "bundle-lock-v1",
  "app_packs": [
    {"reference": "packs/app.gtpack", "digest": "sha256:not-the-digest"}
  ]
}"#,
        );

        let report = run_doctor(&root, Some(DoctorStage::Locks));
        let check_ids = ids(&report);
        assert!(check_ids.contains("setup.lock.digest_match"));
        assert!(check_ids.contains("setup.lock.reference_present"));
    }

    #[test]
    fn route_stage_reports_missing_and_malformed_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        write(&root.join(BUNDLE_WORKSPACE_MARKER), "schema_version: 1\n");
        write(&root.join("tenants/demo/tenant.gmap"), "/ = app");
        write(
            &root.join("tenants/demo/teams/ops/team.gmap"),
            "_ = forbidden\n",
        );

        let report = run_doctor(&root, Some(DoctorStage::Routes));
        let check_ids = ids(&report);
        assert!(check_ids.contains("setup.routes.static_routes_present"));
        assert!(check_ids.contains("setup.routes.resolved_manifest_present"));

        write(&root.join("state/config/platform/static-routes.json"), "{");
        write(&root.join(".greentic/tunnel.json"), "{");
        let resolved = root
            .join("resolved")
            .join(bundle::resolved_manifest_filename("demo", None));
        write(&resolved, "# Resolved manifest placeholder");
        let report = run_doctor(&root, Some(DoctorStage::Routes));
        let check_ids = ids(&report);
        assert!(check_ids.contains("setup.routes.static_routes_parse"));
        assert!(check_ids.contains("setup.routes.tunnel_parse"));
        assert!(check_ids.contains("setup.routes.resolved_manifest_placeholder"));
    }

    #[test]
    fn cache_and_runtime_stages_report_informational_diagnostics() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("demo");
        std::fs::create_dir_all(&root).unwrap();
        write(&root.join(BUNDLE_WORKSPACE_MARKER), "schema_version: 1\n");

        let cache = run_doctor(&root, Some(DoctorStage::Cache));
        assert!(ids(&cache).contains("setup.cache.model"));
        assert_eq!(cache.status, "ok");

        let runtime = run_doctor(&root, Some(DoctorStage::Runtime));
        assert!(ids(&runtime).contains("setup.runtime.state_present"));
        assert_eq!(runtime.status, "ok");
    }

    #[test]
    fn helper_functions_parse_references_and_gmaps() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write(
            &root.join(BUNDLE_WORKSPACE_MARKER),
            "schema_version: 1\napp_packs:\n  - packs/app.gtpack\nextension_providers:\n  - oci://example.com/providers/messaging-slack:stable\n",
        );
        let refs = workspace_refs(root).unwrap();
        assert!(refs.contains("packs/app.gtpack"));
        assert!(refs.contains("oci://example.com/providers/messaging-slack:stable"));

        let candidates =
            materialized_pack_candidates(root, "oci://example.com/providers/messaging-slack:1.0.0");
        assert!(
            candidates
                .iter()
                .any(|p| p.ends_with("providers/messaging/messaging-slack.gtpack"))
        );
        assert!(materialized_pack_candidates(root, "not-a-pack").is_empty());
        assert!(is_remote_reference("https://example.com/app.gtpack"));
        assert!(is_stable_reference("oci://example.com/app:stable"));

        write(
            &root.join("tenants/demo/tenant.gmap"),
            "_ = forbidden\n# comment\n",
        );
        write(&root.join("tenants/demo/teams/ops/team.gmap"), "/ = app\n");
        assert!(is_forbidden_only_gmap(
            &root.join("tenants/demo/tenant.gmap")
        ));
        let targets = discover_gmap_targets(root);
        assert_eq!(targets.len(), 2);
    }
}
