//! Step executor implementations for the setup engine.
//!
//! Each executor handles a specific `SetupStepKind`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde_json::Value;

use crate::plan::{ResolvedPackInfo, SetupPlanMetadata};
use crate::{bundle, discovery};

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

    for pack_ref in &metadata.pack_refs {
        // For now, we only support local pack refs (file paths)
        // OCI resolution requires async and the distributor client
        let path = PathBuf::from(pack_ref);

        // Try to canonicalize the path to handle relative paths correctly
        let resolved_path = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.join(&path))
                .unwrap_or_else(|| path.clone())
        };

        if resolved_path.exists() {
            let canonical = resolved_path
                .canonicalize()
                .unwrap_or(resolved_path.clone());
            resolved.push(ResolvedPackInfo {
                source_ref: pack_ref.clone(),
                mapped_ref: canonical.display().to_string(),
                resolved_digest: format!("sha256:{}", compute_simple_hash(pack_ref)),
                pack_id: canonical
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                entry_flows: Vec::new(),
                cached_path: canonical.clone(),
                output_path: canonical,
            });
        } else if pack_ref.starts_with("oci://")
            || pack_ref.starts_with("repo://")
            || pack_ref.starts_with("store://")
        {
            // Remote packs need async resolution via distributor-client
            // For now, we'll skip and let the caller handle this
            tracing::warn!("remote pack ref requires async resolution: {}", pack_ref);
        } else {
            // Log warning for unresolved local paths
            tracing::warn!(
                "pack ref not found: {} (resolved to: {})",
                pack_ref,
                resolved_path.display()
            );
        }
    }

    Ok(resolved)
}

/// Execute the AddPacksToBundle step.
pub fn execute_add_packs_to_bundle(
    bundle_path: &Path,
    resolved_packs: &[ResolvedPackInfo],
) -> anyhow::Result<()> {
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
    }
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
            d.providers
                .iter()
                .find(|p| p.provider_id == *provider_id)
                .map(|p| p.pack_path.as_path())
        });
        let env = crate::resolve_env(Some(&config.env));
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
        if config.verbose && !persisted.is_empty() {
            println!(
                "  [secrets] persisted {} key(s) for {provider_id}",
                persisted.len()
            );
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
