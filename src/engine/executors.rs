//! Step executor implementations for the setup engine.
//!
//! Each executor handles a specific `SetupStepKind`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use serde_json::{Map as JsonMap, Value};
use sha2::{Digest, Sha256};
use zip::{ZipArchive, result::ZipError};

use crate::plan::{ResolvedPackInfo, SetupPlanMetadata};
use crate::{bundle, bundle_source::BundleSource, discovery};

use super::plan_builders::compute_simple_hash;
use super::types::SetupConfig;

#[derive(Debug)]
pub struct ApplyPackSetupReport {
    pub provider_updates: usize,
    pub pending_setup_actions: Vec<crate::setup_actions::SetupAction>,
}

/// Resolve the canonical set of secret-marked answer keys for a pack (B12a).
///
/// The source of truth is `pack_to_form_spec()`, which unions:
/// - `setup.yaml` / `qa/*.json` questions with `secret: true`, and
/// - entries from `assets/secret-requirements.json` / CBOR manifest.
///
/// Each key is normalized via `canonical_secret_name` so the redaction
/// match logic below can mirror `seed_secret_requirement_aliases`'s
/// suffix-matching (so `bot_token` answers satisfy a `webex_bot_token`
/// requirement).
///
/// Returns `None` when the pack carries no setup metadata at all — the
/// caller should then refuse to write the transitional artifacts for
/// non-empty answers (B12a fail-closed contract).
fn resolve_secret_answer_keys(pack_path: &Path, provider_id: &str) -> Option<BTreeSet<String>> {
    let form = crate::setup_to_formspec::pack_to_form_spec(pack_path, provider_id)?;
    let secret_ids = form
        .questions
        .iter()
        .filter(|q| q.secret)
        .map(|q| crate::secret_name::canonical_secret_name(&q.id))
        .collect::<BTreeSet<String>>();
    Some(secret_ids)
}

/// Match an answer key (post-normalization) against the secret-marked set.
///
/// This MUST mirror `qa::persist::seed_secret_requirement_aliases` exactly
/// (`canonical_req_key.ends_with(&norm_cfg)`), so that the set of answers
/// redacted from disk is identical to the set persisted to the dev secrets
/// store as secrets. If redaction were narrower than seeding, a key the
/// persist path treats as a secret would stay as plaintext on disk — a leak.
///
/// Match when the answer key's canonical form equals a secret key, or a
/// secret key ends with it (forward direction only — so requirement
/// `webex_bot_token` is satisfied by answer `bot_token`). The earlier
/// version ALSO matched the reverse direction (`norm.ends_with(secret)`),
/// which the persist path does not do; that over-matched (answer `bot_token`
/// wrongly redacted for an unrelated secret `token`) and is dropped here.
fn is_secret_answer_key(answer_key: &str, secret_keys: &BTreeSet<String>) -> bool {
    let norm = crate::secret_name::canonical_secret_name(answer_key);
    secret_keys
        .iter()
        .any(|secret| secret == &norm || secret.ends_with(&norm))
}

/// Drop secret-marked answer values entirely (B12a). Used for the on-disk
/// `setup-answers.json` — its downstream readers in `greentic-start`
/// (`messaging_app::inject_pack_setup_answers`,
/// `ingress_dispatch::build_injected_config`) already source secret values
/// from `SecretsManager`, so the key has no value to contribute. Dropping
/// the key avoids putting any reference (URI or otherwise) into a JSON
/// value slot consumers may treat as the raw credential.
fn strip_secret_answer_keys(answers: &Value, secret_keys: &BTreeSet<String>) -> Value {
    let Some(map) = answers.as_object() else {
        return answers.clone();
    };
    let mut filtered = serde_json::Map::with_capacity(map.len());
    for (key, value) in map {
        if is_secret_answer_key(key, secret_keys) {
            continue;
        }
        filtered.insert(key.clone(), value.clone());
    }
    Value::Object(filtered)
}

/// Replace secret-marked answer values with canonical `secrets://` URI
/// references for the `config.envelope.cbor` artifact. Components that
/// already consume the envelope's config via the URI-resolving pattern
/// (e.g. greentic-start `notifier/config.rs` for state-redis) keep working
/// unchanged; components that read the `<key>_b64` injection see the
/// resolved plaintext from `SecretsManager` via `runner_host.get_secret`.
fn redact_secret_answer_values_to_uri_refs(
    answers: &Value,
    secret_keys: &BTreeSet<String>,
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
) -> Value {
    let Some(map) = answers.as_object() else {
        return answers.clone();
    };
    let mut filtered = serde_json::Map::with_capacity(map.len());
    for (key, value) in map {
        if is_secret_answer_key(key, secret_keys) {
            let uri = crate::canonical_secret_uri(env, tenant, team, provider_id, key);
            filtered.insert(key.clone(), Value::String(uri));
        } else {
            filtered.insert(key.clone(), value.clone());
        }
    }
    Value::Object(filtered)
}

/// Decide the secret-key set for redaction, applying the B12a fail-closed
/// contract.
///
/// `resolved` carries a load-bearing `Option`:
///   - `Some(set)` — the pack HAS classifiable metadata. An empty set means
///     the pack legitimately declares zero secrets; proceed (write every
///     answer as non-secret). This is NOT a failure.
///   - `None` — no pack / no classifiable metadata at all. With non-empty
///     answers we cannot tell which are secret, so fail closed rather than
///     risk writing plaintext. With empty answers there's nothing to leak,
///     so proceed with an empty set.
fn secret_keys_or_fail_closed(
    resolved: Option<BTreeSet<String>>,
    answers: &Value,
    provider_id: &str,
    pack_found: bool,
    known_pack_ids: &[String],
) -> anyhow::Result<BTreeSet<String>> {
    match resolved {
        Some(set) => Ok(set),
        // No pack matched `provider_id` at all. Say so. Reporting this as
        // "the pack ships no classifiable setup metadata" is actively
        // misleading — it sends people to inspect a pack that is fine, or one
        // they never had. The real cause is nearly always that the answers
        // file's top-level key does not equal the pack's manifest `pack_id`
        // (lookup is exact string equality), and the two families do not share
        // a convention: messaging packs use short ids (`messaging-telegram`),
        // events packs use dotted ones (`greentic.events.webhook`).
        None if !pack_found && answers_have_content(answers) => {
            let known = if known_pack_ids.is_empty() {
                "  (none — no packs were discovered in this bundle)".to_string()
            } else {
                known_pack_ids
                    .iter()
                    .map(|id| format!("  {id}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            anyhow::bail!(
                "B12a: no pack in this bundle has pack_id `{provider_id}`, so the answers keyed \
                 under it cannot be applied.\n\nPacks that can take setup answers here:\n{known}\n\n\
                 The top-level key in your answers file must equal the pack's manifest pack_id \
                 exactly. If you meant one of the above, rename the key to match.",
            )
        }
        // The pack IS here, it just ships nothing we can classify against.
        None if answers_have_content(answers) => anyhow::bail!(
            "B12a: refusing to write setup-answers for `{provider_id}` — the pack ships no \
             classifiable setup metadata (no setup.yaml / qa/*.json / secret-requirements), so \
             we can't tell which answers are secrets and won't risk writing plaintext. \
             Install/repair the pack with a setup.yaml (`secret: true` flags) or an \
             `assets/secret-requirements.json`, then retry.",
        ),
        None => Ok(BTreeSet::new()),
    }
}

/// Return true if `answers` is a JSON object with at least one non-null
/// string-typed field — i.e. material that could plausibly be a secret.
/// Used to decide whether the B12a fail-closed contract applies when the
/// redaction metadata can't be resolved.
fn answers_have_content(answers: &Value) -> bool {
    let Some(map) = answers.as_object() else {
        return false;
    };
    map.iter().any(|(key, v)| {
        // Host-injected base URLs are never secrets. Exclude them so a pack whose
        // *only* answer is an injected `public_base_url`/`oauth_callback_base_url`
        // (e.g. an app pack with no setup metadata that merely received the base
        // URL during injection) does not trip the B12a fail-closed guard and block
        // the whole setup. Real secret material still triggers fail-closed.
        if matches!(key.as_str(), "public_base_url" | "oauth_callback_base_url") {
            return false;
        }
        match v {
            Value::String(s) => !s.is_empty(),
            Value::Null => false,
            _ => true,
        }
    })
}

/// C7: attempt to emit a `pack-config-input.v1` file for one provider.
/// Soft-fails on error — the C4.2 compat shim still serves these keys from
/// DevStore.
fn try_emit_pack_config_input(
    bundle_path: &Path,
    pack_path: &Path,
    env: &str,
    provider_id: &str,
    answers: &Value,
    trace_context: &str,
) {
    let Some(form_spec) = crate::setup_to_formspec::pack_to_form_spec(pack_path, provider_id)
    else {
        return;
    };
    let bundle_id = crate::qa::persist::infer_bundle_id(bundle_path);
    if let Err(err) = crate::qa::persist::emit_pack_config_input(
        bundle_path,
        env,
        &bundle_id,
        provider_id,
        answers,
        &form_spec,
    ) {
        tracing::warn!(
            provider_id = %provider_id,
            env = %env,
            error = %err,
            "pack-config-input emission failed ({trace_context}); runtime falls back to DevStore via C4.2 compat shim",
        );
    }
}

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

/// Execute the ApplyPackSetup step.
pub fn execute_apply_pack_setup(
    bundle_path: &Path,
    metadata: &SetupPlanMetadata,
    config: &SetupConfig,
) -> anyhow::Result<ApplyPackSetupReport> {
    let mut count = 0;
    let mut pending_setup_actions = Vec::new();

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

    let provider_ids = setup_provider_ids(metadata, discovered.as_ref());

    // Persist setup answers to local config files and dev secrets store
    for provider_id in provider_ids {
        let empty_answers = Value::Object(serde_json::Map::new());
        let answers = metadata
            .setup_answers
            .get(&provider_id)
            .unwrap_or(&empty_answers);
        let mut effective_answers = answers.clone();
        let pack_path = discovered.as_ref().and_then(|d| {
            d.find_setup_target(&provider_id)
                .map(|p| p.pack_path.as_path())
        });
        if !crate::provider_state::provider_enabled(&effective_answers) {
            let persisted_answers = crate::setup_actions::strip_setup_actions(&effective_answers);
            let config_dir = bundle_path.join("state").join("config").join(&provider_id);
            std::fs::create_dir_all(&config_dir)?;
            let config_path = config_dir.join("setup-answers.json");
            let content = serde_json::to_string_pretty(&persisted_answers)
                .context("failed to serialize setup answers")?;
            std::fs::write(&config_path, content).with_context(|| {
                format!(
                    "failed to write setup answers to: {}",
                    config_path.display()
                )
            })?;
            let env = crate::resolve_env(Some(&config.env));
            let rt = tokio::runtime::Runtime::new()
                .context("failed to create tokio runtime for secrets persistence")?;
            rt.block_on(crate::qa::persist::persist_all_config_as_secrets(
                bundle_path,
                &env,
                &config.tenant,
                config.team.as_deref(),
                &provider_id,
                &persisted_answers,
                pack_path,
            ))?;
            if let Some(pack_path) = pack_path {
                crate::config_envelope::write_provider_config_envelope(
                    &bundle_path.join(".providers"),
                    &provider_id,
                    "setup-input",
                    &persisted_answers,
                    pack_path,
                    false,
                )
                .with_context(|| {
                    format!(
                        "failed to write provider config envelope for {} using {}",
                        provider_id,
                        pack_path.display()
                    )
                })?;
                try_emit_pack_config_input(
                    bundle_path,
                    pack_path,
                    &env,
                    &provider_id,
                    &persisted_answers,
                    "setup-input path",
                );
            }
            count += 1;
            continue;
        }
        let mut setup_actions = crate::setup_actions::extract_setup_actions(
            &provider_id,
            &config.tenant,
            config.team.as_deref(),
            answers,
        )?;
        setup_actions.extend(extract_pack_setup_actions(
            discovered.as_ref(),
            &provider_id,
            &config.tenant,
            config.team.as_deref(),
        )?);
        defer_registration_actions_missing_inputs(&mut setup_actions, &effective_answers);
        run_setup_action_registrations(SetupActionRegistrationContext {
            bundle_path,
            discovered: discovered.as_ref(),
            provider_id: &provider_id,
            config,
            bundle_name: metadata.bundle_name.as_deref(),
            public_base_url: metadata.static_routes.public_base_url.as_deref(),
            answers: &mut effective_answers,
            actions: &mut setup_actions,
        })?;
        hydrate_oauth_install_actions(&mut setup_actions, &effective_answers);
        if !setup_actions.is_empty() {
            crate::setup_actions::sign_pending_oauth_actions(bundle_path, &mut setup_actions)?;
            crate::setup_actions::persist_setup_actions(bundle_path, &setup_actions)?;
            pending_setup_actions.extend(setup_actions.clone());
        }
        // Slack's `app_redirect` deep link (the final "Add to Slack" -> DM with
        // the bot) cannot resolve the workspace without a `team` parameter in
        // multi-workspace / Enterprise Grid browsers. When a bot token answer is
        // available, resolve the workspace once via auth.test and pin it.
        let slack_team = slack_team_id_from_answers(&effective_answers);
        if let Some(team) = slack_team.as_deref() {
            apply_slack_team_to_app_url(&mut effective_answers, team);
        }
        let persisted_answers = crate::setup_actions::strip_setup_actions(&effective_answers);

        // Write answers to provider config directory
        let config_dir = bundle_path.join("state").join("config").join(&provider_id);
        std::fs::create_dir_all(&config_dir)?;

        // Resolve the pack path early so we can both discover secret-marked
        // keys (to redact plaintext from the on-disk artifacts — B12a) and
        // pass it to the envelope writer + secrets-persist path.
        let pack_path = discovered.as_ref().and_then(|d| {
            d.find_setup_target(&provider_id)
                .map(|p| p.pack_path.as_path())
        });
        let env = crate::resolve_env(Some(&config.env));

        // B12a fail-closed contract: resolve the secret-marked answer key
        // set from the pack's `pack_to_form_spec` (the union of setup.yaml /
        // qa/*.json `secret: true` questions and `secret-requirements.json`
        // entries). The `Option` is load-bearing:
        //   - `Some(set)` — the pack HAS a form spec. An empty set means the
        //     pack legitimately declares zero secrets (e.g. only model/url
        //     config); we proceed and write every answer as non-secret.
        //   - `None` — the pack ships NO classifiable metadata at all (no
        //     setup.yaml, no qa/*.json, no secret-requirements). We cannot
        //     tell which answers are secret, so with non-empty answers we
        //     fail closed rather than silently writing plaintext.
        // A missing pack path also lands on `None`, but it is a different
        // problem with a different fix, so carry the distinction through
        // rather than collapsing both into one misleading error.
        let resolved_secret_keys: Option<BTreeSet<String>> =
            pack_path.and_then(|pp| resolve_secret_answer_keys(pp, &provider_id));
        let known_pack_ids: Vec<String> = discovered
            .as_ref()
            .map(|d| {
                d.setup_targets()
                    .iter()
                    .map(|p| p.provider_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let secret_keys = secret_keys_or_fail_closed(
            resolved_secret_keys,
            answers,
            &provider_id,
            pack_path.is_some(),
            &known_pack_ids,
        )?;
        let mut answers_for_disk = strip_secret_answer_keys(answers, &secret_keys);
        if let Some(team) = slack_team.as_deref() {
            apply_slack_team_to_app_url(&mut answers_for_disk, team);
        }
        let mut envelope_answers = redact_secret_answer_values_to_uri_refs(
            answers,
            &secret_keys,
            &env,
            &config.tenant,
            config.team.as_deref(),
            &provider_id,
        );
        if let Some(team) = slack_team.as_deref() {
            apply_slack_team_to_app_url(&mut envelope_answers, team);
        }

        let config_path = config_dir.join("setup-answers.json");
        let content = serde_json::to_string_pretty(&answers_for_disk)
            .context("failed to serialize setup answers")?;
        std::fs::write(&config_path, content).with_context(|| {
            format!(
                "failed to write setup answers to: {}",
                config_path.display()
            )
        })?;

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
                &provider_id,
                "_example_key",
            );
            println!("  [secrets] URI pattern: {example_uri}");
            if let Some(config_map) = persisted_answers.as_object() {
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
            &provider_id,
            &persisted_answers,
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

        // Materialize a provider config envelope so runtime/provider ingest
        // paths can read setup-applied config. After B12a the envelope carries
        // `secrets://` URI references for secret-marked keys (matching the
        // canonical URIs in the dev secrets store) instead of plaintext.
        if let Some(pack_path) = pack_path {
            crate::config_envelope::write_provider_config_envelope(
                &bundle_path.join(".providers"),
                &provider_id,
                "setup-input",
                &envelope_answers,
                pack_path,
                false,
            )
            .with_context(|| {
                format!(
                    "failed to write provider config envelope for {} using {}",
                    provider_id,
                    pack_path.display()
                )
            })?;
        } else if config.verbose {
            println!(
                "  [config] WARNING: no resolved pack path for {provider_id}; skipped config envelope write"
            );
        }

        // C7: emit pack-config-input.v1 for the enabled-provider path as
        // well. Same soft-fail posture as the disabled-provider branch above.
        if let Some(pack_path) = pack_path {
            try_emit_pack_config_input(
                bundle_path,
                pack_path,
                &env,
                &provider_id,
                &persisted_answers,
                "apply-answers path",
            );
        }

        // Sync OAuth answers to tenant config JSON for webchat-gui providers
        match crate::tenant_config::sync_oauth_to_tenant_config(
            bundle_path,
            &config.tenant,
            &provider_id,
            &persisted_answers,
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

        // Sync `skin` answer to tenant config JSON for webchat-gui providers
        match crate::tenant_config::sync_skin_to_tenant_config(
            bundle_path,
            &config.tenant,
            &provider_id,
            &persisted_answers,
        ) {
            Ok(true) => {
                if config.verbose {
                    println!("  [skin] updated tenant config for {provider_id}");
                }
            }
            Ok(false) => {}
            Err(e) => {
                println!("  [skin] WARNING: failed to update tenant config: {e}");
            }
        }

        // Sync `nav_links_json` answer to tenant config JSON for webchat-gui providers
        if provider_id.contains("webchat-gui") && config.verbose {
            let preview = answers
                .as_object()
                .and_then(|m| m.get("nav_links"))
                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "<unserializable>".into()))
                .unwrap_or_else(|| "<absent>".into());
            println!("  [nav_links] received answer for {provider_id}: {preview}");
        }
        match crate::tenant_config::sync_nav_links_to_tenant_config(
            bundle_path,
            &config.tenant,
            &provider_id,
            &persisted_answers,
        ) {
            Ok(true) => {
                if config.verbose {
                    println!("  [nav_links] updated tenant config for {provider_id}");
                }
            }
            Ok(false) => {}
            Err(e) => {
                println!("  [nav_links] WARNING: failed to update tenant config: {e}");
            }
        }

        // Register webhooks if the provider needs one (e.g. Telegram, Slack, Webex)
        if let Some(result) = crate::webhook::register_webhook(
            &provider_id,
            &persisted_answers,
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
        .filter(|(_, val)| crate::provider_state::provider_enabled(val))
        .map(|(id, val)| (id.clone(), val.clone()))
        .collect();
    let team = config.team.as_deref().unwrap_or("default");
    crate::webhook::print_post_setup_instructions(&provider_configs, &config.tenant, team);

    Ok(ApplyPackSetupReport {
        provider_updates: count,
        pending_setup_actions,
    })
}

fn setup_provider_ids(
    metadata: &SetupPlanMetadata,
    discovered: Option<&crate::discovery::DiscoveryResult>,
) -> BTreeSet<String> {
    let mut provider_ids: BTreeSet<String> = metadata.setup_answers.keys().cloned().collect();
    if let Some(discovered) = discovered {
        for provider in discovered.setup_targets() {
            if let Ok(Some(spec)) = crate::setup_input::load_setup_spec(&provider.pack_path)
                && !spec.setup_actions.is_empty()
            {
                provider_ids.insert(provider.provider_id.clone());
            }
        }
    }
    provider_ids
}

fn extract_pack_setup_actions(
    discovered: Option<&crate::discovery::DiscoveryResult>,
    provider_id: &str,
    tenant: &str,
    team: Option<&str>,
) -> anyhow::Result<Vec<crate::setup_actions::SetupAction>> {
    let Some(provider) = discovered.and_then(|d| d.find_setup_target(provider_id)) else {
        return Ok(Vec::new());
    };
    let Some(spec) = crate::setup_input::load_setup_spec(&provider.pack_path)? else {
        return Ok(Vec::new());
    };
    if spec.setup_actions.is_empty() {
        return Ok(Vec::new());
    }
    let setup_actions = spec
        .setup_actions
        .into_iter()
        .map(|mut action| {
            if let Some(obj) = action.as_object_mut() {
                obj.remove("provider_id");
                obj.remove("tenant");
                obj.remove("team");
            }
            action
        })
        .collect::<Vec<_>>();
    let value = serde_json::json!({ "setup_actions": setup_actions });
    crate::setup_actions::extract_setup_actions(provider_id, tenant, team, &value)
}

fn defer_registration_actions_missing_inputs(
    actions: &mut Vec<crate::setup_actions::SetupAction>,
    answers: &Value,
) {
    actions.retain(|action| {
        if action.extra.get("registration").is_none() {
            return true;
        }
        let registration_satisfied = match action.kind {
            crate::setup_actions::SetupActionKind::OauthInstallButton => {
                client_id_for_action(action, answers).is_some()
            }
            crate::setup_actions::SetupActionKind::OpenUrl => {
                !registration_output_missing(action, answers)
            }
            _ => return true,
        };
        registration_satisfied
            || registration_has_any_declared_input(action.extra.get("registration"), answers)
    });
}

/// Whether an action's registration op has *not yet* produced any of its
/// declared `*_output` fields — i.e. whether registration still needs to run.
/// Used by kinds (like `open_url`) that have no `client_id` to key off of.
fn registration_output_missing(
    action: &crate::setup_actions::SetupAction,
    answers: &Value,
) -> bool {
    let Some(registration_obj) = action.extra.get("registration").and_then(Value::as_object) else {
        return true;
    };
    let Some(answers_obj) = answers.as_object() else {
        return true;
    };
    !registration_obj.iter().any(|(key, field_value)| {
        key.ends_with("_output")
            && field_value
                .as_str()
                .map(str::trim)
                .filter(|field_name| !field_name.is_empty())
                .and_then(|field_name| answers_obj.get(field_name))
                .is_some_and(|value| !is_empty_value(value))
    })
}

fn registration_has_any_declared_input(registration: Option<&Value>, answers: &Value) -> bool {
    let Some(registration_obj) = registration.and_then(Value::as_object) else {
        return false;
    };
    let Some(answers_obj) = answers.as_object() else {
        return false;
    };
    registration_obj.iter().any(|(key, field_value)| {
        key.ends_with("_field")
            && field_value
                .as_str()
                .map(str::trim)
                .filter(|field_name| !field_name.is_empty())
                .and_then(|field_name| answers_obj.get(field_name))
                .is_some_and(|value| !is_empty_value(value))
    })
}

struct SetupActionRegistrationContext<'a> {
    bundle_path: &'a Path,
    discovered: Option<&'a crate::discovery::DiscoveryResult>,
    provider_id: &'a str,
    config: &'a SetupConfig,
    bundle_name: Option<&'a str>,
    public_base_url: Option<&'a str>,
    answers: &'a mut Value,
    actions: &'a mut [crate::setup_actions::SetupAction],
}

fn run_setup_action_registrations(ctx: SetupActionRegistrationContext<'_>) -> anyhow::Result<()> {
    let SetupActionRegistrationContext {
        bundle_path,
        discovered,
        provider_id,
        config,
        bundle_name,
        public_base_url,
        answers,
        actions,
    } = ctx;

    let Some(provider) = discovered.and_then(|d| d.find_setup_target(provider_id)) else {
        if actions
            .iter()
            .any(|action| needs_setup_action_registration(action, answers))
        {
            anyhow::bail!("provider pack not found for setup action registration: {provider_id}");
        }
        return Ok(());
    };

    for action in actions {
        if !needs_setup_action_registration(action, answers) {
            // Registration may already have run in a prior setup pass (its
            // `*_output` fields are already in `answers`), but `action.extra`
            // is rebuilt fresh from the pack's setup.yaml on every run, so an
            // `open_url` action's resolved `url` never carries over. Recompute
            // it from the already-known answers instead of re-invoking
            // registration.
            if action.kind == crate::setup_actions::SetupActionKind::OpenUrl
                && action.extra.get("registration").is_some()
            {
                resolve_open_url_action(action, answers)?;
            }
            continue;
        }
        let registration = action
            .extra
            .get("registration")
            .cloned()
            .ok_or_else(|| anyhow!("setup action registration metadata missing"))?;
        let request = build_registration_request(
            provider_id,
            config,
            bundle_name,
            public_base_url,
            answers,
            action,
            &registration,
        )?;
        let output = invoke_registration_operation(
            bundle_path,
            &provider.pack_path,
            &registration,
            &request,
            config,
        )
        .with_context(|| {
            format!(
                "failed to run setup action registration {} for {}",
                action.id, provider_id
            )
        })?;
        if let Some(error) = registration_error_message(&output) {
            anyhow::bail!(
                "setup action registration {} returned an error: {}",
                action.id,
                error
            );
        }
        merge_registration_output(action, answers, &registration, &output)?;
        match action.kind {
            crate::setup_actions::SetupActionKind::OauthInstallButton
                if client_id_for_action(action, answers).is_none()
                    && !authorize_url_has_query_key(
                        action.authorize_url.as_deref(),
                        "client_id",
                    ) =>
            {
                anyhow::bail!(
                    "setup action registration {} did not produce a client_id",
                    action.id
                );
            }
            crate::setup_actions::SetupActionKind::OpenUrl => {
                resolve_open_url_action(action, answers)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn needs_setup_action_registration(
    action: &crate::setup_actions::SetupAction,
    answers: &Value,
) -> bool {
    if action.extra.get("registration").is_none() {
        return false;
    }
    match action.kind {
        crate::setup_actions::SetupActionKind::OauthInstallButton => {
            client_id_for_action(action, answers).is_none()
                && !authorize_url_has_query_key(action.authorize_url.as_deref(), "client_id")
        }
        crate::setup_actions::SetupActionKind::OpenUrl => {
            registration_output_missing(action, answers)
        }
        _ => false,
    }
}

/// Resolve an `open_url` action's `url_template` (e.g.
/// `https://api.slack.com/apps/{slack_app_id}/install-on-team?`) against the
/// answers merged in by `merge_registration_output`, storing the result as
/// `action.extra["url"]` for the setup UI to render as a plain link.
fn resolve_open_url_action(
    action: &mut crate::setup_actions::SetupAction,
    answers: &Value,
) -> anyhow::Result<()> {
    let Some(template) = action
        .extra
        .get("url_template")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if !template.starts_with("https://") {
        anyhow::bail!(
            "setup action {} url_template must be an https:// URL",
            action.id
        );
    }
    let placeholder = regex::Regex::new(r"\{([A-Za-z0-9_.-]+)\}")
        .expect("static url template placeholder regex is valid");
    let answers_obj = answers.as_object();
    let mut unresolved = false;
    let resolved = placeholder.replace_all(template, |caps: &regex::Captures<'_>| {
        let name = &caps[1];
        let value = answers_obj
            .and_then(|obj| obj.get(name))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match value {
            Some(value) => percent_encode_url_component(value),
            None => {
                unresolved = true;
                String::new()
            }
        }
    });
    if !unresolved {
        action
            .extra
            .insert("url".into(), Value::String(resolved.into_owned()));
    }
    Ok(())
}

/// Percent-encodes a value for embedding in a URL path/query segment,
/// matching JavaScript's `encodeURIComponent` unreserved-character set.
fn percent_encode_url_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn authorize_url_has_query_key(url: Option<&str>, key: &str) -> bool {
    url.and_then(|value| url::Url::parse(value).ok())
        .is_some_and(|parsed| parsed.query_pairs().any(|(candidate, _)| candidate == key))
}

fn build_registration_request(
    provider_id: &str,
    config: &SetupConfig,
    bundle_name: Option<&str>,
    public_base_url: Option<&str>,
    answers: &Value,
    action: &crate::setup_actions::SetupAction,
    registration: &Value,
) -> anyhow::Result<Value> {
    let registration_obj = registration
        .as_object()
        .ok_or_else(|| anyhow!("setup action registration must be an object"))?;
    let answers_obj = answers
        .as_object()
        .ok_or_else(|| anyhow!("provider setup answers must be an object"))?;
    // For an OAuth developer-install action, the app manifest's `redirect_urls`
    // (written by this registration op) MUST equal the authorize `redirect_uri`
    // that `build_oauth_install_url` will later put in the install link — which is
    // the setup server's own callback base (`GREENTIC_SETUP_PUBLIC_BASE_URL`), not
    // the runtime's `public_base_url`. Prefer that base here so Slack sees a
    // redirect_uri it recognizes; otherwise the exchange fails with
    // `bad_redirect_uri` and no bot token is captured.
    let setup_callback_base =
        if action.kind == crate::setup_actions::SetupActionKind::OauthInstallButton {
            std::env::var("GREENTIC_SETUP_PUBLIC_BASE_URL")
                .ok()
                .map(|value| value.trim().trim_end_matches('/').to_string())
                .filter(|value| value.starts_with("https://"))
        } else {
            None
        };
    let effective_public_base_url =
        setup_callback_base
            .as_deref()
            .or(public_base_url)
            .or_else(|| {
                answers_obj
                    .get("public_base_url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            });
    let effective_team = config.team.as_deref().unwrap_or("default");
    let mut input = JsonMap::new();
    input.insert("answers".into(), answers.clone());
    input.insert("provider_id".into(), Value::String(provider_id.to_string()));
    input.insert("tenant".into(), Value::String(config.tenant.clone()));
    input.insert("team".into(), Value::String(effective_team.to_string()));
    if let Some(public_base_url) = effective_public_base_url {
        input.insert(
            "public_base_url".into(),
            Value::String(public_base_url.to_string()),
        );
    }
    input.insert("action_id".into(), Value::String(action.id.clone()));

    for (key, field_value) in registration_obj {
        let Some(input_name) = key.strip_suffix("_field") else {
            continue;
        };
        let Some(field_name) = field_value
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        if let Some(value) = answers_obj
            .get(field_name)
            .filter(|value| !is_empty_value(value))
        {
            input.insert(field_name.to_string(), value.clone());
            input.insert(input_name.to_string(), value.clone());
        }
    }

    if input.get("app_name").is_none()
        && let Some(app_name) = registration_app_name(action, bundle_name)
    {
        input.insert("app_name".into(), Value::String(app_name.clone()));
        if let Some(field_name) = registration_obj
            .get("app_name_field")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            input.insert(field_name.to_string(), Value::String(app_name));
        }
    }

    let mut context = JsonMap::new();
    context.insert("provider_id".into(), Value::String(provider_id.to_string()));
    context.insert("tenant".into(), Value::String(config.tenant.clone()));
    context.insert("team".into(), Value::String(effective_team.to_string()));
    if let Some(public_base_url) = effective_public_base_url {
        context.insert(
            "public_base_url".into(),
            Value::String(public_base_url.to_string()),
        );
    }
    if let Some(app_name) = input.get("app_name") {
        context.insert("app_name".into(), app_name.clone());
    }
    input.insert("context".into(), Value::Object(context));
    Ok(Value::Object(input))
}

fn registration_app_name(
    action: &crate::setup_actions::SetupAction,
    bundle_name: Option<&str>,
) -> Option<String> {
    let bundle_name = bundle_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Greentic");
    if let Some(template) = action
        .extra
        .get("app_name_template")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let rendered = template
            .replace("{{ bundle_name }}", bundle_name)
            .replace("{{bundle_name}}", bundle_name)
            .trim()
            .to_string();
        if !rendered.is_empty() {
            return Some(rendered);
        }
    }
    action
        .extra
        .get("default_app_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
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

/// Resolve the Slack workspace (team) id for the bot token in `answers` via
/// `auth.test`, so `slack_app_url` can be pinned to the right workspace.
/// Returns `None` (best-effort) when there's no token, no `slack_app_url` to
/// enrich, the URL is already pinned, or the call fails.
fn slack_team_id_from_answers(answers: &Value) -> Option<String> {
    let obj = answers.as_object()?;
    let app_url = obj.get("slack_app_url").and_then(Value::as_str)?;
    if !app_url.contains("app_redirect") || app_url.contains("team=") {
        return None;
    }
    let token = obj
        .get("slack_bot_token")
        .or_else(|| obj.get("bot_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| t.starts_with("xoxb"))?;
    let mut response = crate::http_client::api_agent_any_status()
        .post("https://slack.com/api/auth.test")
        .header("Authorization", &format!("Bearer {token}"))
        .send_empty()
        .ok()?;
    let body: Value = response.body_mut().read_json().ok()?;
    if body.get("ok").and_then(Value::as_bool) != Some(true) {
        eprintln!(
            "[oauth-token] auth.test failed: {:?} — Add to Slack link stays unpinned",
            body.get("error").and_then(Value::as_str)
        );
        return None;
    }
    let team = body
        .get("team_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())?
        .to_string();
    eprintln!("[oauth-token] auth.test ok: team_id={team} — pinning Add to Slack link");
    Some(team)
}

/// Pin `slack_app_url` (an `app_redirect` link) to `team` and record
/// `slack_team_id`, so the final "Add to Slack" opens the bot's DM in the right
/// workspace instead of a workspace picker / app-not-found page.
fn apply_slack_team_to_app_url(answers: &mut Value, team: &str) {
    let Some(obj) = answers.as_object_mut() else {
        return;
    };
    if let Some(url) = obj
        .get("slack_app_url")
        .and_then(Value::as_str)
        .filter(|url| url.contains("app_redirect") && !url.contains("team="))
    {
        let separator = if url.contains('?') { '&' } else { '?' };
        let pinned = format!("{url}{separator}team={team}");
        obj.insert("slack_app_url".to_string(), Value::String(pinned));
    }
    obj.entry("slack_team_id".to_string())
        .or_insert_with(|| Value::String(team.to_string()));
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

#[derive(Default)]
struct SetupRegistrationSecrets {
    /// Secrets written by the op during this invocation (e.g. freshly minted
    /// credentials). Takes precedence over the persisted store.
    values: Mutex<BTreeMap<String, Vec<u8>>>,
    /// Read-through to the bundle's persisted dev store so the op can recover
    /// previously-stored secrets on re-runs (e.g. the Slack signing secret,
    /// which Slack only returns when it *creates* an app — not on reuse).
    dev_store: Option<Arc<greentic_secrets_lib::DevStore>>,
}

impl SetupRegistrationSecrets {
    fn with_dev_store(dev_store: Option<Arc<greentic_secrets_lib::DevStore>>) -> Self {
        Self {
            values: Mutex::new(BTreeMap::new()),
            dev_store,
        }
    }
}

/// Candidate secret URIs to try when reading a previously-stored secret.
///
/// A secret URI is `secrets://<env>/<tenant>/<team>/<provider>/<key>`, and two
/// segments are normalized inconsistently across persist/read sites:
/// - **env** is branched between the current default [`crate::DEFAULT_ENV_ID`]
///   (`local`) and the legacy [`crate::LEGACY_ENV_ID`] (`dev`); a value stored
///   under one may be read under the other.
/// - **provider** is branched between the hyphenated pack id (`messaging-slack`)
///   and the underscore-normalized form the store uses (`messaging_slack`).
///
/// Emit the path as-is plus the env × provider variants so reuse can recover a
/// value regardless of which normalization was used when it was written.
fn secret_uri_candidates(path: &str) -> Vec<String> {
    let mut out = vec![path.to_string()];
    let Some(rest) = path.strip_prefix("secrets://") else {
        return out;
    };
    let segs: Vec<&str> = rest.splitn(5, '/').collect();
    if segs.len() != 5 {
        return out;
    }
    let (env, tenant, team, provider, key) = (segs[0], segs[1], segs[2], segs[3], segs[4]);

    let env_variants: Vec<&str> = if env == crate::DEFAULT_ENV_ID {
        vec![crate::DEFAULT_ENV_ID, crate::LEGACY_ENV_ID]
    } else if env == crate::LEGACY_ENV_ID {
        vec![crate::LEGACY_ENV_ID, crate::DEFAULT_ENV_ID]
    } else {
        vec![env]
    };
    let mut provider_variants = vec![provider.to_string()];
    for alt in [provider.replace('-', "_"), provider.replace('_', "-")] {
        if alt != provider && !provider_variants.contains(&alt) {
            provider_variants.push(alt);
        }
    }

    for env in &env_variants {
        for provider in &provider_variants {
            let candidate = format!("secrets://{env}/{tenant}/{team}/{provider}/{key}");
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

#[async_trait::async_trait]
impl greentic_secrets_lib::SecretsManager for SetupRegistrationSecrets {
    async fn read(&self, path: &str) -> greentic_secrets_lib::Result<Vec<u8>> {
        {
            let values = self.values.lock().map_err(|_| {
                greentic_secrets_lib::SecretError::Backend(
                    "setup component secrets lock poisoned".into(),
                )
            })?;
            if let Some(value) = values.get(path) {
                return Ok(value.clone());
            }
        }
        if let Some(store) = &self.dev_store {
            use greentic_secrets_lib::SecretsStore;
            for candidate in secret_uri_candidates(path) {
                match store.get(&candidate).await {
                    Ok(bytes) => return Ok(bytes),
                    Err(err) => {
                        tracing::debug!(candidate, error = %err, "setup secrets read miss");
                    }
                }
            }
        }
        Err(greentic_secrets_lib::SecretError::NotFound(
            path.to_string(),
        ))
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
    // greentic-runner-host's provider-core-only enforcement defaults to ON and
    // denies every component secrets_store::get/put BEFORE consulting the secrets
    // manager. greentic-start disables it at startup; mirror that here or
    // registration ops can never read stored secrets (e.g. Slack's signing secret
    // or configuration tokens on app reuse) and fail with misleading
    // "<secret> is required" errors.
    unsafe { std::env::set_var("GREENTIC_PROVIDER_CORE_ONLY", "0") };

    // Back the setup-component secrets with the bundle's persisted dev store so
    // registration ops can recover previously-stored secrets on re-runs (e.g. the
    // Slack signing secret on app reuse). Best-effort: an unreadable store just
    // means the op sees only what it writes this invocation (prior behavior).
    let reg_env = crate::resolve_env(Some(&config.env));
    let dev_store = crate::secrets::open_dev_store_for_env(bundle_path, &reg_env)
        .ok()
        .map(Arc::new);
    let secrets: greentic_runner_host::secrets::DynSecretsManager =
        Arc::new(SetupRegistrationSecrets::with_dev_store(dev_store));
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

fn merge_registration_output(
    action: &mut crate::setup_actions::SetupAction,
    answers: &mut Value,
    registration: &Value,
    output: &Value,
) -> anyhow::Result<()> {
    let registration_obj = registration
        .as_object()
        .ok_or_else(|| anyhow!("setup action registration must be an object"))?;
    let output_obj = output
        .as_object()
        .ok_or_else(|| anyhow!("setup action registration output must be an object"))?;
    let answers_obj = answers
        .as_object_mut()
        .ok_or_else(|| anyhow!("provider setup answers must be an object"))?;

    for (mapping_key, source_value) in registration_obj {
        let Some(generic_key) = mapping_key.strip_suffix("_output") else {
            continue;
        };
        let Some(source_key) = source_value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let Some(value) = output_obj
            .get(source_key)
            .or_else(|| output_obj.get(generic_key))
            .filter(|value| !is_empty_value(value))
            .cloned()
        else {
            continue;
        };
        answers_obj.insert(source_key.to_string(), value.clone());
        answers_obj.insert(generic_key.to_string(), value.clone());
        if generic_key == "client_id" {
            if let Some(client_id_field) =
                action.extra.get("client_id_field").and_then(Value::as_str)
            {
                answers_obj.insert(client_id_field.to_string(), value.clone());
            }
            action.extra.insert("client_id".into(), value);
        } else {
            action.extra.insert(generic_key.to_string(), value);
        }
    }
    Ok(())
}

fn registration_error_message(output: &Value) -> Option<String> {
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        return output
            .get("error")
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or_else(|| Some(output.to_string()));
    }
    None
}

fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

fn hydrate_oauth_install_actions(
    actions: &mut [crate::setup_actions::SetupAction],
    answers: &Value,
) {
    for action in actions {
        if action.kind != crate::setup_actions::SetupActionKind::OauthInstallButton {
            continue;
        }
        let client_id = client_id_for_action(action, answers);
        let Some(authorize_url) = action.authorize_url.as_mut() else {
            continue;
        };
        let Ok(mut parsed) = url::Url::parse(authorize_url) else {
            continue;
        };
        if !parsed.query_pairs().any(|(key, _)| key == "client_id")
            && let Some(client_id) = client_id
        {
            parsed
                .query_pairs_mut()
                .append_pair("client_id", &client_id);
        }
        if !parsed.query_pairs().any(|(key, _)| key == "scope")
            && let Some(scopes) = action.extra.get("scopes").and_then(Value::as_array)
        {
            let scope = scopes
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(",");
            if !scope.is_empty() {
                parsed.query_pairs_mut().append_pair("scope", &scope);
            }
        }
        *authorize_url = parsed.to_string();
    }
}

fn client_id_for_action(
    action: &crate::setup_actions::SetupAction,
    answers: &Value,
) -> Option<String> {
    let obj = answers.as_object()?;
    let mut keys = Vec::new();
    if let Some(field) = action.extra.get("client_id_field").and_then(Value::as_str) {
        keys.push(field);
    }
    keys.extend(["client_id", "oauth_client_id"]);
    keys.into_iter().find_map(|key| {
        obj.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
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

    #[test]
    fn secret_uri_candidates_cover_env_and_provider_branches() {
        // Stored under the current default env + underscore provider; a read that
        // resolves to the legacy env + hyphenated provider must still find it.
        let cands =
            secret_uri_candidates("secrets://dev/demo/_/messaging-slack/slack_signing_secret");
        assert!(
            cands
                .contains(&"secrets://dev/demo/_/messaging-slack/slack_signing_secret".to_string())
        );
        assert!(
            cands.contains(
                &"secrets://local/demo/_/messaging_slack/slack_signing_secret".to_string()
            )
        );
        assert!(
            cands.contains(
                &"secrets://local/demo/_/messaging-slack/slack_signing_secret".to_string()
            )
        );
        assert!(
            cands
                .contains(&"secrets://dev/demo/_/messaging_slack/slack_signing_secret".to_string())
        );
        // First candidate is always the path as-is.
        assert_eq!(
            cands[0],
            "secrets://dev/demo/_/messaging-slack/slack_signing_secret"
        );
    }

    #[test]
    fn secret_uri_candidates_do_not_alias_custom_env() {
        let cands = secret_uri_candidates("secrets://prod/acme/team/messaging_slack/bot_token");
        // Provider still branches, but a non-default/non-legacy env is never
        // remapped to dev/local.
        assert!(cands.contains(&"secrets://prod/acme/team/messaging_slack/bot_token".to_string()));
        assert!(cands.contains(&"secrets://prod/acme/team/messaging-slack/bot_token".to_string()));
        assert!(
            !cands
                .iter()
                .any(|c| c.contains("/dev/") || c.contains("/local/"))
        );
    }

    #[test]
    fn secret_uri_candidates_passthrough_non_secret_paths() {
        assert_eq!(
            secret_uri_candidates("SLACK_BOT_TOKEN"),
            vec!["SLACK_BOT_TOKEN".to_string()]
        );
    }

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

    fn secret_keys_for(keys: &[&str]) -> BTreeSet<String> {
        keys.iter()
            .map(|k| crate::secret_name::canonical_secret_name(k))
            .collect()
    }

    #[test]
    fn envelope_redaction_replaces_secret_values_with_canonical_uri_refs() {
        let secret_keys = secret_keys_for(&["api_key", "oauth_client_secret"]);

        let answers = serde_json::json!({
            "model": "gpt-4o-mini",
            "api_key": "sk-PLAINTEXT-MUST-NOT-LEAK",
            "oauth_client_secret": "PLAINTEXT-OAUTH-SECRET",
            "non_secret_url": "https://api.openai.com/v1"
        });

        let redacted = redact_secret_answer_values_to_uri_refs(
            &answers,
            &secret_keys,
            "dev",
            "demo",
            Some("default"),
            "openai-llm",
        );

        let map = redacted.as_object().expect("object");
        assert_eq!(map["model"].as_str(), Some("gpt-4o-mini"));
        assert_eq!(
            map["non_secret_url"].as_str(),
            Some("https://api.openai.com/v1")
        );
        // `canonical_secret_uri` collapses the literal "default" team into
        // the wildcard segment `_` (via `greentic_secrets_lib::normalize_team`).
        assert_eq!(
            map["api_key"].as_str(),
            Some("secrets://dev/demo/_/openai_llm/api_key"),
            "secret value must be replaced with canonical secrets:// URI",
        );
        assert_eq!(
            map["oauth_client_secret"].as_str(),
            Some("secrets://dev/demo/_/openai_llm/oauth_client_secret"),
        );

        let json = serde_json::to_string(&redacted).expect("serialize");
        assert!(
            !json.contains("PLAINTEXT-MUST-NOT-LEAK"),
            "api_key plaintext leaked into envelope JSON: {json}",
        );
        assert!(
            !json.contains("PLAINTEXT-OAUTH-SECRET"),
            "oauth_client_secret plaintext leaked into envelope JSON: {json}",
        );
    }

    #[test]
    fn setup_answers_redaction_drops_secret_keys_entirely() {
        // setup-answers.json's downstream readers in greentic-start skip
        // secret-marked keys (PR #179) and fetch from `SecretsManager`
        // instead, so the producer drops them from this artifact — no
        // value or URI ref appears in the JSON value slot.
        let secret_keys = secret_keys_for(&["api_key"]);
        let answers = serde_json::json!({
            "model": "gpt-4o-mini",
            "api_key": "sk-PLAINTEXT-MUST-NOT-LEAK"
        });

        let stripped = strip_secret_answer_keys(&answers, &secret_keys);
        let map = stripped.as_object().expect("object");
        assert_eq!(map["model"].as_str(), Some("gpt-4o-mini"));
        assert!(
            !map.contains_key("api_key"),
            "secret key must be removed entirely from setup-answers",
        );
        let json = serde_json::to_string(&stripped).expect("serialize");
        assert!(
            !json.contains("PLAINTEXT-MUST-NOT-LEAK"),
            "plaintext leaked into setup-answers: {json}",
        );
        assert!(
            !json.contains("secrets://"),
            "setup-answers must not carry URI refs either — readers fetch via SecretsManager",
        );
    }

    #[test]
    fn is_secret_answer_key_matches_aliases_via_canonical_suffix() {
        // Mirrors `qa::persist::seed_secret_requirement_aliases` (Codex
        // F3): a `webex_bot_token` requirement is satisfied by an answer
        // key `bot_token`, so redaction must match it too (forward direction:
        // secret key ends with answer key).
        let secret_keys = secret_keys_for(&["webex_bot_token"]);
        assert!(is_secret_answer_key("bot_token", &secret_keys));
        assert!(is_secret_answer_key("BOT_TOKEN", &secret_keys));
        assert!(is_secret_answer_key("webex_bot_token", &secret_keys));
        // Non-aliases must not match.
        assert!(!is_secret_answer_key("model", &secret_keys));
        assert!(!is_secret_answer_key("bot_url", &secret_keys));
    }

    #[test]
    fn is_secret_answer_key_does_not_over_match_reverse_direction() {
        // xhigh review C4: the previous symmetric `norm.ends_with(secret)`
        // direction over-matched. A pack whose ONLY secret is the short key
        // `token` must NOT cause an unrelated longer answer `bot_token` to be
        // redacted — `seed_secret_requirement_aliases` would not seed it
        // either (it matches `requirement.ends_with(answer)`, not the
        // reverse), so redaction must stay consistent and leave it alone.
        let secret_keys = secret_keys_for(&["token"]);
        assert!(is_secret_answer_key("token", &secret_keys));
        assert!(
            !is_secret_answer_key("bot_token", &secret_keys),
            "answer key longer than the secret key must not match (reverse direction removed)",
        );
        assert!(!is_secret_answer_key("refresh_token", &secret_keys));
    }

    #[test]
    fn is_secret_answer_key_punctuation_only_key_does_not_match_unrelated_secret() {
        // `canonical_secret_name` maps empty/punctuation-only keys to the
        // sentinel "secret"; it must not collide with an unrelated secret
        // key like `api_key`.
        let secret_keys = secret_keys_for(&["api_key"]);
        assert!(!is_secret_answer_key("", &secret_keys));
        assert!(!is_secret_answer_key("---", &secret_keys));
    }

    #[test]
    fn alias_answer_key_redacted_in_setup_answers_and_envelope() {
        // End-to-end check for Codex F3: requirement `webex_bot_token`,
        // operator-supplied key `bot_token`.
        let secret_keys = secret_keys_for(&["webex_bot_token"]);
        let answers = serde_json::json!({"bot_token": "T0K3N-MUST-NOT-LEAK"});

        let stripped = strip_secret_answer_keys(&answers, &secret_keys);
        assert!(
            stripped.as_object().unwrap().is_empty(),
            "alias-matched secret key must be dropped from setup-answers",
        );

        let envelope = redact_secret_answer_values_to_uri_refs(
            &answers,
            &secret_keys,
            "dev",
            "demo",
            None,
            "messaging-webex",
        );
        assert_eq!(
            envelope["bot_token"].as_str(),
            Some("secrets://dev/demo/_/messaging_webex/bot_token"),
        );
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(!json.contains("T0K3N-MUST-NOT-LEAK"));
    }

    #[test]
    fn secret_keys_fail_closed_distinguishes_none_from_empty_set() {
        let content = serde_json::json!({"model": "gpt-4o"});
        let empty = serde_json::json!({});

        let found = true;
        let known: Vec<String> = vec![];

        // xhigh review C3: a pack WITH a form spec that declares zero secrets
        // resolves to Some(empty) and MUST proceed (write all answers as
        // non-secret) — not bail.
        let r = secret_keys_or_fail_closed(Some(BTreeSet::new()), &content, "p", found, &known)
            .unwrap();
        assert!(r.is_empty(), "Some(empty) proceeds with no redaction");

        // Some(nonempty) passes the set through.
        let set = secret_keys_for(&["api_key"]);
        let r =
            secret_keys_or_fail_closed(Some(set.clone()), &content, "p", found, &known).unwrap();
        assert_eq!(r, set);

        // None + content => fail closed (can't classify, won't risk plaintext).
        assert!(secret_keys_or_fail_closed(None, &content, "p", found, &known).is_err());

        // None + empty answers => nothing to leak, proceed.
        assert!(
            secret_keys_or_fail_closed(None, &empty, "p", found, &known)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn missing_pack_reports_the_key_mismatch_not_missing_metadata() {
        let content = serde_json::json!({"api_key": "x"});
        let known = vec![
            "greentic.events.webhook".to_string(),
            "messaging-telegram".to_string(),
        ];

        // The answers name a pack that is not in the bundle. Blaming the pack's
        // METADATA here is what misleads people — the key simply does not match
        // any pack_id. Name the ids we do have so the fix is obvious.
        let err = secret_keys_or_fail_closed(None, &content, "events-webhook", false, &known)
            .expect_err("a pack we do not have must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("no pack in this bundle has pack_id `events-webhook`"),
            "must say the pack_id did not match, got: {msg}",
        );
        assert!(
            msg.contains("greentic.events.webhook"),
            "must list the pack_ids we DO have, got: {msg}",
        );
        assert!(
            !msg.contains("ships no classifiable setup metadata"),
            "must not blame the pack's metadata when the pack was never found, got: {msg}",
        );

        // A pack that IS present but ships nothing classifiable still gets the
        // original fail-closed message — that one is accurate.
        let err = secret_keys_or_fail_closed(None, &content, "events-webhook", true, &known)
            .expect_err("present-but-unclassifiable must still fail closed");
        assert!(
            err.to_string()
                .contains("ships no classifiable setup metadata")
        );

        // Empty answers still proceed even when the pack is missing: nothing to leak.
        assert!(
            secret_keys_or_fail_closed(None, &serde_json::json!({}), "nope", false, &known)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn answers_have_content_distinguishes_empty_from_meaningful() {
        assert!(!answers_have_content(&serde_json::json!({})));
        assert!(!answers_have_content(&serde_json::json!({"a": null})));
        assert!(!answers_have_content(&serde_json::json!({"a": ""})));
        assert!(answers_have_content(&serde_json::json!({"a": "value"})));
        assert!(answers_have_content(&serde_json::json!({"a": 42})));
        assert!(answers_have_content(&serde_json::json!({"a": true})));
        assert!(answers_have_content(&serde_json::json!({"a": ["x"]})));
    }
}
