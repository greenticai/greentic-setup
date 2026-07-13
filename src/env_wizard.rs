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
//! scoped to the bundle's route tenant) and, for each required secret, asks
//! whether the value comes from a named environment variable (the operator
//! enters the NAME; apply reads it from the env / dev-store) or is pasted in
//! now (collected with a masked prompt and handed to apply, which writes it
//! to the env's secrets store). The path is always auto-derived, and the
//! pasted value is never written to the manifest. So the operator only
//! enters the secrets the configured bundles actually need.
//!
//! Basic vs advanced: by default the wizard asks only the everyday fields.
//! The optional row columns an operator almost always leaves empty
//! (`customer_id`, `config_overrides`, `route_hosts` on bundles; the
//! `welcome_*` trio and `secret_refs` on endpoints) are hidden until
//! `--advanced` is passed — the same flag that already reveals the optional
//! top-level questions. The route path/tenant/team and endpoint links stay
//! in the basic flow; the common multi-bundle setup needs them.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use greentic_deployer::cli::env_manifest::{
    ENV_MANIFEST_FORM_ID, ENV_MANIFEST_FORM_VERSION, EnvManifest, ManifestBundle,
    TrustRootDirective, answers_to_manifest, manifest_form_spec_for_env,
};
use greentic_deployer::runtime_secrets::{
    SecretValue, bundle_secret_requirements, manifest_secret_path,
};
use qa_spec::{AnswerSet, FormSpec, QuestionSpec};
use rpassword::prompt_password;
use serde_json::{Map as JsonMap, Value, json};

use crate::cli_i18n::CliI18n;
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
    i18n: &CliI18n,
) -> Result<()> {
    if non_interactive || !std::io::stdin().is_terminal() {
        bail!(
            "the environment wizard is interactive; in headless runs pass an env manifest via \
             --answers <file> (generate a skeleton with `gtc op env apply --emit-answers-template`)"
        );
    }
    let manifest_path = prompt_manifest_path(env, i18n)?;
    let initial = load_initial_answers(&manifest_path, env)?;

    // Drive the shared form WITHOUT the secrets section: this terminal
    // wizard owns secrets as a final, derived step — it asks only for the
    // secrets the configured bundles actually declare, and only for the
    // env-var NAME of each (never the path, never the value).
    //
    // Pass the target env so the `webchat_gui` question defaults match the
    // runtime resolver (`resolved_gui_enabled()`): on for the `local` env id,
    // off elsewhere. An explicit answer in a pre-loaded manifest still wins.
    let spec = manifest_form_spec_for_env(env);
    // Basic flow hides the advanced-only row columns; `--advanced` reveals
    // them (mirrors the existing top-level optional-question gating).
    let spec = spec_for_mode(&spec, advanced);
    let form_spec = spec_without_question(&spec, "secrets");
    // Localize the question prompts (titles/descriptions/list nouns) for the
    // requested locale before handing them to the prompt loop; choice VALUES
    // stay canonical (see `localize_spec`). The deployer's English literals are
    // the fallback, so an untranslated locale still renders.
    let form_spec = localize_spec(form_spec, i18n);
    if !advanced {
        println!(
            "\n{}",
            i18n.t_or(
                "env_wizard.basic_mode",
                "Basic mode — pass --advanced to also set customer id, config \
                 overrides, route hosts, welcome flow, and endpoint secret refs.",
            )
        );
    }
    let prompted = prompt_form_spec_answers_with_existing(
        &form_spec,
        "environment",
        advanced,
        &Value::Object(initial),
        Some(i18n),
    )?;
    let mut answers = prompted.as_object().cloned().unwrap_or_default();

    // Pre-loaded from_env values (editing an existing manifest) become the
    // per-secret defaults so a re-run doesn't re-ask what hasn't changed.
    let existing_from_env = existing_from_env_by_path(&answers);
    // Pre-loaded source choices default the env-vs-paste prompt on a re-edit.
    let existing_source = existing_source_by_path(&answers);

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
    let (secret_rows, prefilled_secrets) = derive_and_prompt_secrets(
        &manifest_dir,
        env,
        &provisional.bundles,
        &existing_from_env,
        &existing_source,
        i18n,
    )?;
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
        "\n{}",
        i18n.tf_or(
            "env_wizard.wrote_manifest",
            "Wrote `{}` — the manifest is the durable artifact; keep it in version control.",
            &[&manifest_path.display().to_string()],
        )
    );

    // A pasted value lives only in memory until a confirmed apply writes it to
    // the store. A dry run previews without executing, so it would silently
    // discard what was just entered — and, because a paste secret already in
    // the store is treated as satisfied on a later apply, an existing stale
    // value would then win. Warn rather than drop it silently.
    if dry_run && !prefilled_secrets.is_empty() {
        println!(
            "\n{}",
            i18n.tf_or(
                "env_wizard.dry_run_secrets_note",
                "Note: --dry-run previews only — the {} pasted secret value(s) you entered are \
                 NOT written to the store. Re-run without --dry-run and confirm the plan to \
                 persist them.",
                &[&prefilled_secrets.len().to_string()],
            )
        );
    }

    env_mode::run_env_apply(&manifest_path, &doc, env, dry_run, false, prefilled_secrets)
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

/// Return `spec` with the user-facing prompt text localized for
/// `i18n`'s locale: the form title/description, every question's
/// title/description (recursing into `List` row columns), and each `List`
/// item label. Keys are derived from the stable question ids
/// (`env_wizard.q.<id>.title`, `.desc`, `env_wizard.list.<id>.item_label`,
/// `env_wizard.form.title`/`.desc`). A missing catalog key falls back to the
/// English literal already on the spec — the deployer is the canonical English
/// source, so an untranslated locale still renders.
///
/// Choice VALUES (`enum` options) are deliberately left untouched: they are
/// canonical answer tokens written into the manifest, not display text.
fn localize_spec(mut spec: FormSpec, i18n: &CliI18n) -> FormSpec {
    spec.title = i18n.t_or("env_wizard.form.title", &spec.title);
    if let Some(desc) = spec.description.take() {
        spec.description = Some(i18n.t_or("env_wizard.form.desc", &desc));
    }
    for question in &mut spec.questions {
        localize_question(question, i18n);
    }
    spec
}

/// Localize a single question in place (see [`localize_spec`]); recurses into
/// `List` row columns.
fn localize_question(question: &mut QuestionSpec, i18n: &CliI18n) {
    question.title = i18n.t_or(
        &format!("env_wizard.q.{}.title", question.id),
        &question.title,
    );
    if let Some(desc) = question.description.take() {
        question.description =
            Some(i18n.t_or(&format!("env_wizard.q.{}.desc", question.id), &desc));
    }
    if let Some(list) = question.list.as_mut() {
        if let Some(label) = list.item_label.take() {
            list.item_label = Some(i18n.t_or(
                &format!("env_wizard.list.{}.item_label", question.id),
                &label,
            ));
        }
        for field in &mut list.fields {
            localize_question(field, i18n);
        }
    }
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

/// `path -> source` (`env` | `paste`) from a pre-loaded `secrets` answer
/// array, used to default the env-vs-paste choice on a re-edit. A row with no
/// explicit `source` is inferred from `from_env` presence (back-compat with
/// manifests authored before the source discriminator existed).
fn existing_source_by_path(answers: &JsonMap<String, Value>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(Value::Array(rows)) = answers.get("secrets") {
        for row in rows {
            let Some(path) = row.get("path").and_then(Value::as_str) else {
                continue;
            };
            let source = row.get("source").and_then(Value::as_str).unwrap_or(
                if row.get("from_env").is_some() {
                    "env"
                } else {
                    "paste"
                },
            );
            map.insert(path.to_string(), source.to_string());
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
        // A bundle declares either a single `bundle_path` or a multi-revision
        // `revisions[]` list (mutually exclusive). For secret auto-detection,
        // resolve the single artifact, or the first revision as a representative
        // (revisions of one deployment share the same tenant/secrets).
        let Some(raw) = bundle.bundle_path.as_ref().or_else(|| {
            bundle
                .revisions
                .as_ref()
                .and_then(|revs| revs.first())
                .map(|rev| &rev.bundle_path)
        }) else {
            skipped = true;
            continue;
        };
        let artifact = if raw.is_absolute() {
            raw.clone()
        } else {
            manifest_dir.join(raw)
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

/// Derive the secrets the configured bundles need and, for each, prompt for a
/// source: an environment variable NAME or a pasted value. Returns the
/// `secrets[]` answer rows (`{path, source, from_env?}`) to merge into the
/// manifest answers, plus the pasted values keyed by path to hand to the apply
/// engine (`ApplyOptions.prefilled_secrets`) so it does not re-prompt.
///
/// When a bundle was skipped (unbuilt/unreadable) any pre-loaded secrets not
/// re-derived are preserved as-is, so a partial edit never silently drops a
/// secret the wizard couldn't recompute.
fn derive_and_prompt_secrets(
    manifest_dir: &Path,
    env: &str,
    bundles: &[ManifestBundle],
    existing_from_env: &BTreeMap<String, String>,
    existing_source: &BTreeMap<String, String>,
    i18n: &CliI18n,
) -> Result<(Vec<Value>, BTreeMap<String, SecretValue>)> {
    let (derived, skipped) = derive_required_secrets(manifest_dir, env, bundles);

    // When a bundle was skipped, pre-loaded secrets we couldn't recompute are
    // preserved rather than dropped (see the tail of this fn).
    let preserving = skipped && !existing_source.is_empty();
    if derived.is_empty() && !preserving {
        println!(
            "\n{}",
            i18n.t_or(
                "env_wizard.secrets.none",
                "Secrets — the configured bundles declare no secrets; nothing to enter.",
            )
        );
        return Ok((Vec::new(), BTreeMap::new()));
    }

    if !derived.is_empty() {
        println!(
            "\n{}",
            i18n.tf_or(
                "env_wizard.secrets.need",
                "Secrets — the configured bundles need {} secret(s).",
                &[&derived.len().to_string()],
            )
        );
        println!(
            "{}",
            i18n.t_or(
                "env_wizard.secrets.choose",
                "For each, choose where the value comes from: a named environment\n\
                 variable, or paste it in now. Pasted values are stored in the\n\
                 environment's secrets store — never written to the manifest.",
            )
        );
    }

    let mut rows = Vec::with_capacity(derived.len());
    let mut prefilled = BTreeMap::new();
    let mut taken = BTreeSet::new();
    for secret in &derived {
        println!();
        let optional_suffix = if secret.required {
            String::new()
        } else {
            i18n.t_or("env_wizard.secrets.optional_suffix", " [optional]")
        };
        println!(
            "  {}",
            i18n.tf_or(
                "env_wizard.secrets.entry",
                "{} — {} (bundle: {}){}",
                &[
                    &secret.key,
                    &secret.provider_id,
                    &secret.bundle_ids.join(", "),
                    &optional_suffix,
                ],
            )
        );
        println!(
            "  {}",
            i18n.tf_or(
                "env_wizard.secrets.path",
                "secret path: {}",
                &[&secret.path]
            )
        );

        // Re-edit defaults to the previously-chosen source.
        let was_paste = existing_source.get(&secret.path).map(String::as_str) == Some("paste");
        match prompt_secret_source(was_paste, i18n)? {
            SecretSource::Env => {
                let default = existing_from_env
                    .get(&secret.path)
                    .cloned()
                    .unwrap_or_else(|| default_env_var_name(&secret.tenant, &secret.key));
                let from_env = prompt_env_var_name(&default, i18n)?;
                rows.push(
                    json!({ "path": secret.path.clone(), "source": "env", "from_env": from_env }),
                );
            }
            SecretSource::Paste => {
                // On a re-edit of an already-paste secret, the value is already
                // in the store: empty input keeps it (apply no-ops). Otherwise a
                // value is required now.
                if let Some(value) = prompt_paste_value(was_paste, i18n)? {
                    prefilled.insert(secret.path.clone(), SecretValue::from(value));
                }
                rows.push(json!({ "path": secret.path.clone(), "source": "paste" }));
            }
        }
        taken.insert(secret.path.clone());
    }

    // Preserve pre-loaded secrets the wizard couldn't recompute (a bundle was
    // skipped), so editing a manifest without rebuilt bundles is non-destructive.
    // Env secrets keep their `from_env`; paste secrets keep their store value
    // (apply no-ops on the already-stored value).
    if skipped {
        for (path, source) in existing_source {
            if taken.contains(path.as_str()) {
                continue;
            }
            match source.as_str() {
                "paste" => {
                    eprintln!(
                        "  {}",
                        i18n.tf_or(
                            "env_wizard.secrets.keep_paste_note",
                            "note: keeping existing pasted secret `{}` (bundle not rebuilt)",
                            &[path],
                        )
                    );
                    rows.push(json!({ "path": path, "source": "paste" }));
                }
                _ => {
                    let Some(from_env) = existing_from_env.get(path) else {
                        continue;
                    };
                    eprintln!(
                        "  {}",
                        i18n.tf_or(
                            "env_wizard.secrets.keep_env_note",
                            "note: keeping existing secret `{}` (bundle not rebuilt)",
                            &[path],
                        )
                    );
                    rows.push(json!({ "path": path, "source": "env", "from_env": from_env }));
                }
            }
        }
    }

    Ok((rows, prefilled))
}

/// Where a secret's value comes from, as chosen in the wizard.
enum SecretSource {
    /// A named environment variable, read at apply time.
    Env,
    /// A value pasted in now and stored in the env's secrets store.
    Paste,
}

/// Prompt whether a secret's value comes from an environment variable or is
/// pasted in now. `default_paste` seeds the default (a re-edit keeps the
/// previous choice).
fn prompt_secret_source(default_paste: bool, i18n: &CliI18n) -> Result<SecretSource> {
    let default = if default_paste { "2" } else { "1" };
    loop {
        // The `[1]`/`[2]` reply tokens stay canonical — only the prose around
        // them is localized.
        print!(
            "  > {}",
            i18n.tf_or(
                "env_wizard.secrets.source_prompt",
                "value from [1] environment variable or [2] paste it now? [{}]: ",
                &[default],
            )
        );
        std::io::stdout().flush()?;
        let mut line = String::new();
        let n = std::io::stdin().read_line(&mut line)?;
        if n == 0 {
            bail!("unexpected end of input while choosing a secret source");
        }
        let trimmed = line.trim();
        let choice = if trimmed.is_empty() { default } else { trimmed };
        match choice.to_ascii_lowercase().as_str() {
            "1" | "env" | "e" => return Ok(SecretSource::Env),
            "2" | "paste" | "p" => return Ok(SecretSource::Paste),
            _ => println!(
                "  {}",
                i18n.t_or(
                    "env_wizard.secrets.source_invalid",
                    "Enter 1 (environment variable) or 2 (paste).",
                )
            ),
        }
    }
}

/// Masked prompt for a pasted secret value. When `keep_stored` is true (a
/// re-edit of an already-stored paste secret), empty input keeps the stored
/// value (`Ok(None)` — apply leaves the store entry untouched). Otherwise a
/// non-empty value is required.
///
/// Single-line only: `rpassword` reads to the first newline, so a multiline
/// secret (a PEM key, certificate, multiline JSON) would be truncated to its
/// first line. Such secrets should use the environment-variable source
/// instead — the prompt says "single line" to steer the operator there.
fn prompt_paste_value(keep_stored: bool, i18n: &CliI18n) -> Result<Option<String>> {
    loop {
        let prompt = if keep_stored {
            format!(
                "  > {}",
                i18n.t_or(
                    "env_wizard.secrets.paste_prompt_keep",
                    "paste value (hidden, single line; empty keeps the stored value): ",
                )
            )
        } else {
            format!(
                "  > {}",
                i18n.t_or(
                    "env_wizard.secrets.paste_prompt",
                    "paste value (hidden, single line): ",
                )
            )
        };
        let value = prompt_password(&prompt)?;
        if value.is_empty() {
            if keep_stored {
                return Ok(None);
            }
            println!(
                "  {}",
                i18n.t_or("env_wizard.secrets.paste_required", "A value is required.")
            );
            continue;
        }
        return Ok(Some(value));
    }
}

/// Prompt for one env-var name, defaulting to `default` on empty input.
/// Re-prompts only if both the input and the default are blank.
fn prompt_env_var_name(default: &str, i18n: &CliI18n) -> Result<String> {
    loop {
        print!(
            "  > {}",
            i18n.tf_or(
                "env_wizard.secrets.envvar_prompt",
                "env var name [{}]: ",
                &[default]
            )
        );
        std::io::stdout().flush()?;
        let mut line = String::new();
        let n = std::io::stdin().read_line(&mut line)?;
        if n == 0 {
            bail!("unexpected end of input while prompting for env var name");
        }
        let trimmed = line.trim();
        let value = if trimmed.is_empty() { default } else { trimmed };
        if value.is_empty() {
            println!(
                "  {}",
                i18n.t_or(
                    "env_wizard.secrets.envvar_required",
                    "An environment variable name is required.",
                )
            );
            continue;
        }
        return Ok(value.to_string());
    }
}

/// Ask where the manifest lives (and will be written). Empty input takes
/// the conventional default `./<env>.env.json`.
fn prompt_manifest_path(env: &str, i18n: &CliI18n) -> Result<PathBuf> {
    let default = format!("./{env}.env.json");
    print!(
        "{}",
        i18n.tf_or(
            "env_wizard.manifest_prompt",
            "Manifest file [{}]: ",
            &[&default]
        )
    );
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
    // Emit `webchat_gui` only when the manifest carries an explicit choice —
    // an unset (`None`) value is left absent so it round-trips back to `None`
    // (the env-id default resolves at runtime) and the wizard re-asks the
    // question with its default-true. Mirrors `public_base_url` above.
    if let Some(gui_enabled) = manifest.environment.gui_enabled {
        map.insert("webchat_gui".to_string(), Value::Bool(gui_enabled));
    }
    if !manifest.secrets.is_empty() {
        let rows = manifest
            .secrets
            .iter()
            .map(|s| match &s.from_env {
                Some(from_env) => json!({"path": s.path, "source": "env", "from_env": from_env}),
                None => json!({"path": s.path, "source": "paste"}),
            })
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
                // Single-revision bundles carry `bundle_path`; multi-revision
                // (JSON-first) bundles use the first revision's artifact as a
                // representative for the wizard answer-set (which models one path).
                if let Some(bp) = b.bundle_path.as_ref().or_else(|| {
                    b.revisions
                        .as_ref()
                        .and_then(|revs| revs.first())
                        .map(|rev| &rev.bundle_path)
                }) {
                    row.insert(
                        "bundle_path".to_string(),
                        Value::String(bp.display().to_string()),
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
        ManifestSecret, ManifestWelcomeFlow, manifest_form_spec,
    };
    use std::collections::BTreeMap;

    fn full_manifest() -> EnvManifest {
        EnvManifest {
            schema: ENV_MANIFEST_SCHEMA_V1.to_string(),
            environment: ManifestEnvironment {
                id: "demo".to_string(),
                public_base_url: Some("https://demo.example.com".to_string()),
                // Form-backed (the `webchat_gui` question), so an explicit value
                // survives the round-trip — unlike the form-less fields below.
                gui_enabled: Some(true),
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
                from_env: Some("DEMO_BOT_TOKEN".to_string()),
            }],
            bundles: vec![ManifestBundle {
                bundle_id: "realbot".to_string(),
                bundle_path: Some(PathBuf::from("./bundles/realbot.gtbundle")),
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
                bundle_source_uri: None,
                bundle_digest: None,
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
            cluster: None,
            updates: None,
            vault_bootstrap: None,
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
    fn gui_enabled_maps_to_webchat_gui_answer() {
        // An explicit value surfaces as the boolean answer, so edit mode
        // pre-fills the question instead of re-asking it.
        let answers = manifest_to_answers(&full_manifest()).unwrap();
        assert_eq!(answers.answers["webchat_gui"], json!(true));

        // An explicit `false` opt-out is preserved as the boolean answer and
        // survives the round-trip — it is never silently dropped or flipped.
        let mut manifest = full_manifest();
        manifest.environment.gui_enabled = Some(false);
        let answers = manifest_to_answers(&manifest).unwrap();
        assert_eq!(answers.answers["webchat_gui"], json!(false));
        assert_eq!(round_trip(&manifest).environment.gui_enabled, Some(false));

        // An unset value omits the answer entirely: the round-trip preserves
        // `None` (env-id default resolves at runtime) and the wizard re-asks
        // with its default-true.
        let mut manifest = full_manifest();
        manifest.environment.gui_enabled = None;
        let answers = manifest_to_answers(&manifest).unwrap();
        assert!(answers.answers.get("webchat_gui").is_none());
        assert_eq!(round_trip(&manifest).environment.gui_enabled, None);
    }

    #[test]
    fn paste_and_env_secrets_round_trip_through_the_converter() {
        // Mixed sources: an env-sourced secret keeps `from_env`; a
        // paste-sourced one carries `source: paste` and no `from_env` (no
        // value in the manifest). Both survive manifest → answers → manifest.
        let mut manifest = full_manifest();
        manifest.secrets = vec![
            ManifestSecret {
                path: "default/_/messaging-telegram/telegram_bot_token".to_string(),
                from_env: Some("DEMO_BOT_TOKEN".to_string()),
            },
            ManifestSecret {
                path: "default/_/messaging-slack/slack_bot_token".to_string(),
                from_env: None,
            },
        ];
        let back = round_trip(&manifest);
        assert_eq!(back.secrets[0].from_env.as_deref(), Some("DEMO_BOT_TOKEN"));
        assert_eq!(back.secrets[1].from_env, None);

        // The wizard answers carry the source discriminator; the paste row
        // never carries a value.
        let answers = manifest_to_answers(&manifest).unwrap();
        let rows = answers.answers["secrets"].as_array().unwrap();
        assert_eq!(rows[0]["source"], "env");
        assert_eq!(rows[0]["from_env"], "DEMO_BOT_TOKEN");
        assert_eq!(rows[1]["source"], "paste");
        assert!(rows[1].get("from_env").is_none());
    }

    #[test]
    fn existing_source_by_path_reads_and_infers() {
        let answers = json!({
            "secrets": [
                {"path": "a/_/p/tok", "source": "paste"},
                {"path": "b/_/p/tok", "source": "env", "from_env": "B"},
                // Legacy row (no `source`): inferred from `from_env` presence.
                {"path": "c/_/p/tok", "from_env": "C"},
                {"path": "d/_/p/tok"}
            ]
        });
        let map = existing_source_by_path(answers.as_object().unwrap());
        assert_eq!(map.get("a/_/p/tok").map(String::as_str), Some("paste"));
        assert_eq!(map.get("b/_/p/tok").map(String::as_str), Some("env"));
        assert_eq!(map.get("c/_/p/tok").map(String::as_str), Some("env"));
        assert_eq!(map.get("d/_/p/tok").map(String::as_str), Some("paste"));
    }

    #[test]
    fn minimal_manifest_round_trips() {
        let original = EnvManifest {
            schema: ENV_MANIFEST_SCHEMA_V1.to_string(),
            environment: ManifestEnvironment {
                id: "local".to_string(),
                public_base_url: None,
                gui_enabled: None,
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
            cluster: None,
            updates: None,
            vault_bootstrap: None,
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
        let i18n = CliI18n::from_request(None).unwrap();
        let err = run_env_wizard("demo", false, false, true, &i18n).unwrap_err();
        assert!(
            format!("{err:#}").contains("--answers"),
            "points at the headless alternative: {err:#}"
        );
    }

    #[test]
    fn localize_spec_translates_questions_and_list_labels_to_dutch() {
        let i18n = CliI18n::from_request(Some("nl")).unwrap();
        let spec = localize_spec(manifest_form_spec_for_env("local"), &i18n);

        // Form title + top-level question title are Dutch.
        assert_eq!(spec.title, "Omgeving instellen");
        let bundles = spec.questions.iter().find(|q| q.id == "bundles").unwrap();
        assert_eq!(bundles.title, "Bundels");

        let list = bundles.list.as_ref().unwrap();
        // The list "Add <noun>?" label is localized…
        assert_eq!(list.item_label.as_deref(), Some("bundel"));
        // …and nested row-column titles recurse.
        let bundle_id = list.fields.iter().find(|c| c.id == "bundle_id").unwrap();
        assert_eq!(bundle_id.title, "Bundel-id");

        // Enum choice VALUES stay canonical (answer tokens, not display text).
        let secrets = spec.questions.iter().find(|q| q.id == "secrets").unwrap();
        let source = secrets
            .list
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .find(|c| c.id == "source")
            .unwrap();
        assert_eq!(
            source.choices.as_deref(),
            Some(["env".to_string(), "paste".to_string()].as_slice()),
            "choice values must not be translated"
        );
    }

    #[test]
    fn localize_spec_falls_back_to_english_for_unknown_locale() {
        // A locale with no catalog resolves to the English fallback, i.e. the
        // deployer's canonical literals — the wizard still renders.
        let i18n = CliI18n::from_request(Some("zz")).unwrap();
        let spec = localize_spec(manifest_form_spec_for_env("local"), &i18n);
        assert_eq!(spec.title, "Environment setup");
        let bundles = spec.questions.iter().find(|q| q.id == "bundles").unwrap();
        assert_eq!(bundles.title, "Bundles");
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
            "webchat_gui": true,
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
