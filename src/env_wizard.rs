//! Bare `--env` wizard (operator-surface PR-6): author or gap-fill a
//! `greentic.env-manifest.v1` manifest interactively, persist it, and hand
//! off to the deployer's env-apply engine via [`crate::env_mode`].
//!
//! `greentic-setup --env demo` (explicit `--env`, no bundle positional, no
//! `--answers`, TTY) drives the deployer-owned `manifest_form_spec()`
//! through the existing FormSpec prompt loop
//! ([`crate::qa::prompts::prompt_form_spec_answers_with_existing`] —
//! advanced gating and table rows come free), converts the answers with
//! the deployer's `answers_to_manifest`, writes the manifest file (the
//! durable artifact — commit it), and runs the engine: plan rows, TTY
//! confirmation, execute. Re-running against an existing manifest
//! pre-loads it via [`manifest_to_answers`]: satisfied questions are kept
//! (gap-fill mode); for full edits, hand-edit the file and re-apply with
//! `--answers <file>`.
//!
//! Secrets are the exception to the pure-form flow: this wizard drops the
//! generic `secrets` table from the prompt loop and handles it *last* as a
//! derived step. Once the bundles are known it reads each bundle's packs'
//! `secret-requirements.json` (via the deployer's
//! [`greentic_deployer::runtime_secrets::bundle_secret_requirements`],
//! scoped to the bundle's route tenant) and asks only for the env-var NAME
//! of each required secret — never the path (auto-derived), never the value
//! (apply reads it from the env / dev-store). So the operator only enters
//! the secrets the configured bundles actually need.
//!
//! Basic vs advanced: by default the wizard asks only the everyday fields.
//! The optional row columns an operator almost always leaves empty
//! (`customer_id`, `config_overrides`, `route_hosts` on bundles; the
//! `welcome_*` trio and `secret_refs` on endpoints) are hidden until
//! `--advanced` is passed — the same flag that already reveals the optional
//! top-level questions. The route path/tenant/team and endpoint links stay
//! in the basic flow; the common multi-bundle setup needs them.

use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::env_manifest::{
    ENV_MANIFEST_FORM_ID, ENV_MANIFEST_FORM_VERSION, EnvManifest, ManifestBundle,
    TrustRootDirective, answers_to_manifest, manifest_form_spec,
};
use greentic_deployer::runtime_secrets::{bundle_secret_requirements, manifest_secret_path};
use qa_spec::{AnswerSet, FormSpec};
use serde_json::{Map as JsonMap, Value, json};

use crate::env_mode;
use crate::qa::prompts::prompt_form_spec_answers_with_existing;

/// Wizard entry: prompt for the manifest path, pre-load or seed the
/// answers, run the form, persist the manifest, and hand off to the
/// engine. `dry_run` stops after the plan preview; otherwise the engine's
/// own TTY confirmation gates execution (no second confirm here).
pub fn run_env_wizard(
    env: &str,
    advanced: bool,
    dry_run: bool,
    non_interactive: bool,
) -> Result<()> {
    if non_interactive || !std::io::stdin().is_terminal() {
        bail!(
            "the environment wizard is interactive; in headless runs pass an env manifest via \
             --answers <file> (generate a skeleton with `gtc op env apply --emit-answers-template`)"
        );
    }
    let manifest_path = prompt_manifest_path(env)?;
    let initial = load_initial_answers(&manifest_path, env)?;

    // Drive the shared form WITHOUT the secrets section: this terminal
    // wizard owns secrets as a final, derived step — it asks only for the
    // secrets the configured bundles actually declare, and only for the
    // env-var NAME of each (never the path, never the value).
    let spec = manifest_form_spec();
    // Basic flow hides the advanced-only row columns; `--advanced` reveals
    // them (mirrors the existing top-level optional-question gating).
    let spec = spec_for_mode(&spec, advanced);
    let form_spec = spec_without_question(&spec, "secrets");
    if !advanced {
        println!(
            "\nBasic mode — pass --advanced to also set customer id, config \
             overrides, route hosts, welcome flow, and endpoint secret refs."
        );
    }
    let prompted = prompt_form_spec_answers_with_existing(
        &form_spec,
        "environment",
        advanced,
        &Value::Object(initial),
    )?;
    let mut answers = prompted.as_object().cloned().unwrap_or_default();

    // Pre-loaded from_env values (editing an existing manifest) become the
    // per-secret defaults so a re-run doesn't re-ask what hasn't changed.
    let existing_from_env = existing_from_env_by_path(&answers);

    // Relative bundle paths resolve against the manifest file's directory,
    // exactly like the apply engine.
    let manifest_dir = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    // Derive + prompt secrets from the bundles just authored. A provisional
    // manifest gives the typed bundle list; any pre-loaded secrets in it are
    // ignored (recomputed below).
    let provisional = answers_to_manifest(&answer_set(answers.clone()))?;
    let secret_rows =
        derive_and_prompt_secrets(&manifest_dir, env, &provisional.bundles, &existing_from_env)?;
    if secret_rows.is_empty() {
        answers.remove("secrets");
    } else {
        answers.insert("secrets".to_string(), Value::Array(secret_rows));
    }

    let manifest = answers_to_manifest(&answer_set(answers))?;

    let doc = serde_json::to_value(&manifest)?;
    let mut rendered = serde_json::to_string_pretty(&doc)?;
    rendered.push('\n');
    std::fs::write(&manifest_path, rendered)
        .with_context(|| format!("failed to write `{}`", manifest_path.display()))?;
    println!(
        "\nWrote `{}` — the manifest is the durable artifact; keep it in version control.",
        manifest_path.display()
    );

    env_mode::run_env_apply(&manifest_path, &doc, env, dry_run, false)
}

/// The env-manifest answer-set wrapper (form id + version) around a raw
/// answers map.
fn answer_set(answers: JsonMap<String, Value>) -> AnswerSet {
    AnswerSet {
        form_id: ENV_MANIFEST_FORM_ID.to_string(),
        spec_version: ENV_MANIFEST_FORM_VERSION.to_string(),
        answers: Value::Object(answers),
        meta: None,
    }
}

/// Clone of `spec` with the question whose id is `id` removed. The terminal
/// wizard handles `secrets` itself (derived), so it is dropped from the
/// prompt loop while remaining in the shared spec for other front-ends.
fn spec_without_question(spec: &FormSpec, id: &str) -> FormSpec {
    let mut reduced = spec.clone();
    reduced.questions.retain(|question| question.id != id);
    reduced
}

/// Optional `List` row columns hidden from the basic (non-`--advanced`)
/// flow, keyed by the owning list question id. They map to manifest fields
/// an operator almost always leaves empty. Everything not listed here — and
/// every required column — stays in the basic flow, notably the route
/// path/tenant/team and endpoint links the common multi-bundle setup needs.
const ADVANCED_LIST_COLUMNS: &[(&str, &[&str])] = &[
    (
        "bundles",
        &["customer_id", "config_overrides", "route_hosts"],
    ),
    (
        "messaging_endpoints",
        &[
            "welcome_bundle_id",
            "welcome_pack_id",
            "welcome_flow_id",
            "secret_refs",
        ],
    ),
];

/// Clone of `spec` with the advanced-only row columns
/// ([`ADVANCED_LIST_COLUMNS`]) removed from each `List` question, so the
/// basic flow asks only the everyday fields. A no-op when `advanced` is
/// true. Hiding at the spec level (like [`spec_without_question`]) keeps the
/// shared prompt loop generic — it never learns which columns are advanced.
fn spec_for_mode(spec: &FormSpec, advanced: bool) -> FormSpec {
    if advanced {
        return spec.clone();
    }
    let mut reduced = spec.clone();
    for question in &mut reduced.questions {
        let Some(hidden) = ADVANCED_LIST_COLUMNS
            .iter()
            .find(|(id, _)| *id == question.id)
            .map(|(_, columns)| *columns)
        else {
            continue;
        };
        if let Some(list) = question.list.as_mut() {
            list.fields
                .retain(|field| !hidden.contains(&field.id.as_str()));
        }
    }
    reduced
}

/// `path -> from_env` from a pre-loaded `secrets` answer array, used as
/// per-secret defaults when editing an existing manifest.
fn existing_from_env_by_path(answers: &JsonMap<String, Value>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(Value::Array(rows)) = answers.get("secrets") {
        for row in rows {
            if let (Some(path), Some(from_env)) = (
                row.get("path").and_then(Value::as_str),
                row.get("from_env").and_then(Value::as_str),
            ) {
                map.insert(path.to_string(), from_env.to_string());
            }
        }
    }
    map
}

/// Tenant a bundle's route binding selects (defaulting to `default`) — the
/// scope the dev-store secret path is built under.
fn bundle_tenant(bundle: &ManifestBundle) -> String {
    bundle
        .route_binding
        .as_ref()
        .and_then(|binding| binding.tenant_selector.as_ref())
        .map(|selector| selector.tenant.clone())
        .unwrap_or_else(|| "default".to_string())
}

/// Suggested env-var name for a derived secret: `<TENANT>_<KEY>` (upper), or
/// just `<KEY>` for the default tenant. A hint only — the operator types the
/// actual variable name.
fn default_env_var_name(tenant: &str, key: &str) -> String {
    /// Replace non-ASCII-alphanumeric chars with `_` and uppercase — produces
    /// a POSIX-safe env-var name fragment from an arbitrary identifier.
    fn sanitize(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect()
    }
    let key = sanitize(key);
    if tenant.is_empty() || tenant.eq_ignore_ascii_case("default") {
        key
    } else {
        format!("{}_{}", sanitize(tenant), key)
    }
}

/// One secret a configured bundle declares, with the context needed to
/// prompt for its env-var name.
struct DerivedSecret {
    /// Manifest secret path `<tenant>/<team>/<pack>/<name>`.
    path: String,
    /// Pack/provider that declared it (for display).
    provider_id: String,
    /// Canonical secret key (drives the default env-var name).
    key: String,
    /// Tenant the path is scoped to.
    tenant: String,
    /// Whether the declaring pack marked it required.
    required: bool,
    /// Bundle ids that need this same secret (display only).
    bundle_ids: Vec<String>,
}

/// Read each bundle's packs' `secret-requirements.json` (via the deployer's
/// [`bundle_secret_requirements`]) and collect the unique secrets they
/// declare, in first-seen order, deduplicated by manifest path. Bundles
/// whose artifact (or built `packs/`) is missing are skipped with a note and
/// reported via the returned `skipped` flag — the wizard never hard-fails
/// just because a bundle has not been built yet.
fn derive_required_secrets(
    manifest_dir: &Path,
    env: &str,
    bundles: &[ManifestBundle],
) -> (Vec<DerivedSecret>, bool) {
    let mut order: Vec<String> = Vec::new();
    let mut by_path: BTreeMap<String, DerivedSecret> = BTreeMap::new();
    let mut skipped = false;

    for bundle in bundles {
        let tenant = bundle_tenant(bundle);
        // Revision-based bundles carry no `.gtbundle` path, so there is no
        // artifact to scan for secret requirements — skip them quietly (the
        // wizard authors path-based bundles; this only arises when pre-loading
        // an existing manifest).
        let Some(bundle_path) = bundle.bundle_path.as_ref() else {
            continue;
        };
        let artifact = if bundle_path.is_absolute() {
            bundle_path.clone()
        } else {
            manifest_dir.join(bundle_path)
        };
        let Some(bundle_root) = artifact.parent() else {
            skipped = true;
            continue;
        };
        if !artifact.exists() {
            eprintln!(
                "  note: bundle `{}` artifact `{}` not found — build it before the \
                 wizard to auto-detect its secrets (skipping)",
                bundle.bundle_id,
                artifact.display()
            );
            skipped = true;
            continue;
        }
        let requirements = match bundle_secret_requirements(bundle_root, env, &tenant) {
            Ok(requirements) => requirements,
            Err(err) => {
                eprintln!(
                    "  note: could not read secrets for bundle `{}`: {err} (skipping)",
                    bundle.bundle_id
                );
                skipped = true;
                continue;
            }
        };
        for requirement in requirements {
            let Some(path) = manifest_secret_path(&requirement.uri, env) else {
                continue;
            };
            match by_path.get_mut(&path) {
                Some(existing) => {
                    existing.required |= requirement.required;
                    existing.bundle_ids.push(bundle.bundle_id.clone());
                }
                None => {
                    order.push(path.clone());
                    by_path.insert(
                        path.clone(),
                        DerivedSecret {
                            path,
                            provider_id: requirement.provider_id,
                            key: requirement.key,
                            tenant: tenant.clone(),
                            required: requirement.required,
                            bundle_ids: vec![bundle.bundle_id.clone()],
                        },
                    );
                }
            }
        }
    }

    let derived = order
        .into_iter()
        .map(|path| by_path.remove(&path).expect("path was just inserted"))
        .collect();
    (derived, skipped)
}

/// Derive the secrets the configured bundles need and prompt for the env-var
/// NAME of each, returning `secrets[]` answer rows (`{path, from_env}`) to
/// merge into the manifest answers.
///
/// When a bundle was skipped (unbuilt/unreadable) any pre-loaded secrets not
/// re-derived are preserved as-is, so a partial edit never silently drops a
/// secret the wizard couldn't recompute.
fn derive_and_prompt_secrets(
    manifest_dir: &Path,
    env: &str,
    bundles: &[ManifestBundle],
    existing_from_env: &BTreeMap<String, String>,
) -> Result<Vec<Value>> {
    let (derived, skipped) = derive_required_secrets(manifest_dir, env, bundles);

    // When a bundle was skipped, pre-loaded secrets we couldn't recompute are
    // preserved rather than dropped (see the tail of this fn).
    let preserving = skipped && !existing_from_env.is_empty();
    if derived.is_empty() && !preserving {
        println!("\nSecrets — the configured bundles declare no secrets; nothing to enter.");
        return Ok(Vec::new());
    }

    if !derived.is_empty() {
        println!(
            "\nSecrets — the configured bundles need {} secret(s).",
            derived.len()
        );
        println!("Enter the NAME of the environment variable that holds each value.");
        println!("(The value itself is never written to the manifest; apply reads it");
        println!(" from the environment / dev-store at apply time.)");
    }

    let mut rows = Vec::with_capacity(derived.len());
    let mut taken = BTreeMap::new();
    for secret in &derived {
        let default = existing_from_env
            .get(&secret.path)
            .cloned()
            .unwrap_or_else(|| default_env_var_name(&secret.tenant, &secret.key));
        println!();
        println!(
            "  {} — {} (bundle: {}){}",
            secret.key,
            secret.provider_id,
            secret.bundle_ids.join(", "),
            if secret.required { "" } else { " [optional]" }
        );
        println!("  secret path: {}", secret.path);
        let from_env = prompt_env_var_name(&default)?;
        taken.insert(secret.path.clone(), ());
        rows.push(json!({ "path": secret.path.clone(), "from_env": from_env }));
    }

    // Preserve pre-loaded secrets the wizard couldn't recompute (a bundle was
    // skipped), so editing a manifest without rebuilt bundles is non-destructive.
    if skipped {
        for (path, from_env) in existing_from_env {
            if taken.contains_key(path) {
                continue;
            }
            eprintln!("  note: keeping existing secret `{path}` (bundle not rebuilt)");
            rows.push(json!({ "path": path, "from_env": from_env }));
        }
    }

    Ok(rows)
}

/// Prompt for one env-var name, defaulting to `default` on empty input.
/// Re-prompts only if both the input and the default are blank.
fn prompt_env_var_name(default: &str) -> Result<String> {
    loop {
        print!("  > env var name [{default}]: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        let n = std::io::stdin().read_line(&mut line)?;
        if n == 0 {
            bail!("unexpected end of input while prompting for env var name");
        }
        let trimmed = line.trim();
        let value = if trimmed.is_empty() { default } else { trimmed };
        if value.is_empty() {
            println!("  An environment variable name is required.");
            continue;
        }
        return Ok(value.to_string());
    }
}

/// Ask where the manifest lives (and will be written). Empty input takes
/// the conventional default `./<env>.env.json`.
fn prompt_manifest_path(env: &str) -> Result<PathBuf> {
    let default = format!("./{env}.env.json");
    print!("Manifest file [{default}]: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let trimmed = line.trim();
    Ok(PathBuf::from(if trimmed.is_empty() {
        default.as_str()
    } else {
        trimmed
    }))
}

/// Initial answers for the prompt loop.
///
/// Missing file → a fresh map seeded with `environment_id` (the user
/// already said it via `--env`; don't re-ask). Existing env manifest →
/// its answers via [`manifest_to_answers`], after the same env
/// cross-check the apply path enforces. Any other existing file → error;
/// the wizard never overwrites a document it doesn't own.
fn load_initial_answers(path: &Path, env: &str) -> Result<JsonMap<String, Value>> {
    if !path.exists() {
        let mut map = JsonMap::new();
        map.insert("environment_id".to_string(), Value::String(env.to_string()));
        return Ok(map);
    }
    let Some(doc) = env_mode::sniff_env_manifest(path) else {
        bail!(
            "`{}` exists and is not a greentic.env-manifest.v1 document; refusing to overwrite \
             it — pick another path or remove the file",
            path.display()
        );
    };
    let manifest: EnvManifest = serde_json::from_value(doc)
        .with_context(|| format!("`{}` is not a valid env manifest", path.display()))?;
    if manifest.environment.id != env {
        bail!(
            "`{}` targets environment `{}` but --env resolves to `{env}`; pass `--env {}` to \
             edit it (the manifest is never silently overridden)",
            path.display(),
            manifest.environment.id,
            manifest.environment.id,
        );
    }
    println!(
        "Editing `{}` — existing answers are kept; only missing ones are asked.",
        path.display()
    );
    let answers = manifest_to_answers(&manifest)?;
    Ok(answers
        .answers
        .as_object()
        .cloned()
        .expect("manifest_to_answers always produces an Object"))
}

/// Inverse of the deployer's `answers_to_manifest` — manifest → wizard
/// answers, for pre-loading an existing manifest into the prompt loop.
///
/// Mirrors the deployer's conventions exactly: every `Vec<String>`
/// manifest field (`links`, `route_hosts`, `route_path_prefixes`,
/// `secret_refs`) renders as a comma-separated string, and
/// `config_overrides` renders as its JSON text — `Some({})` stays the
/// load-bearing "explicit clear", distinct from absent. The round-trip
/// (manifest → answers → `answers_to_manifest`) is pinned by tests, so a
/// new manifest field that misses this converter fails in CI, not in an
/// operator's edit session.
pub fn manifest_to_answers(manifest: &EnvManifest) -> Result<AnswerSet> {
    let mut map = JsonMap::new();
    map.insert(
        "environment_id".to_string(),
        Value::String(manifest.environment.id.clone()),
    );
    if let Some(url) = &manifest.environment.public_base_url {
        map.insert("public_base_url".to_string(), Value::String(url.clone()));
    }
    map.insert(
        "trust_root_bootstrap".to_string(),
        Value::Bool(match manifest.trust_root {
            Some(TrustRootDirective::Bootstrap) => true,
            None => false,
        }),
    );
    if !manifest.secrets.is_empty() {
        let rows = manifest
            .secrets
            .iter()
            .map(|s| json!({"path": s.path, "from_env": s.from_env}))
            .collect();
        map.insert("secrets".to_string(), Value::Array(rows));
    }
    if !manifest.bundles.is_empty() {
        let rows = manifest
            .bundles
            .iter()
            .map(|b| -> Result<Value> {
                let mut row = JsonMap::new();
                row.insert("bundle_id".to_string(), Value::String(b.bundle_id.clone()));
                // Render the bundle path only when present. A path-less
                // (revision-based) bundle has no `bundle_path` form field;
                // `revisions`/`revenue_share`/`status` have no form questions
                // either, so the path-based wizard simply round-trips them as
                // their `answers_to_manifest` defaults (None).
                if let Some(path) = &b.bundle_path {
                    row.insert(
                        "bundle_path".to_string(),
                        Value::String(path.display().to_string()),
                    );
                }
                if let Some(customer) = &b.customer_id {
                    row.insert("customer_id".to_string(), Value::String(customer.clone()));
                }
                if let Some(overrides) = &b.config_overrides {
                    row.insert(
                        "config_overrides".to_string(),
                        Value::String(serde_json::to_string(overrides)?),
                    );
                }
                if let Some(binding) = &b.route_binding {
                    if !binding.hosts.is_empty() {
                        row.insert(
                            "route_hosts".to_string(),
                            Value::String(binding.hosts.join(", ")),
                        );
                    }
                    if !binding.path_prefixes.is_empty() {
                        row.insert(
                            "route_path_prefixes".to_string(),
                            Value::String(binding.path_prefixes.join(", ")),
                        );
                    }
                    if let Some(selector) = &binding.tenant_selector {
                        row.insert(
                            "route_tenant".to_string(),
                            Value::String(selector.tenant.clone()),
                        );
                        row.insert(
                            "route_team".to_string(),
                            Value::String(selector.team.clone()),
                        );
                    }
                }
                Ok(Value::Object(row))
            })
            .collect::<Result<Vec<_>>>()?;
        map.insert("bundles".to_string(), Value::Array(rows));
    }
    if !manifest.messaging_endpoints.is_empty() {
        let rows = manifest
            .messaging_endpoints
            .iter()
            .map(|ep| {
                let mut row = JsonMap::new();
                row.insert("name".to_string(), Value::String(ep.name.clone()));
                row.insert(
                    "provider_type".to_string(),
                    Value::String(ep.provider_type.clone()),
                );
                if !ep.links.is_empty() {
                    row.insert("links".to_string(), Value::String(ep.links.join(", ")));
                }
                if let Some(flow) = &ep.welcome_flow {
                    row.insert(
                        "welcome_bundle_id".to_string(),
                        Value::String(flow.bundle_id.clone()),
                    );
                    row.insert(
                        "welcome_pack_id".to_string(),
                        Value::String(flow.pack_id.clone()),
                    );
                    row.insert(
                        "welcome_flow_id".to_string(),
                        Value::String(flow.flow_id.clone()),
                    );
                }
                if !ep.secret_refs.is_empty() {
                    row.insert(
                        "secret_refs".to_string(),
                        Value::String(ep.secret_refs.join(", ")),
                    );
                }
                Value::Object(row)
            })
            .collect();
        map.insert("messaging_endpoints".to_string(), Value::Array(rows));
    }
    Ok(AnswerSet {
        form_id: ENV_MANIFEST_FORM_ID.to_string(),
        spec_version: ENV_MANIFEST_FORM_VERSION.to_string(),
        answers: Value::Object(map),
        meta: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_deployer::cli::bundles::{RouteBindingPayload, TenantSelectorPayload};
    use greentic_deployer::cli::env_manifest::{
        ENV_MANIFEST_SCHEMA_V1, ManifestBundle, ManifestEndpoint, ManifestEnvironment,
        ManifestSecret, ManifestWelcomeFlow,
    };
    use std::collections::BTreeMap;

    fn full_manifest() -> EnvManifest {
        EnvManifest {
            schema: ENV_MANIFEST_SCHEMA_V1.to_string(),
            environment: ManifestEnvironment {
                id: "demo".to_string(),
                public_base_url: Some("https://demo.example.com".to_string()),
                // Form-less env fields: `answers_to_manifest` always produces
                // None, so the round-trip only holds when these start None.
                name: None,
                region: None,
                tenant_org_id: None,
                listen_addr: None,
            },
            trust_root: Some(TrustRootDirective::Bootstrap),
            secrets: vec![ManifestSecret {
                path: "default/_/messaging-telegram/telegram_bot_token".to_string(),
                from_env: "DEMO_BOT_TOKEN".to_string(),
            }],
            bundles: vec![ManifestBundle {
                bundle_id: "realbot".to_string(),
                bundle_path: Some(PathBuf::from("./bundles/realbot.gtbundle")),
                // Revision/billing/status fields have no form questions;
                // `answers_to_manifest` defaults them to None.
                revisions: None,
                revenue_share: None,
                status: None,
                customer_id: Some("acme".to_string()),
                config_overrides: Some(BTreeMap::from([(
                    "pack-a".to_string(),
                    BTreeMap::from([("greeting".to_string(), json!("hi"))]),
                )])),
                route_binding: Some(RouteBindingPayload {
                    hosts: vec![
                        "demo.example.com".to_string(),
                        "alt.example.com".to_string(),
                    ],
                    path_prefixes: vec!["/bot".to_string(), "/api".to_string()],
                    tenant_selector: Some(TenantSelectorPayload {
                        tenant: "acme".to_string(),
                        team: "support".to_string(),
                    }),
                }),
            }],
            // No form questions for packs/extensions; default empty.
            packs: Vec::new(),
            extensions: Vec::new(),
            messaging_endpoints: vec![ManifestEndpoint {
                name: "demo-telegram".to_string(),
                provider_type: "messaging.telegram.bot".to_string(),
                links: vec!["realbot".to_string(), "auditbot".to_string()],
                welcome_flow: Some(ManifestWelcomeFlow {
                    bundle_id: "realbot".to_string(),
                    pack_id: "pack-a".to_string(),
                    flow_id: "welcome".to_string(),
                }),
                secret_refs: vec![
                    "secret://local/realbot/telegram/token".to_string(),
                    "secret://local/realbot/telegram/webhook".to_string(),
                ],
            }],
        }
    }

    fn round_trip(manifest: &EnvManifest) -> EnvManifest {
        let answers = manifest_to_answers(manifest).expect("manifest converts to answers");
        answers_to_manifest(&answers).expect("answers convert back to a manifest")
    }

    #[test]
    fn full_manifest_round_trips_through_the_deployer_converter() {
        let original = full_manifest();
        let back = round_trip(&original);
        assert_eq!(
            serde_json::to_value(&original).unwrap(),
            serde_json::to_value(&back).unwrap(),
        );
    }

    #[test]
    fn minimal_manifest_round_trips() {
        let original = EnvManifest {
            schema: ENV_MANIFEST_SCHEMA_V1.to_string(),
            environment: ManifestEnvironment {
                id: "local".to_string(),
                public_base_url: None,
                name: None,
                region: None,
                tenant_org_id: None,
                listen_addr: None,
            },
            trust_root: None,
            secrets: Vec::new(),
            packs: Vec::new(),
            bundles: Vec::new(),
            extensions: Vec::new(),
            messaging_endpoints: Vec::new(),
        };
        let back = round_trip(&original);
        assert_eq!(
            serde_json::to_value(&original).unwrap(),
            serde_json::to_value(&back).unwrap(),
        );
    }

    #[test]
    fn empty_config_overrides_stays_an_explicit_clear() {
        // `Some({})` is `op deploy`'s "explicit clear" — distinct from
        // absent ("leave untouched"). The round-trip must not collapse it.
        let mut manifest = full_manifest();
        manifest.bundles[0].config_overrides = Some(BTreeMap::new());
        let back = round_trip(&manifest);
        assert_eq!(back.bundles[0].config_overrides, Some(BTreeMap::new()));
    }

    #[test]
    fn generated_answers_pass_the_form_validation() {
        let answers = manifest_to_answers(&full_manifest()).unwrap();
        let result = qa_spec::validate(&manifest_form_spec(), &answers.answers);
        assert!(
            result.valid,
            "errors: {:?}, missing: {:?}, unknown: {:?}",
            result.errors, result.missing_required, result.unknown_fields
        );
    }

    #[test]
    fn load_initial_answers_seeds_env_for_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let map = load_initial_answers(&dir.path().join("demo.env.json"), "demo").unwrap();
        assert_eq!(map.get("environment_id"), Some(&json!("demo")));
        assert_eq!(map.len(), 1, "only the env id is pre-seeded: {map:?}");
    }

    #[test]
    fn load_initial_answers_refuses_files_it_does_not_own() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        std::fs::write(&path, r#"{"some": "other document"}"#).unwrap();
        let err = load_initial_answers(&path, "demo").unwrap_err();
        assert!(
            format!("{err:#}").contains("refusing to overwrite"),
            "got: {err:#}"
        );
    }

    #[test]
    fn load_initial_answers_rejects_env_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.env.json");
        let manifest = serde_json::to_string(&full_manifest()).unwrap();
        std::fs::write(&path, manifest).unwrap();
        let err = load_initial_answers(&path, "local").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("`demo`"), "names the manifest env: {msg}");
        assert!(msg.contains("`local`"), "names the --env value: {msg}");
    }

    #[test]
    fn load_initial_answers_preloads_a_matching_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.env.json");
        std::fs::write(&path, serde_json::to_string(&full_manifest()).unwrap()).unwrap();
        let map = load_initial_answers(&path, "demo").unwrap();
        assert_eq!(map.get("environment_id"), Some(&json!("demo")));
        assert!(map.get("secrets").is_some_and(Value::is_array));
        assert!(map.get("bundles").is_some_and(Value::is_array));
    }

    #[test]
    fn wizard_is_interactive_only() {
        let err = run_env_wizard("demo", false, false, true).unwrap_err();
        assert!(
            format!("{err:#}").contains("--answers"),
            "points at the headless alternative: {err:#}"
        );
    }

    #[test]
    fn spec_without_question_drops_only_the_named_question() {
        let spec = manifest_form_spec();
        let reduced = spec_without_question(&spec, "secrets");
        assert!(
            spec.questions.iter().any(|q| q.id == "secrets"),
            "fixture has the secrets question"
        );
        assert!(
            reduced.questions.iter().all(|q| q.id != "secrets"),
            "secrets question is dropped"
        );
        assert_eq!(
            reduced.questions.len(),
            spec.questions.len() - 1,
            "exactly one question removed"
        );
    }

    #[test]
    fn existing_from_env_by_path_indexes_preloaded_secrets() {
        let answers = json!({
            "secrets": [
                {"path": "legal/_/messaging-telegram/telegram_bot_token", "from_env": "LEGAL_TOK"},
                {"path": "acct/_/messaging-telegram/telegram_bot_token", "from_env": "ACCT_TOK"}
            ]
        });
        let map = existing_from_env_by_path(answers.as_object().unwrap());
        assert_eq!(
            map.get("legal/_/messaging-telegram/telegram_bot_token")
                .map(String::as_str),
            Some("LEGAL_TOK")
        );
        assert_eq!(map.len(), 2);
        // No secrets key → empty map.
        assert!(existing_from_env_by_path(&JsonMap::new()).is_empty());
    }

    #[test]
    fn default_env_var_name_prefixes_non_default_tenant() {
        assert_eq!(
            default_env_var_name("legal", "telegram_bot_token"),
            "LEGAL_TELEGRAM_BOT_TOKEN"
        );
        assert_eq!(
            default_env_var_name("default", "telegram_bot_token"),
            "TELEGRAM_BOT_TOKEN"
        );
        assert_eq!(default_env_var_name("", "api_key"), "API_KEY");
        // Special characters in tenant/key are sanitized to underscores so
        // the suggested default is a valid POSIX env-var name.
        assert_eq!(
            default_env_var_name("my-tenant", "bot.token"),
            "MY_TENANT_BOT_TOKEN"
        );
    }

    fn bundle_from(value: Value) -> ManifestBundle {
        serde_json::from_value(value).expect("valid manifest bundle")
    }

    /// Lay down a built-bundle workspace next to a `.gtbundle` artifact with a
    /// marked provider pack declaring one secret. Returns the manifest dir.
    fn built_bundle_with_telegram_secret(root: &Path, workspace: &str) {
        let pack_dir = root.join(workspace).join("packs/messaging-telegram");
        std::fs::create_dir_all(pack_dir.join("assets")).unwrap();
        std::fs::write(pack_dir.join("pack.yaml"), "id: messaging-telegram\n").unwrap();
        std::fs::write(
            pack_dir.join("assets/secret-requirements.json"),
            r#"[{"key":"TELEGRAM_BOT_TOKEN","required":true}]"#,
        )
        .unwrap();
        std::fs::write(
            root.join(workspace).join("realbot.gtbundle"),
            b"squashfs-placeholder",
        )
        .unwrap();
    }

    #[test]
    fn derive_required_secrets_reads_built_bundle_packs() {
        let dir = tempfile::tempdir().unwrap();
        built_bundle_with_telegram_secret(dir.path(), "ws-legal");
        let bundle = bundle_from(json!({
            "bundle_id": "realbot-legal",
            "bundle_path": "ws-legal/realbot.gtbundle",
            "route_binding": {
                "hosts": [],
                "path_prefixes": ["/legal"],
                "tenant_selector": {"tenant": "legal", "team": "default"}
            }
        }));

        let (derived, skipped) =
            derive_required_secrets(dir.path(), "local", std::slice::from_ref(&bundle));
        assert!(!skipped);
        assert_eq!(derived.len(), 1);
        assert_eq!(
            derived[0].path,
            "legal/_/messaging-telegram/telegram_bot_token"
        );
        assert_eq!(derived[0].tenant, "legal");
        assert_eq!(derived[0].bundle_ids, vec!["realbot-legal".to_string()]);
    }

    #[test]
    fn derive_required_secrets_dedups_same_path_across_bundles() {
        // Two bundles, same tenant + same provider pack → one secret path,
        // both bundle ids recorded.
        let dir = tempfile::tempdir().unwrap();
        built_bundle_with_telegram_secret(dir.path(), "ws-a");
        built_bundle_with_telegram_secret(dir.path(), "ws-b");
        let bundles = [
            bundle_from(json!({
                "bundle_id": "a", "bundle_path": "ws-a/realbot.gtbundle",
                "route_binding": {"hosts": [], "path_prefixes": ["/a"],
                    "tenant_selector": {"tenant": "shared", "team": "default"}}
            })),
            bundle_from(json!({
                "bundle_id": "b", "bundle_path": "ws-b/realbot.gtbundle",
                "route_binding": {"hosts": [], "path_prefixes": ["/b"],
                    "tenant_selector": {"tenant": "shared", "team": "default"}}
            })),
        ];
        let (derived, _) = derive_required_secrets(dir.path(), "local", &bundles);
        assert_eq!(derived.len(), 1, "deduped by path");
        assert_eq!(
            derived[0].bundle_ids,
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn derive_required_secrets_skips_unbuilt_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle_from(json!({
            "bundle_id": "missing",
            "bundle_path": "ws-missing/realbot.gtbundle"
        }));
        let (derived, skipped) =
            derive_required_secrets(dir.path(), "local", std::slice::from_ref(&bundle));
        assert!(derived.is_empty());
        assert!(skipped, "missing artifact flags skipped");
    }

    /// Sorted row-field ids of the `List` question `id` in `spec`.
    fn list_field_ids(spec: &FormSpec, id: &str) -> Vec<String> {
        let mut ids: Vec<String> = spec
            .questions
            .iter()
            .find(|q| q.id == id)
            .and_then(|q| q.list.as_ref())
            .map(|list| list.fields.iter().map(|f| f.id.clone()).collect())
            .unwrap_or_default();
        ids.sort();
        ids
    }

    #[test]
    fn spec_for_mode_basic_hides_only_the_curated_columns() {
        let basic = spec_for_mode(&manifest_form_spec(), false);
        assert_eq!(
            list_field_ids(&basic, "bundles"),
            [
                "bundle_id",
                "bundle_path",
                "route_path_prefixes",
                "route_team",
                "route_tenant",
            ],
            "basic bundles keep id/path + route path/tenant/team only"
        );
        assert_eq!(
            list_field_ids(&basic, "messaging_endpoints"),
            ["links", "name", "provider_type"],
            "basic endpoints keep name/provider_type/links only"
        );
    }

    #[test]
    fn spec_for_mode_advanced_is_a_noop() {
        let spec = manifest_form_spec();
        let advanced = spec_for_mode(&spec, true);
        // Every curated column survives, and the column sets are identical to
        // the source spec's.
        assert_eq!(
            list_field_ids(&advanced, "bundles"),
            list_field_ids(&spec, "bundles"),
        );
        assert_eq!(
            list_field_ids(&advanced, "messaging_endpoints"),
            list_field_ids(&spec, "messaging_endpoints"),
        );
        for col in ["customer_id", "config_overrides", "route_hosts"] {
            assert!(
                list_field_ids(&advanced, "bundles").contains(&col.to_string()),
                "advanced keeps bundles.{col}"
            );
        }
        for col in [
            "welcome_bundle_id",
            "welcome_pack_id",
            "welcome_flow_id",
            "secret_refs",
        ] {
            assert!(
                list_field_ids(&advanced, "messaging_endpoints").contains(&col.to_string()),
                "advanced keeps messaging_endpoints.{col}"
            );
        }
    }

    #[test]
    fn basic_spec_answers_convert_to_a_valid_manifest() {
        // A two-dept-style answer set using only basic-flow columns passes the
        // basic form spec and converts + shape-validates through the deployer —
        // proving the hidden columns are never required.
        let basic = spec_for_mode(&manifest_form_spec(), false);
        let raw = json!({
            "environment_id": "local",
            "trust_root_bootstrap": true,
            "bundles": [{
                "bundle_id": "legal",
                "bundle_path": "ws-legal/realbot.gtbundle",
                "route_path_prefixes": "/legal",
                "route_tenant": "legal",
                "route_team": "default"
            }],
            "messaging_endpoints": [{
                "name": "legal",
                "provider_type": "messaging.telegram.bot",
                "links": "legal"
            }]
        });
        let set = answer_set(raw.as_object().unwrap().clone());

        let report = qa_spec::validate(&basic, &set.answers);
        assert!(
            report.valid,
            "basic answers must pass the basic spec: {report:?}"
        );

        let manifest = answers_to_manifest(&set).expect("converts");
        manifest.validate_shape().expect("valid shape");
        let bundle = &manifest.bundles[0];
        assert!(bundle.customer_id.is_none());
        assert!(bundle.config_overrides.is_none());
        let rb = bundle.route_binding.as_ref().expect("route binding built");
        assert_eq!(rb.path_prefixes, ["/legal"]);
        assert!(rb.hosts.is_empty(), "route_hosts stays empty in basic mode");
        assert!(manifest.messaging_endpoints[0].welcome_flow.is_none());
        assert!(manifest.messaging_endpoints[0].secret_refs.is_empty());
    }
}
