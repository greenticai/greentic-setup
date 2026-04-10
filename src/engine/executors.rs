//! Step executor implementations for the setup engine.
//!
//! Each executor handles a specific `SetupStepKind`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::Value;
use sha2::{Digest, Sha256};

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
        if pack.cached_path.exists() && !target_path.exists() {
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

/// Execute the ApplyPackSetup step.
pub fn execute_apply_pack_setup(
    bundle_path: &Path,
    metadata: &SetupPlanMetadata,
    config: &SetupConfig,
) -> anyhow::Result<usize> {
    let mut count = 0;

    if !metadata.providers_remove.is_empty() {
        count += execute_remove_provider_artifacts(bundle_path, &metadata.providers_remove)?;
    }

    // Auto-install provider packs that are referenced in setup_answers
    // but not yet present in the bundle.
    auto_install_provider_packs(bundle_path, metadata);

    // Discover packs so we can find pack_path for secret alias seeding
    let discovered = if bundle_path.exists() {
        discovery::discover(bundle_path).ok()
    } else {
        None
    };

    // Persist setup answers to local config files and dev secrets store
    for (provider_id, answers) in &metadata.setup_answers {
        // Write answers to provider config directory
        let config_dir = bundle_path.join("state").join("config").join(provider_id);
        std::fs::create_dir_all(&config_dir)?;

        let config_path = config_dir.join("setup-answers.json");
        let content =
            serde_json::to_string_pretty(answers).context("failed to serialize setup answers")?;
        std::fs::write(&config_path, content).with_context(|| {
            format!(
                "failed to write setup answers to: {}",
                config_path.display()
            )
        })?;

        // Persist all answer values to the dev secrets store so that
        // WASM components can read them via the secrets API at runtime.
        let pack_path = discovered.as_ref().and_then(|d| {
            d.find_setup_target(provider_id)
                .map(|p| p.pack_path.as_path())
        });
        let env = crate::resolve_env(Some(&config.env));
        if config.verbose {
            let team_display = config.team.as_deref().unwrap_or("(none)");
            println!(
                "  [secrets] scope: env={env}, tenant={}, team={team_display}, provider={provider_id}",
                config.tenant
            );
            let example_uri = crate::canonical_secret_uri(
                &env,
                &config.tenant,
                config.team.as_deref(),
                provider_id,
                "_example_key",
            );
            println!("  [secrets] URI pattern: {example_uri}");
            if let Some(config_map) = answers.as_object() {
                let keys: Vec<&String> = config_map.keys().collect();
                println!("  [secrets] answer keys: {keys:?}");
            }
        }
        let rt = tokio::runtime::Runtime::new()
            .context("failed to create tokio runtime for secrets persistence")?;
        let persisted = rt.block_on(crate::qa::persist::persist_all_config_as_secrets(
            bundle_path,
            &env,
            &config.tenant,
            config.team.as_deref(),
            provider_id,
            answers,
            pack_path,
        ))?;
        if config.verbose {
            if persisted.is_empty() {
                println!(
                    "  [secrets] WARNING: 0 key(s) persisted for {provider_id} (all values empty?)"
                );
            } else {
                println!(
                    "  [secrets] persisted {} key(s) for {provider_id}: {:?}",
                    persisted.len(),
                    persisted
                );
            }
        }

        // Sync OAuth answers to tenant config JSON for webchat-gui providers
        match crate::tenant_config::sync_oauth_to_tenant_config(
            bundle_path,
            &config.tenant,
            provider_id,
            answers,
        ) {
            Ok(true) => {
                if config.verbose {
                    println!("  [oauth] updated tenant config for {provider_id}");
                }
            }
            Ok(false) => {}
            Err(e) => {
                println!("  [oauth] WARNING: failed to update tenant config: {e}");
            }
        }

        // Register webhooks if the provider needs one (e.g. Telegram, Slack, Webex)
        if let Some(result) = crate::webhook::register_webhook(
            provider_id,
            answers,
            &config.tenant,
            config.team.as_deref(),
        ) {
            let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
            if ok {
                println!("  [webhook] registered for {provider_id}");
            } else {
                let err = result
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                println!("  [webhook] WARNING: registration failed for {provider_id}: {err}");
            }
        }

        count += 1;
    }

    crate::platform_setup::persist_static_routes_artifact(bundle_path, &metadata.static_routes)?;
    let _ = crate::deployment_targets::persist_explicit_deployment_targets(
        bundle_path,
        &metadata.deployment_targets,
    );

    // Print post-setup instructions for providers needing manual steps
    let provider_configs: Vec<(String, Value)> = metadata
        .setup_answers
        .iter()
        .map(|(id, val)| (id.clone(), val.clone()))
        .collect();
    let team = config.team.as_deref().unwrap_or("default");
    crate::webhook::print_post_setup_instructions(&provider_configs, &config.tenant, team);

    Ok(count)
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
pub fn auto_install_provider_packs(bundle_path: &Path, metadata: &SetupPlanMetadata) {
    let bundle_abs =
        std::fs::canonicalize(bundle_path).unwrap_or_else(|_| bundle_path.to_path_buf());

    for provider_id in metadata.setup_answers.keys() {
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
}
