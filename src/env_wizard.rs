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

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::env_manifest::{
    ENV_MANIFEST_FORM_ID, ENV_MANIFEST_FORM_VERSION, EnvManifest, TrustRootDirective,
    answers_to_manifest, manifest_form_spec,
};
use qa_spec::AnswerSet;
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

    let spec = manifest_form_spec();
    let answers = prompt_form_spec_answers_with_existing(
        &spec,
        "environment",
        advanced,
        &Value::Object(initial),
    )?;
    let answer_set = AnswerSet {
        form_id: ENV_MANIFEST_FORM_ID.to_string(),
        spec_version: ENV_MANIFEST_FORM_VERSION.to_string(),
        answers,
        meta: None,
    };
    let manifest = answers_to_manifest(&answer_set)?;

    let mut rendered = serde_json::to_string_pretty(&manifest)?;
    rendered.push('\n');
    std::fs::write(&manifest_path, rendered)
        .with_context(|| format!("failed to write `{}`", manifest_path.display()))?;
    println!(
        "\nWrote `{}` — the manifest is the durable artifact; keep it in version control.",
        manifest_path.display()
    );

    let doc = serde_json::to_value(&manifest)?;
    env_mode::run_env_apply(&manifest_path, &doc, env, dry_run, false)
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
    Ok(answers.answers.as_object().cloned().unwrap_or_default())
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
        Value::Bool(matches!(
            manifest.trust_root,
            Some(TrustRootDirective::Bootstrap)
        )),
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
                row.insert(
                    "bundle_path".to_string(),
                    Value::String(b.bundle_path.display().to_string()),
                );
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
            },
            trust_root: Some(TrustRootDirective::Bootstrap),
            secrets: vec![ManifestSecret {
                path: "default/_/messaging-telegram/telegram_bot_token".to_string(),
                from_env: "DEMO_BOT_TOKEN".to_string(),
            }],
            bundles: vec![ManifestBundle {
                bundle_id: "realbot".to_string(),
                bundle_path: PathBuf::from("./bundles/realbot.gtbundle"),
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
            },
            trust_root: None,
            secrets: Vec::new(),
            bundles: Vec::new(),
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
}
