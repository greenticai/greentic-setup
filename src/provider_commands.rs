//! `greentic-setup provider {add,list,remove}` — one-command provider wiring.
//!
//! Porcelain over the deployer's messaging-endpoint library API: resolves the
//! provider pack, collects setup answers (interactive or headless), writes
//! secrets into the env's dev store (the store the runtime reads), registers
//! the messaging endpoint, and links it to a deployed bundle.
//!
//! Secrets are written via the deployer's `secrets::put` (env-level dev store
//! at `<env_dir>/.greentic/dev/.dev.secrets.env`) — NOT the setup-native
//! `SecretsSetup` (bundle-scoped store the runtime never reads). The
//! endpoint's `secret_refs` carry `secret://` URIs (deployer convention, no
//! trailing `s`) so the runtime can resolve them.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::bootstrap::{LocalEnvOutcome, ensure_local_environment};
use greentic_deployer::cli::dispatch::print_outcome;
use greentic_deployer::cli::messaging::{
    EndpointAddPayload, EndpointLinkBundlePayload, EndpointRemovePayload,
};
use greentic_deployer::cli::secrets::SecretsPutPayload;
use greentic_deployer::cli::{OpFlags, messaging, secrets};
use greentic_deployer::environment::LocalFsStore;

use crate::bundle_source::BundleSource;
use crate::cli_args::ProviderAddArgs;
use crate::provider_registry::{self, ProviderPackInfo};
use crate::secrets::load_secret_requirements_from_pack;
use crate::setup_input::{self, SetupInputAnswers};

/// The `updated_by` identity stamped on every mutation this module performs.
const UPDATED_BY: &str = "greentic-setup";

/// No-op flags for direct deployer library calls (no `--schema`, no
/// `--answers` file — we supply payloads programmatically).
fn op_flags() -> OpFlags {
    OpFlags {
        schema_only: false,
        answers: None,
    }
}

// ---------------------------------------------------------------------------
// Pack resolution
// ---------------------------------------------------------------------------

/// Resolve a provider pack to a local `.gtpack` path.
///
/// Resolution order:
/// 1. `--pack <path>` explicit override.
/// 2. OCI fetch from GHCR.
/// 3. Offline fallback: pack already inside the deployed bundle in this env.
///
/// `pack_version` overrides the OCI tag (the `--pack-version` escape hatch).
/// It is ignored when `explicit_pack` is `Some` (the `--pack` flag wins).
pub fn resolve_pack(
    explicit_pack: Option<&Path>,
    info: &ProviderPackInfo,
    store: &LocalFsStore,
    env_id: &str,
    pack_version: Option<&str>,
) -> Result<PathBuf> {
    // 1. Explicit override.
    if let Some(pack) = explicit_pack {
        if !pack.exists() {
            bail!("pack path does not exist: {}", pack.display());
        }
        eprintln!("Resolved pack: {} (local override)", pack.display());
        return Ok(pack.to_path_buf());
    }

    // 2. OCI fetch.
    let oci_ref = provider_registry::oci_reference(info, pack_version);
    match BundleSource::parse(&oci_ref) {
        Ok(source) => match source.resolve() {
            Ok(path) => {
                eprintln!("Resolved pack: {oci_ref} -> {}", path.display());
                return Ok(path);
            }
            Err(err) => {
                // When the user explicitly requested a version, do not
                // silently fall back to an arbitrary old pack — the offline
                // fallback has no version awareness and would return
                // whichever pack happened to be deployed previously.
                if pack_version.is_some() {
                    bail!(
                        "OCI fetch failed for {oci_ref}: {err:#}\n\
                         The --pack-version override requires a successful \
                         OCI fetch. Supply --pack <path> to use a local file."
                    );
                }
                eprintln!(
                    "Warning: OCI fetch failed for {oci_ref}: {err:#}\n\
                     Falling back to bundle-embedded pack."
                );
            }
        },
        Err(err) => {
            if pack_version.is_some() {
                bail!(
                    "OCI reference could not be parsed ({oci_ref}): {err:#}\n\
                     The --pack-version override requires a valid OCI reference."
                );
            }
            eprintln!(
                "Warning: OCI reference could not be parsed ({oci_ref}): {err:#}\n\
                 Falling back to bundle-embedded pack."
            );
        }
    }

    // 3. Offline fallback: scan deployed revisions for the pack.
    let env_dir = store.root().join(env_id);
    if env_dir.is_dir()
        && let Some(path) = find_pack_in_revisions(&env_dir, info.pack_name)
    {
        eprintln!(
            "Resolved pack: {} (offline fallback from deployed revision)",
            path.display()
        );
        return Ok(path);
    }

    bail!(
        "could not resolve provider pack `{}`.\n\
         Tried:\n  \
         1. OCI: {oci_ref}\n  \
         2. Offline: pack not found in any deployed revision.\n\n\
         Supply --pack <path> to point at a local .gtpack file.",
        info.pack_name,
    )
}

/// Scan `<env_dir>/revisions/*/bundle/packs/` for a pack matching `pack_name`.
fn find_pack_in_revisions(env_dir: &Path, pack_name: &str) -> Option<PathBuf> {
    let revisions_dir = env_dir.join("revisions");
    let entries = std::fs::read_dir(&revisions_dir).ok()?;
    for entry in entries.flatten() {
        let candidate = entry
            .path()
            .join("bundle")
            .join("packs")
            .join(format!("{pack_name}.gtpack"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Secret URI helpers
// ---------------------------------------------------------------------------

/// Normalize a provider segment for secret URIs/paths. Mirrors the runtime's
/// `normalize_pack_segment` (greentic-runner-host/src/secrets.rs): lowercase,
/// keep `a-z 0-9 _ -`. Unlike `canonical_secret_name` (which maps hyphens to
/// underscores), this preserves hyphens so that `messaging-telegram` stays
/// `messaging-telegram`.
fn normalize_provider_segment(raw: &str) -> String {
    let s: String = raw
        .chars()
        .map(|ch| {
            let ch = ch.to_ascii_lowercase();
            match ch {
                'a'..='z' | '0'..='9' | '_' | '-' => ch,
                _ => '_',
            }
        })
        .collect();
    if s.is_empty() {
        "provider".to_string()
    } else {
        s
    }
}

/// Build a `secret://` URI (deployer convention, no trailing `s`) from
/// components. The provider segment is normalized with
/// [`normalize_provider_segment`] (hyphens preserved) to match the runtime's
/// `normalize_pack_segment`. The key segment uses `canonical_secret_name`
/// (hyphens → underscores) to match the runtime's `canonicalize_secret_key`.
fn deployer_secret_uri(
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider: &str,
    key: &str,
) -> String {
    let rel_path = deployer_secret_path(tenant, team, provider, key);
    format!("secret://{env}/{rel_path}")
}

/// Build the relative path portion (everything after `secret://<env>/`) for
/// the deployer's `SecretsPutPayload.path`. Provider segment uses
/// [`normalize_provider_segment`]; key segment uses `canonical_secret_name`.
/// Team segment uses [`greentic_secrets_lib::normalize_team`] (canonical
/// source of truth for the empty / `"default"` → `_` rule).
fn deployer_secret_path(tenant: &str, team: Option<&str>, provider: &str, key: &str) -> String {
    let team_segment = greentic_secrets_lib::normalize_team(team)
        .unwrap_or_else(|| greentic_secrets_lib::TEAM_PLACEHOLDER.to_string());
    let normalized_provider = normalize_provider_segment(provider);
    let normalized_key = crate::secret_name::canonical_secret_name(key);
    format!("{tenant}/{team_segment}/{normalized_provider}/{normalized_key}")
}

// ---------------------------------------------------------------------------
// Bundle-id auto-detection
// ---------------------------------------------------------------------------

/// Auto-detect the bundle id from the environment. Returns `Ok(id)` when
/// exactly one bundle is deployed; returns an actionable error otherwise.
///
/// Reads `environment.json` directly from the store's on-disk layout rather
/// than going through `EnvironmentReads` (which requires `EnvId`, a type
/// from `greentic-deploy-spec` that is not in our dependency graph).
pub fn auto_detect_bundle_id(store: &LocalFsStore, env_id_str: &str) -> Result<String> {
    let env_json_path = store.root().join(env_id_str).join("environment.json");
    if !env_json_path.is_file() {
        bail!(
            "no bundle is deployed in environment `{env_id_str}`.\n\
             Deploy a bundle first: gtc start <bundle>"
        );
    }
    let raw = std::fs::read_to_string(&env_json_path)
        .with_context(|| format!("read {}", env_json_path.display()))?;
    let doc: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", env_json_path.display()))?;
    let all_bundles = doc
        .get("bundles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    // Only consider active bundles (or entries without a status field, for
    // backward compat). Archived / Paused bundles are not valid link targets.
    //
    // `BundleDeploymentStatus` is `#[serde(rename_all = "lowercase")]`, so the
    // on-disk value is `"active"` — match case-insensitively rather than
    // against a hand-guessed casing.
    let bundles: Vec<_> = all_bundles
        .into_iter()
        .filter(|b| {
            b.get("status")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.eq_ignore_ascii_case("active"))
        })
        .collect();
    match bundles.len() {
        0 => bail!(
            "no active bundle is deployed in environment `{env_id_str}`.\n\
             Deploy a bundle first: gtc start <bundle>"
        ),
        1 => {
            let id = bundles[0]
                .get("bundle_id")
                .and_then(|v| v.as_str())
                .context("bundle entry missing bundle_id")?;
            Ok(id.to_string())
        }
        n => {
            let ids: Vec<String> = bundles
                .iter()
                .filter_map(|b| b.get("bundle_id").and_then(|v| v.as_str()))
                .map(|id| format!("  - {id}"))
                .collect();
            bail!(
                "{n} active bundles deployed in environment `{env_id_str}`. \
                 Pass --bundle-id to choose one:\n{}",
                ids.join("\n"),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Mutation core — reusable by `add()` and answers-driven auto-deploy
// ---------------------------------------------------------------------------

/// Payload for [`register_provider_core`].
pub struct RegisterProviderPayload<'a> {
    pub env_id: &'a str,
    pub tenant: &'a str,
    pub team: Option<&'a str>,
    pub provider_type: &'a str,
    pub provider_id: &'a str,
    pub pack_name: &'a str,
    pub display_name: String,
    pub bundle_id: &'a str,
    pub link_bundle: bool,
    pub answers: &'a serde_json::Map<String, serde_json::Value>,
    pub pack_path: &'a Path,
}

/// Register a messaging endpoint, write its secrets, and optionally link a
/// bundle. This is the mutation core shared by `add()` (interactive) and
/// answers-driven auto-deploy (non-interactive).
///
/// Ordering invariant: bundle_id is resolved BEFORE this function is called
/// (the caller must verify it exists). Inside, the sequence is:
/// 1. register endpoint (fail-fast on duplicate)
/// 2. write secrets
/// 3. link bundle
///
/// When `idempotency_key` is `Some`, the deployer's replay logic makes
/// same-identity re-runs no-ops. When `None`, the deployer mints a fresh
/// UUID (legacy interactive path).
pub fn register_provider_core(
    store: &LocalFsStore,
    payload: &RegisterProviderPayload<'_>,
    idempotency_key: Option<String>,
) -> Result<String> {
    // ── Build secret entries ─────────────────────────────────────────
    let mut secret_keys = collect_secret_keys_from_pack(payload.pack_path);
    for req in load_secret_requirements_from_pack(payload.pack_path).unwrap_or_default() {
        secret_keys.insert(req.key.clone());
        secret_keys.insert(crate::secret_name::canonical_secret_name(&req.key));
    }

    let mut secret_entries: Vec<(String, String, String, String)> = Vec::new();

    for (key, value) in payload.answers {
        let is_secret = secret_keys.contains(key)
            || secret_keys.contains(&crate::secret_name::canonical_secret_name(key));
        if !is_secret {
            continue;
        }
        let secret_value = match value.as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let rel_path = deployer_secret_path(payload.tenant, payload.team, payload.pack_name, key);
        let uri = deployer_secret_uri(
            payload.env_id,
            payload.tenant,
            payload.team,
            payload.pack_name,
            key,
        );
        secret_entries.push((key.clone(), secret_value, rel_path, uri));
    }

    let secret_refs: Vec<String> = secret_entries
        .iter()
        .map(|(_, _, _, uri)| uri.clone())
        .collect();

    // ── Register the messaging endpoint ──────────────────────────────
    let add_result = messaging::add(
        store,
        &op_flags(),
        Some(EndpointAddPayload {
            environment_id: payload.env_id.to_string(),
            provider_id: payload.provider_id.to_string(),
            provider_type: payload.provider_type.to_string(),
            display_name: payload.display_name.clone(),
            secret_refs,
            webhook_secret_ref: None,
            idempotency_key,
            updated_by: UPDATED_BY.to_string(),
        }),
    )
    .map_err(|e| anyhow::anyhow!("add messaging endpoint: {e}"))?;

    let endpoint_id = add_result
        .result
        .get("endpoint_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .context("add outcome missing endpoint_id")?;

    eprintln!("Endpoint registered: {endpoint_id}");
    print_outcome(&add_result).ok();

    // ── Write secrets ────────────────────────────────────────────────
    for (key, value, rel_path, uri) in &secret_entries {
        let put_result = secrets::put(
            store,
            &op_flags(),
            Some(SecretsPutPayload {
                environment_id: payload.env_id.to_string(),
                path: rel_path.clone(),
                value: value.clone(),
                idempotency_key: None,
            }),
        );
        match put_result {
            Ok(_outcome) => {
                eprintln!("  Secret written: {uri}");
            }
            Err(e) => {
                bail!(
                    "failed to write secret `{key}` to env store: {e}\n\n\
                     The endpoint `{endpoint_id}` was already registered. \
                     To clean up, run:\n  \
                     greentic-setup provider remove {endpoint_id} --env {payload_env}",
                    payload_env = payload.env_id,
                );
            }
        }
    }

    // ── Link bundle ──────────────────────────────────────────────────
    if payload.link_bundle {
        let link_result = messaging::link_bundle(
            store,
            &op_flags(),
            Some(EndpointLinkBundlePayload {
                environment_id: payload.env_id.to_string(),
                endpoint_id: endpoint_id.clone(),
                bundle_id: payload.bundle_id.to_string(),
                idempotency_key: None,
                updated_by: UPDATED_BY.to_string(),
            }),
        )
        .map_err(|e| anyhow::anyhow!("link bundle `{}` to endpoint: {e}", payload.bundle_id))?;
        print_outcome(&link_result).ok();
    }

    Ok(endpoint_id)
}

/// Derive a deterministic idempotency key from (env_id, provider_type,
/// provider_id). Re-running the same answers.json against the same
/// environment naturally replays as a no-op in the deployer's endpoint
/// engine, which is idempotent on same-key same-identity replays.
pub fn deterministic_idempotency_key(
    env_id: &str,
    provider_type: &str,
    provider_id: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"greentic-setup:provider:");
    hasher.update(env_id.as_bytes());
    hasher.update(b":");
    hasher.update(provider_type.as_bytes());
    hasher.update(b":");
    hasher.update(provider_id.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("setup-provider-{hex}")
}

// ---------------------------------------------------------------------------
// provider add
// ---------------------------------------------------------------------------

/// `greentic-setup provider add <KIND>`.
pub fn add(
    args: &ProviderAddArgs,
    env_id: &str,
    tenant: &str,
    team: Option<&str>,
    dry_run: bool,
    non_interactive: bool,
    answers_path: Option<&Path>,
) -> Result<()> {
    let kind = args.kind.to_ascii_lowercase();
    let info = provider_registry::lookup(&kind).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown provider kind `{}`. Known kinds: {}",
            kind,
            provider_registry::known_kinds().join(", "),
        )
    })?;

    // ── Build the deployer store ───────────────────────────────────────
    let root = LocalFsStore::default_root()
        .context("cannot locate the environment store: HOME / USERPROFILE not set")?;
    let store = LocalFsStore::new(root);

    // Ensure / validate the target environment exists.
    if env_id == "local" {
        let (_env, outcome) = ensure_local_environment(&store, None)
            .map_err(|e| anyhow::anyhow!("ensure local environment: {e}"))?;
        if matches!(outcome, LocalEnvOutcome::Created) {
            eprintln!("Created environment `local`.");
        }
    } else {
        let env_json = store.root().join(env_id).join("environment.json");
        if !env_json.is_file() {
            bail!(
                "environment `{env_id}` does not exist.\n\
                 Create it first, or use --env local."
            );
        }
    }

    // ── Resolve the provider pack ─────────────────────────────────────
    let pack_path = resolve_pack(
        args.pack.as_deref(),
        info,
        &store,
        env_id,
        args.pack_version.as_deref(),
    )?;
    eprintln!("Using provider pack: {}", pack_path.display());

    // ── Resolve provider id (used by the device-code check and all
    //    downstream steps) ────────────────────────────────────────────
    let provider_id = args
        .provider_id
        .as_deref()
        .unwrap_or(info.default_provider_id);

    // ── Check for teams device-code flow (before collecting answers) ──
    if has_oauth_device_code_action(&pack_path) {
        bail!(
            "provider `teams` requires an OAuth device-code flow that is currently only \
             available through the full bundle-setup engine.\n\n\
             Use the existing two-step path instead:\n  \
             1. greentic-setup bundle add {pack} --bundle <bundle-dir>\n  \
             2. greentic-setup bundle setup teams --bundle <bundle-dir>\n\n\
             Then register the endpoint manually:\n  \
             gtc op messaging endpoint add --env {env} --provider-type teams \
             --provider-id {pid} --display-name \"Teams\" --updated-by greentic-setup\n\n\
             `provider add teams` will be supported once the device-code flow is \
             callable outside the bundle-setup engine.",
            pack = pack_path.display(),
            env = env_id,
            pid = provider_id,
        );
    }

    let setup_input = if let Some(path) = answers_path {
        let raw = setup_input::load_setup_input(path)
            .with_context(|| format!("load answers from {}", path.display()))?;
        let keys = std::collections::BTreeSet::new();
        Some(SetupInputAnswers::new(raw, keys)?)
    } else {
        None
    };

    let answers = setup_input::collect_setup_answers(
        &pack_path,
        provider_id,
        setup_input.as_ref(),
        !non_interactive,
    )
    .context("collect setup answers")?;

    if dry_run {
        eprintln!(
            "Dry run: would register provider `{provider_id}` (type: {}) in env `{env_id}`.",
            info.provider_type
        );
        let json = serde_json::to_string_pretty(&answers).context("serialize dry-run answers")?;
        println!("{json}");
        return Ok(());
    }

    // ── Resolve the link target BEFORE any mutation ───────────────────
    let bundle_id = if let Some(id) = &args.bundle_id {
        id.clone()
    } else {
        auto_detect_bundle_id(&store, env_id)?
    };

    let display_name = args
        .display_name
        .clone()
        .unwrap_or_else(|| crate::setup_to_formspec::capitalize(info.kind));

    let answers_map = answers.as_object().cloned().unwrap_or_default();

    let _endpoint_id = register_provider_core(
        &store,
        &RegisterProviderPayload {
            env_id,
            tenant,
            team,
            provider_type: info.provider_type,
            provider_id,
            pack_name: info.pack_name,
            display_name,
            bundle_id: &bundle_id,
            link_bundle: true,
            answers: &answers_map,
            pack_path: &pack_path,
        },
        None, // auto-minted (interactive path)
    )?;

    // ── Closing message ───────────────────────────────────────────────
    eprintln!();
    eprintln!(
        "Provider `{provider_id}` (type: {ptype}) is registered in environment `{env_id}` \
         and linked to bundle `{bundle_id}`.",
        ptype = info.provider_type,
    );
    eprintln!("A running runtime (`gtc start`) will pick up the new endpoint on its next reload.");

    // Warn about webhook registration if no public base URL is resolvable.
    if !has_resolvable_public_url(&store, env_id) {
        eprintln!();
        eprintln!(
            "Note: no public base URL is configured for environment `{env_id}`.\n\
             Webhook registration will be skipped until one is set.\n\
             Options:\n  \
             - Start a tunnel (the runtime auto-detects it)\n  \
             - Set PUBLIC_BASE_URL in the environment\n  \
             - Run: gtc op env set-public-url {env_id} <url>"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// provider list
// ---------------------------------------------------------------------------

/// `greentic-setup provider list`.
pub fn list(env_id: &str) -> Result<()> {
    let root = LocalFsStore::default_root()
        .context("cannot locate the environment store: HOME / USERPROFILE not set")?;
    let store = LocalFsStore::new(root);

    let outcome = messaging::list(&store, &op_flags(), env_id)
        .map_err(|e| anyhow::anyhow!("list messaging endpoints: {e}"))?;
    print_outcome(&outcome)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// provider remove
// ---------------------------------------------------------------------------

/// `greentic-setup provider remove <ID>`.
pub fn remove(endpoint_id: &str, env_id: &str) -> Result<()> {
    let root = LocalFsStore::default_root()
        .context("cannot locate the environment store: HOME / USERPROFILE not set")?;
    let store = LocalFsStore::new(root);

    let outcome = messaging::remove(
        &store,
        &op_flags(),
        Some(EndpointRemovePayload {
            environment_id: env_id.to_string(),
            endpoint_id: endpoint_id.to_string(),
            idempotency_key: None,
            updated_by: UPDATED_BY.to_string(),
        }),
    )
    .map_err(|e| anyhow::anyhow!("remove messaging endpoint: {e}"))?;
    print_outcome(&outcome)?;

    eprintln!(
        "Endpoint `{endpoint_id}` removed from environment `{env_id}`.\n\
         Note: secrets associated with this endpoint were NOT deleted. \
         Remove them manually if no longer needed:\n  \
         gtc op secrets list --answers '{{\"environment_id\":\"{env_id}\"}}'"
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check whether the pack's `setup.yaml` declares an `oauth_device_code` setup
/// action (used by teams-graph).
fn has_oauth_device_code_action(pack_path: &Path) -> bool {
    let spec = match setup_input::load_setup_spec(pack_path) {
        Ok(Some(spec)) => spec,
        _ => return false,
    };
    spec.setup_actions.iter().any(|action| {
        action
            .get("kind")
            .and_then(|v| v.as_str())
            .is_some_and(|k| k == "oauth_device_code")
    })
}

/// Collect the set of question names marked `secret: true` in the pack's
/// `setup.yaml`.
fn collect_secret_keys_from_pack(pack_path: &Path) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    if let Ok(Some(spec)) = setup_input::load_setup_spec(pack_path) {
        for q in &spec.questions {
            if q.secret {
                keys.insert(q.name.clone());
                keys.insert(crate::secret_name::canonical_secret_name(&q.name));
            }
        }
    }
    keys
}

/// Check whether a public base URL is resolvable for the given environment.
fn has_resolvable_public_url(store: &LocalFsStore, env_id: &str) -> bool {
    // Check environment variable first.
    if std::env::var("PUBLIC_BASE_URL")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return true;
    }

    // Check the environment's host config from environment.json.
    let env_json_path = store.root().join(env_id).join("environment.json");
    let Ok(raw) = std::fs::read_to_string(&env_json_path) else {
        return false;
    };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    doc.get("host_config")
        .and_then(|hc| hc.get("public_base_url"))
        .and_then(|v| v.as_str())
        .is_some_and(|url| !url.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_provider_segment_preserves_hyphens() {
        // Must match the runtime's normalize_pack_segment behavior.
        assert_eq!(
            normalize_provider_segment("messaging-telegram"),
            "messaging-telegram"
        );
        assert_eq!(
            normalize_provider_segment("MESSAGING-SLACK"),
            "messaging-slack"
        );
        assert_eq!(normalize_provider_segment("telegram"), "telegram");
    }

    #[test]
    fn normalize_provider_segment_maps_dots_and_spaces() {
        assert_eq!(
            normalize_provider_segment("my.provider name"),
            "my_provider_name"
        );
    }

    #[test]
    fn normalize_provider_segment_empty() {
        assert_eq!(normalize_provider_segment(""), "provider");
    }

    #[test]
    fn deployer_secret_uri_basic() {
        let uri = deployer_secret_uri("local", "demo", None, "telegram", "bot_token");
        assert_eq!(uri, "secret://local/demo/_/telegram/bot_token");
    }

    #[test]
    fn deployer_secret_uri_preserves_hyphens_in_provider() {
        // The provider segment (pack name) must preserve hyphens to match
        // the runtime's normalize_pack_segment.
        let uri = deployer_secret_uri("local", "demo", None, "messaging-telegram", "bot_token");
        assert_eq!(uri, "secret://local/demo/_/messaging-telegram/bot_token");
    }

    #[test]
    fn deployer_secret_uri_with_team() {
        let uri = deployer_secret_uri("local", "acme", Some("ops"), "slack", "token");
        assert_eq!(uri, "secret://local/acme/ops/slack/token");
    }

    #[test]
    fn deployer_secret_uri_default_team() {
        let uri = deployer_secret_uri("local", "demo", Some("default"), "webex", "bot_token");
        assert_eq!(uri, "secret://local/demo/_/webex/bot_token");
    }

    #[test]
    fn deployer_secret_path_basic() {
        let path = deployer_secret_path("demo", None, "telegram", "bot_token");
        assert_eq!(path, "demo/_/telegram/bot_token");
    }

    #[test]
    fn deployer_secret_path_preserves_hyphens_in_provider() {
        let path = deployer_secret_path("demo", None, "messaging-telegram", "bot_token");
        assert_eq!(path, "demo/_/messaging-telegram/bot_token");
    }

    #[test]
    fn auto_detect_bundle_id_missing_env() {
        let root = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        let result = auto_detect_bundle_id(&store, "nonexistent");
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        // No environment.json at all — the early bail says "no bundle".
        assert!(msg.contains("no bundle"), "got: {msg}");
    }

    #[test]
    fn auto_detect_bundle_id_empty_bundles() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = root.path().join("local");
        std::fs::create_dir_all(&env_dir).unwrap();
        let env_json = serde_json::json!({
            "schema": "greentic.environment.v1",
            "environment_id": "local",
            "bundles": [],
        });
        std::fs::write(
            env_dir.join("environment.json"),
            serde_json::to_string_pretty(&env_json).unwrap(),
        )
        .unwrap();
        let store = LocalFsStore::new(root.path());
        let result = auto_detect_bundle_id(&store, "local");
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("no active bundle"), "got: {msg}");
    }

    #[test]
    fn auto_detect_bundle_id_one_bundle() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = root.path().join("local");
        std::fs::create_dir_all(&env_dir).unwrap();
        let env_json = serde_json::json!({
            "schema": "greentic.environment.v1",
            "environment_id": "local",
            "bundles": [{"bundle_id": "my-bundle"}],
        });
        std::fs::write(
            env_dir.join("environment.json"),
            serde_json::to_string_pretty(&env_json).unwrap(),
        )
        .unwrap();
        let store = LocalFsStore::new(root.path());
        let result = auto_detect_bundle_id(&store, "local").unwrap();
        assert_eq!(result, "my-bundle");
    }

    #[test]
    fn auto_detect_bundle_id_skips_archived() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = root.path().join("local");
        std::fs::create_dir_all(&env_dir).unwrap();
        let env_json = serde_json::json!({
            "schema": "greentic.environment.v1",
            "environment_id": "local",
            // Lowercase, exactly as `BundleDeploymentStatus`
            // (`#[serde(rename_all = "lowercase")]`) writes it to
            // environment.json. A capitalised fixture here would pass while the
            // real store silently matched nothing.
            "bundles": [
                {"bundle_id": "active-bundle", "status": "active"},
                {"bundle_id": "old-bundle", "status": "archived"},
            ],
        });
        std::fs::write(
            env_dir.join("environment.json"),
            serde_json::to_string_pretty(&env_json).unwrap(),
        )
        .unwrap();
        let store = LocalFsStore::new(root.path());
        // Only the Active bundle should be auto-detected.
        let result = auto_detect_bundle_id(&store, "local").unwrap();
        assert_eq!(result, "active-bundle");
    }

    #[test]
    fn auto_detect_bundle_id_multiple_bundles() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = root.path().join("local");
        std::fs::create_dir_all(&env_dir).unwrap();
        let env_json = serde_json::json!({
            "schema": "greentic.environment.v1",
            "environment_id": "local",
            "bundles": [
                {"bundle_id": "bundle-a"},
                {"bundle_id": "bundle-b"},
            ],
        });
        std::fs::write(
            env_dir.join("environment.json"),
            serde_json::to_string_pretty(&env_json).unwrap(),
        )
        .unwrap();
        let store = LocalFsStore::new(root.path());
        let result = auto_detect_bundle_id(&store, "local");
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("--bundle-id"), "got: {msg}");
        assert!(msg.contains("bundle-a"), "got: {msg}");
        assert!(msg.contains("bundle-b"), "got: {msg}");
    }

    // -- resolve_pack precedence tests (no network) --------------------------

    #[test]
    fn resolve_pack_explicit_path_wins() {
        // Precedence rule: --pack (explicit path) wins over OCI and fallback.
        let root = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        let info = provider_registry::lookup("telegram").unwrap();

        // Create a fake pack file.
        let pack_file = root.path().join("my.gtpack");
        std::fs::write(&pack_file, b"fake").unwrap();

        let result = resolve_pack(Some(&pack_file), info, &store, "local", None);
        assert_eq!(result.unwrap(), pack_file);
    }

    #[test]
    fn resolve_pack_explicit_path_missing_errors() {
        let root = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        let info = provider_registry::lookup("telegram").unwrap();

        let missing = root.path().join("nonexistent.gtpack");
        let result = resolve_pack(Some(&missing), info, &store, "local", None);
        assert!(result.is_err());
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("does not exist"), "got: {msg}");
    }

    #[test]
    fn find_pack_in_revisions_finds_pack() {
        // Test the offline fallback scan directly (no network dependency).
        let root = tempfile::tempdir().unwrap();
        let env_dir = root.path().join("local");

        // Set up the revision directory structure the fallback expects.
        let pack_dir = env_dir
            .join("revisions")
            .join("rev-001")
            .join("bundle")
            .join("packs");
        std::fs::create_dir_all(&pack_dir).unwrap();
        let pack_file = pack_dir.join("messaging-telegram.gtpack");
        std::fs::write(&pack_file, b"fake-pack").unwrap();

        let found = find_pack_in_revisions(&env_dir, "messaging-telegram");
        assert_eq!(found.unwrap(), pack_file);
    }

    #[test]
    fn find_pack_in_revisions_returns_none_when_missing() {
        let root = tempfile::tempdir().unwrap();
        let env_dir = root.path().join("local");
        std::fs::create_dir_all(env_dir.join("revisions")).unwrap();

        let found = find_pack_in_revisions(&env_dir, "messaging-telegram");
        assert!(found.is_none());
    }

    #[test]
    fn resolve_pack_no_source_available_errors() {
        // Use a tag that will never exist on the registry so OCI always fails,
        // regardless of whether GHCR is reachable.
        let root = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        let info = provider_registry::lookup("telegram").unwrap();

        let result = resolve_pack(
            None,
            info,
            &store,
            "local",
            Some("nonexistent-tag-for-testing"),
        );
        assert!(result.is_err(), "expected error, got: {result:?}");
        let msg = format!("{:#}", result.unwrap_err());
        assert!(
            msg.contains("OCI"),
            "error should mention OCI attempt: {msg}"
        );
        assert!(
            msg.contains("--pack"),
            "error should suggest --pack escape hatch: {msg}"
        );
    }

    #[test]
    fn resolve_pack_version_override_rejects_offline_fallback() {
        // When the user explicitly passes --pack-version, OCI failure must NOT
        // silently fall back to an arbitrary old pack from a deployed revision.
        // The function must error so the user knows the requested version could
        // not be fetched.
        let root = tempfile::tempdir().unwrap();
        let store = LocalFsStore::new(root.path());
        let info = provider_registry::lookup("telegram").unwrap();

        // Plant a pack in a deployed revision so the offline fallback *would*
        // find it.
        let pack_dir = root
            .path()
            .join("local")
            .join("revisions")
            .join("rev-old")
            .join("bundle")
            .join("packs");
        std::fs::create_dir_all(&pack_dir).unwrap();
        std::fs::write(
            pack_dir.join(format!("{}.gtpack", info.pack_name)),
            b"old-pack",
        )
        .unwrap();

        let result = resolve_pack(
            None,
            info,
            &store,
            "local",
            Some("nonexistent-tag-for-testing"),
        );
        assert!(
            result.is_err(),
            "should error when --pack-version is set and OCI fails, \
             not silently use an offline fallback; got: {result:?}"
        );
    }

    #[test]
    fn resolve_pack_version_override_reflected_in_oci_ref() {
        // Verify the version override flows into the OCI reference. We cannot
        // observe the OCI reference directly from resolve_pack (it's internal),
        // so we test via the registry helper.
        let info = provider_registry::lookup("telegram").unwrap();
        let ref_default = provider_registry::oci_reference(info, None);
        let ref_pinned = provider_registry::oci_reference(info, Some("0.5.17"));

        assert!(ref_default.ends_with(":stable"));
        assert!(ref_pinned.ends_with(":0.5.17"));
    }
}
