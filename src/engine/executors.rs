//! Step executor implementations for the setup engine.
//!
//! Each executor handles a specific `SetupStepKind`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, anyhow};
use serde_json::Value;
use sha2::{Digest, Sha256};
use zip::{ZipArchive, result::ZipError};

use crate::plan::{ResolvedPackInfo, SetupPlanMetadata};
use crate::{bundle, bundle_source::BundleSource, discovery};

use super::plan_builders::compute_simple_hash;
use super::types::SetupConfig;

/// Execute the CreateBundle step.
pub fn execute_create_bundle(
    bundle_path: &Path,
    metadata: &SetupPlanMetadata,
) -> anyhow::Result<()> {
    bundle::create_demo_bundle_structure(bundle_path, metadata.bundle_name.as_deref())
        .context("failed to create bundle structure")
}

/// Execute the ResolvePacks step.
pub fn execute_resolve_packs(
    _bundle_path: &Path,
    metadata: &SetupPlanMetadata,
) -> anyhow::Result<Vec<ResolvedPackInfo>> {
    let mut resolved = Vec::new();
    let mut failures = Vec::new();

    for pack_ref in &metadata.pack_refs {
        match resolve_pack_ref(pack_ref) {
            Ok(resolved_path) => {
                let canonical = resolved_path
                    .canonicalize()
                    .unwrap_or(resolved_path.clone());
                let pack_meta = discovery::read_pack_meta(&canonical)?;
                resolved.push(ResolvedPackInfo {
                    source_ref: pack_ref.clone(),
                    mapped_ref: canonical.display().to_string(),
                    resolved_digest: compute_file_digest(&canonical)
                        .unwrap_or_else(|_| format!("sha256:{}", compute_simple_hash(pack_ref))),
                    pack_id: pack_meta.map(|meta| meta.pack_id).unwrap_or_else(|| {
                        canonical
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string()
                    }),
                    entry_flows: Vec::new(),
                    cached_path: canonical.clone(),
                    output_path: canonical,
                });
            }
            Err(err) => {
                failures.push(format!("{pack_ref}: {err}"));
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "failed to resolve {} pack ref(s):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    Ok(resolved)
}

/// Execute the AddPacksToBundle step.
pub fn execute_add_packs_to_bundle(
    bundle_path: &Path,
    resolved_packs: &[ResolvedPackInfo],
) -> anyhow::Result<()> {
    let mut metadata_entries = Vec::new();

    for pack in resolved_packs {
        // Determine target directory based on pack ID domain prefix
        let target_dir = get_pack_target_dir(bundle_path, &pack.pack_id);
        std::fs::create_dir_all(&target_dir)?;

        let target_path = target_dir.join(format!("{}.gtpack", pack.pack_id));
        let source_path = pack.cached_path.canonicalize().ok();
        let existing_target_path = target_path.canonicalize().ok();
        if pack.cached_path.exists() && source_path != existing_target_path {
            std::fs::copy(&pack.cached_path, &target_path).with_context(|| {
                format!(
                    "failed to copy pack {} to {}",
                    pack.cached_path.display(),
                    target_path.display()
                )
            })?;
        }

        let reference = target_path
            .strip_prefix(bundle_path)
            .unwrap_or(&target_path)
            .to_string_lossy()
            .replace('\\', "/");
        let kind = if reference.starts_with("providers/") {
            bundle::BundleReferenceKind::ExtensionProvider
        } else {
            bundle::BundleReferenceKind::AppPack
        };
        metadata_entries.push(bundle::BundleReference {
            kind,
            reference,
            digest: Some(pack.resolved_digest.clone()),
        });
    }

    bundle::register_bundle_references(bundle_path, &metadata_entries, None)?;
    Ok(())
}

/// Determine the target directory for a pack based on its ID.
///
/// Packs with domain prefixes (e.g., `messaging-telegram`, `events-webhook`)
/// go to `providers/<domain>/`. Other packs go to `packs/`.
pub fn get_pack_target_dir(bundle_path: &Path, pack_id: &str) -> PathBuf {
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

fn invoke_registration_operation(
    bundle_path: &Path,
    pack_path: &Path,
    registration: &Value,
    request: &Value,
    config: &SetupConfig,
) -> anyhow::Result<Value> {
    let registration_obj = registration
        .as_object()
        .ok_or_else(|| anyhow!("setup component invocation must be an object"))?;
    let component_ref = registration_obj
        .get("component_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("setup component invocation missing component_ref"))?;
    let op = registration_obj
        .get("op")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("setup component invocation missing op"))?;

    if let Some(result) = registration_obj
        .get("result")
        .or_else(|| registration_obj.get("mock_result"))
        .or_else(|| registration_obj.get("outputs"))
    {
        return Ok(result.clone());
    }

    if let Ok(component) = read_registration_component(pack_path, component_ref)
        && let Some(output) = invoke_json_registration_component(&component, op, request)
    {
        return Ok(output);
    }

    invoke_wasm_registration_component(bundle_path, pack_path, component_ref, op, request, config)
}

pub fn invoke_setup_component_operation(
    bundle_path: &Path,
    pack_path: &Path,
    component_ref: &str,
    op: &str,
    request: &Value,
    config: &SetupConfig,
) -> anyhow::Result<Value> {
    let registration = serde_json::json!({
        "component_ref": component_ref,
        "op": op,
    });
    invoke_registration_operation(bundle_path, pack_path, &registration, request, config)
}

#[derive(Debug, Default)]
struct SetupRegistrationSecrets {
    values: Mutex<BTreeMap<String, Vec<u8>>>,
}

#[async_trait::async_trait]
impl greentic_secrets_lib::SecretsManager for SetupRegistrationSecrets {
    async fn read(&self, path: &str) -> greentic_secrets_lib::Result<Vec<u8>> {
        let values = self.values.lock().map_err(|_| {
            greentic_secrets_lib::SecretError::Backend(
                "setup component secrets lock poisoned".into(),
            )
        })?;
        values
            .get(path)
            .cloned()
            .ok_or_else(|| greentic_secrets_lib::SecretError::NotFound(path.to_string()))
    }

    async fn write(&self, path: &str, bytes: &[u8]) -> greentic_secrets_lib::Result<()> {
        let mut values = self.values.lock().map_err(|_| {
            greentic_secrets_lib::SecretError::Backend(
                "setup component secrets lock poisoned".into(),
            )
        })?;
        values.insert(path.to_string(), bytes.to_vec());
        Ok(())
    }

    async fn delete(&self, path: &str) -> greentic_secrets_lib::Result<()> {
        let mut values = self.values.lock().map_err(|_| {
            greentic_secrets_lib::SecretError::Backend(
                "setup component secrets lock poisoned".into(),
            )
        })?;
        values.remove(path);
        Ok(())
    }
}

fn invoke_wasm_registration_component(
    bundle_path: &Path,
    pack_path: &Path,
    component_ref: &str,
    op: &str,
    request: &Value,
    config: &SetupConfig,
) -> anyhow::Result<Value> {
    use greentic_runner_host::component_api::node::{
        ExecCtx as ComponentExecCtx, TenantCtx as ComponentTenantCtx,
    };
    use greentic_runner_host::config::{OperatorPolicy, SecretsPolicy};
    use greentic_runner_host::pack::{ComponentResolution, PackRuntime};
    use greentic_runner_host::provider::ProviderBinding;
    use greentic_runner_host::storage::{new_session_store, new_state_store};
    use greentic_runner_host::{HostConfig, RunnerWasiPolicy};
    use std::sync::Arc;

    let bindings_path = bundle_path
        .join("state")
        .join("config")
        .join("setup-component-bindings.yaml");
    if let Some(parent) = bindings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        &bindings_path,
        format!(
            r#"tenant: {}
flow_type_bindings:
  messaging:
    adapter: setup-component
    config: {{}}
    secrets: []
rate_limits: {{}}
retry: {{}}
timers: []
"#,
            config.tenant
        ),
    )
    .with_context(|| format!("write {}", bindings_path.display()))?;

    let mut host_config = HostConfig::load_from_path(&bindings_path)
        .with_context(|| format!("load {}", bindings_path.display()))?;
    host_config.secrets_policy = SecretsPolicy::allow_all();
    host_config.operator_policy = OperatorPolicy::allow_all();
    let host_config = Arc::new(host_config);

    let session_store = new_session_store();
    let state_store = new_state_store();
    let secrets: greentic_runner_host::secrets::DynSecretsManager =
        Arc::new(SetupRegistrationSecrets::default());
    let pack = greentic_runner_host::runtime::block_on(PackRuntime::load(
        pack_path,
        Arc::clone(&host_config),
        None,
        Some(pack_path),
        Some(Arc::clone(&session_store)),
        Some(Arc::clone(&state_store)),
        Arc::new(RunnerWasiPolicy::default()),
        secrets,
        None,
        false,
        ComponentResolution::default(),
    ))
    .with_context(|| format!("load setup component pack {}", pack_path.display()))?;

    let exec_ctx = ComponentExecCtx {
        tenant: ComponentTenantCtx {
            tenant: config.tenant.clone(),
            team: config.team.clone(),
            user: None,
            trace_id: None,
            i18n_id: None,
            correlation_id: Some(format!("setup-component:{component_ref}:{op}")),
            deadline_unix_ms: None,
            attempt: 1,
            idempotency_key: Some(format!("setup-component:{component_ref}:{op}")),
        },
        i18n_id: None,
        flow_id: format!("setup-component/{op}"),
        node_id: Some(component_ref.to_string()),
    };
    let input_json = serde_json::to_vec(request)?;
    let binding = ProviderBinding {
        provider_id: Some(component_ref.to_string()),
        provider_type: component_ref.to_string(),
        component_ref: component_ref.to_string(),
        export: "schema-core-api".to_string(),
        world: "greentic:provider/schema-core@1.0.0".to_string(),
        config_json: None,
        pack_ref: None,
    };
    match greentic_runner_host::runtime::block_on(pack.invoke_provider(
        &binding,
        exec_ctx.clone(),
        op,
        input_json,
    )) {
        Ok(output) => Ok(output),
        Err(provider_err) => {
            let input_json = serde_json::to_string(request)?;
            greentic_runner_host::runtime::block_on(pack.invoke_component(
                component_ref,
                exec_ctx,
                op,
                None,
                input_json,
            ))
            .with_context(|| {
                format!(
                    "invoke setup component '{component_ref}' op '{op}' (provider path failed: {provider_err})"
                )
            })
        }
    }
}

fn read_registration_component(pack_path: &Path, component_ref: &str) -> anyhow::Result<Value> {
    let file = File::open(pack_path).with_context(|| format!("open {}", pack_path.display()))?;
    let mut archive = match ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(ZipError::InvalidArchive(_)) | Err(ZipError::UnsupportedArchive(_)) => {
            anyhow::bail!("{} is not a zip pack", pack_path.display())
        }
        Err(err) => return Err(err.into()),
    };
    let candidates = registration_component_candidates(component_ref);
    for candidate in candidates {
        match archive.by_name(&candidate) {
            Ok(mut entry) => {
                let mut raw = String::new();
                entry
                    .read_to_string(&mut raw)
                    .with_context(|| format!("read setup component {candidate}"))?;
                return serde_json::from_str(&raw)
                    .or_else(|_| serde_yaml_bw::from_str(&raw))
                    .with_context(|| format!("parse setup component {candidate}"));
            }
            Err(ZipError::FileNotFound) => continue,
            Err(err) => return Err(err.into()),
        }
    }
    anyhow::bail!(
        "setup component_ref '{}' not found in {}",
        component_ref,
        pack_path.display()
    )
}

fn registration_component_candidates(component_ref: &str) -> Vec<String> {
    let trimmed = component_ref.trim().trim_start_matches("./");
    let mut candidates = vec![trimmed.to_string()];
    if !trimmed.ends_with(".json") && !trimmed.ends_with(".yaml") && !trimmed.ends_with(".yml") {
        candidates.push(format!("{trimmed}.json"));
        candidates.push(format!("components/{trimmed}.json"));
        candidates.push(format!("assets/{trimmed}.json"));
        candidates.push(format!("assets/components/{trimmed}.json"));
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn invoke_json_registration_component(
    component: &Value,
    op: &str,
    request: &Value,
) -> Option<Value> {
    let obj = component.as_object()?;
    if let Some(operations) = obj.get("operations").and_then(Value::as_object)
        && let Some(operation) = operations.get(op)
    {
        return operation_result(operation, request);
    }
    if let Some(ops) = obj.get("ops").and_then(Value::as_array) {
        for operation in ops {
            if operation.get("op").and_then(Value::as_str) == Some(op)
                || operation.get("name").and_then(Value::as_str) == Some(op)
                || operation.get("id").and_then(Value::as_str) == Some(op)
            {
                return operation_result(operation, request);
            }
        }
    }
    obj.get(op)
        .and_then(|operation| operation_result(operation, request))
}

fn operation_result(operation: &Value, request: &Value) -> Option<Value> {
    if let Some(result) = operation
        .get("result")
        .or_else(|| operation.get("output"))
        .or_else(|| operation.get("outputs"))
    {
        return Some(result.clone());
    }
    if operation.get("echo_request").and_then(Value::as_bool) == Some(true) {
        return Some(request.clone());
    }
    if operation.is_object() {
        return Some(operation.clone());
    }
    None
}

fn compute_file_digest(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let digest = Sha256::digest(bytes);
    let encoded = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("sha256:{encoded}"))
}

fn resolve_pack_ref(pack_ref: &str) -> anyhow::Result<PathBuf> {
    let source = BundleSource::parse(pack_ref)?;
    let resolved = source.resolve()?;

    if resolved.extension().and_then(|ext| ext.to_str()) != Some("gtpack") {
        anyhow::bail!(
            "resolved pack ref is not a .gtpack file: {}",
            resolved.display()
        );
    }

    Ok(resolved)
}

/// Remove provider artifacts and config directories.
pub fn execute_remove_provider_artifacts(
    bundle_path: &Path,
    providers_remove: &[String],
) -> anyhow::Result<usize> {
    let mut removed = 0usize;
    let discovered = discovery::discover(bundle_path).ok();
    for provider_id in providers_remove {
        if let Some(discovered) = discovered.as_ref()
            && let Some(provider) = discovered
                .providers
                .iter()
                .find(|provider| provider.provider_id == *provider_id)
        {
            if provider.pack_path.exists() {
                std::fs::remove_file(&provider.pack_path).with_context(|| {
                    format!(
                        "failed to remove provider pack {}",
                        provider.pack_path.display()
                    )
                })?;
            }
            removed += 1;
        } else {
            let target_dir = get_pack_target_dir(bundle_path, provider_id);
            let target_path = target_dir.join(format!("{provider_id}.gtpack"));
            if target_path.exists() {
                std::fs::remove_file(&target_path).with_context(|| {
                    format!("failed to remove provider pack {}", target_path.display())
                })?;
                removed += 1;
            }
        }

        let config_dir = bundle_path.join("state").join("config").join(provider_id);
        if config_dir.exists() {
            std::fs::remove_dir_all(&config_dir).with_context(|| {
                format!(
                    "failed to remove provider config dir {}",
                    config_dir.display()
                )
            })?;
        }
    }
    Ok(removed)
}

/// Search sibling bundles for provider packs referenced in setup_answers
/// and install them into this bundle if missing.
///
/// "Missing" is determined by pack_id, not filename: a pack file with any
/// filename that declares the matching pack_id in its manifest counts as
/// already installed. Otherwise a custom-named pack (e.g. a tenant-specific
/// build placed alongside the canonical name) gets clobbered every time
/// setup runs.
pub fn auto_install_provider_packs(bundle_path: &Path, metadata: &SetupPlanMetadata) {
    let bundle_abs =
        std::fs::canonicalize(bundle_path).unwrap_or_else(|_| bundle_path.to_path_buf());

    let installed_ids: std::collections::HashSet<String> = discovery::discover(bundle_path)
        .map(|d| {
            d.providers
                .into_iter()
                .chain(d.app_packs)
                .map(|p| p.provider_id)
                .collect()
        })
        .unwrap_or_default();

    for provider_id in metadata.setup_answers.keys() {
        if installed_ids.contains(provider_id) {
            continue;
        }
        let target_dir = get_pack_target_dir(bundle_path, provider_id);
        let target_path = target_dir.join(format!("{provider_id}.gtpack"));
        if target_path.exists() {
            continue;
        }

        // Determine the provider domain from the ID
        let domain = domain_from_provider_id(provider_id);

        // Search for the pack in sibling bundles and build output
        if let Some(source) = find_provider_pack_source(provider_id, domain, &bundle_abs) {
            if let Err(err) = std::fs::create_dir_all(&target_dir) {
                eprintln!(
                    "  [provider] WARNING: failed to create {}: {err}",
                    target_dir.display()
                );
                continue;
            }
            match std::fs::copy(&source, &target_path) {
                Ok(_) => println!(
                    "  [provider] installed {provider_id}.gtpack from {}",
                    source.display()
                ),
                Err(err) => eprintln!(
                    "  [provider] WARNING: failed to copy {}: {err}",
                    source.display()
                ),
            }
        } else {
            eprintln!("  [provider] WARNING: {provider_id}.gtpack not found in sibling bundles");
        }
    }
}

/// Extract domain from a provider ID (e.g. "messaging-telegram" → "messaging").
pub fn domain_from_provider_id(provider_id: &str) -> &str {
    const DOMAIN_PREFIXES: &[&str] = &[
        "messaging-",
        "events-",
        "oauth-",
        "secrets-",
        "mcp-",
        "state-",
        "telemetry-",
    ];
    for prefix in DOMAIN_PREFIXES {
        if provider_id.starts_with(prefix) {
            return prefix.trim_end_matches('-');
        }
    }
    "messaging" // default
}

/// Search known locations for a provider pack file.
///
/// Search order:
/// 1. Sibling bundle directories: `../<bundle>/providers/<domain>/<id>.gtpack`
/// 2. Build output: `../greentic-messaging-providers/target/packs/<id>.gtpack`
pub fn find_provider_pack_source(
    provider_id: &str,
    domain: &str,
    bundle_abs: &Path,
) -> Option<PathBuf> {
    let parent = bundle_abs.parent()?;
    let filename = format!("{provider_id}.gtpack");

    // 1. Sibling bundles
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let sibling = entry.path();
            if sibling == *bundle_abs || !sibling.is_dir() {
                continue;
            }
            let candidate = sibling.join("providers").join(domain).join(&filename);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 2. Build output from greentic-messaging-providers
    for ancestor in parent.ancestors().take(4) {
        let candidate = ancestor
            .join("greentic-messaging-providers")
            .join("target")
            .join("packs")
            .join(&filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Execute the WriteGmapRules step.
pub fn execute_write_gmap_rules(
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

/// Execute the CopyResolvedManifest step.
pub fn execute_copy_resolved_manifests(
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

/// Execute the ValidateBundle step.
pub fn execute_validate_bundle(bundle_path: &Path) -> anyhow::Result<()> {
    bundle::validate_bundle_exists(bundle_path)
}

/// Execute the BuildFlowIndex step.
///
/// Scans all flows in the bundle, builds a TF-IDF index and a routing-compatible
/// index, and optionally generates intents.md documentation.
/// Output is written to `bundle/state/indexes/`.
///
/// Requires the `fast2flow` feature AND the `fast2flow-bundle` crate wired as a
/// dependency.  Until `fast2flow-bundle` is published or vendored, this is a
/// no-op stub that logs a skip message.
pub fn execute_build_flow_index(_bundle_path: &Path, _config: &SetupConfig) -> anyhow::Result<()> {
    tracing::debug!("fast2flow indexing skipped (fast2flow-bundle not available)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_setup::StaticRoutesPolicy;
    use std::collections::BTreeSet;

    fn empty_metadata(pack_refs: Vec<String>) -> SetupPlanMetadata {
        SetupPlanMetadata {
            bundle_name: None,
            pack_refs,
            tenants: Vec::new(),
            default_assignments: Vec::new(),
            providers: Vec::new(),
            update_ops: BTreeSet::new(),
            remove_targets: BTreeSet::new(),
            packs_remove: Vec::new(),
            providers_remove: Vec::new(),
            tenants_remove: Vec::new(),
            access_changes: Vec::new(),
            static_routes: StaticRoutesPolicy::default(),
            deployment_targets: Vec::new(),
            setup_answers: serde_json::Map::new(),
            tunnel: None,
            telemetry: None,
        }
    }

    #[test]
    fn resolve_packs_errors_when_any_pack_ref_fails() {
        let metadata = empty_metadata(vec!["/definitely/missing/example.gtpack".to_string()]);
        let err = execute_resolve_packs(Path::new("."), &metadata).unwrap_err();
        let message = err.to_string();

        assert!(message.contains("failed to resolve 1 pack ref"));
        assert!(message.contains("/definitely/missing/example.gtpack"));
    }

    /// Regression: a custom-named pack whose manifest declares the matching
    /// pack_id must satisfy `auto_install_provider_packs`. Filename-only
    /// detection caused tenant-specific builds (e.g. `*-3aigent.gtpack`) to
    /// be clobbered by the canonical name on every setup run.
    #[test]
    fn auto_install_skips_when_pack_id_matches_under_custom_filename() {
        use std::io::Write;
        use zip::write::{FileOptions, ZipWriter};

        let temp = tempfile::tempdir().expect("tempdir");
        let bundle = temp.path().join("bundle");
        let messaging_dir = bundle.join("providers").join("messaging");
        std::fs::create_dir_all(&messaging_dir).expect("create messaging dir");

        let custom_pack = messaging_dir.join("messaging-webchat-gui-3aigent.gtpack");
        let file = std::fs::File::create(&custom_pack).expect("create pack file");
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("pack.manifest.json", options)
            .expect("start manifest");
        writer
            .write_all(
                serde_json::json!({
                    "pack_id": "messaging-webchat-gui",
                    "display_name": "WebChat GUI",
                })
                .to_string()
                .as_bytes(),
            )
            .expect("write manifest");
        writer.finish().expect("finish zip");

        let canonical_pack = messaging_dir.join("messaging-webchat-gui.gtpack");
        assert!(!canonical_pack.exists(), "precondition: canonical absent");

        let mut metadata = empty_metadata(vec![]);
        metadata.setup_answers.insert(
            "messaging-webchat-gui".to_string(),
            serde_json::Value::Object(serde_json::Map::new()),
        );

        auto_install_provider_packs(&bundle, &metadata);

        assert!(
            custom_pack.exists(),
            "custom-named pack must be left in place"
        );
        assert!(
            !canonical_pack.exists(),
            "must not auto-install canonical-named duplicate when pack_id already present"
        );
    }
}
