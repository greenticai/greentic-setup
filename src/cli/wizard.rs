//! Bundle wizard CLI commands.
//!
//! Handles the `wizard apply` command for creating bundles with packs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::{Deserialize, Serialize};

use crate::bundle;
use crate::cli::pack_extract::{extract_pack_to_bundle, get_provider_id_from_pack_ref};
use crate::cli_i18n::CliI18n;
use crate::discovery;
use crate::engine::{SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::qa::wizard as qa_wizard;
use crate::setup_to_formspec;
use crate::{SetupEngine, SetupMode};

/// Wizard apply command arguments.
#[derive(Args, Debug, Clone)]
pub struct WizardApplyArgs {
    /// Path to answer document (JSON/YAML)
    #[arg(long = "answers", short = 'a', required = true)]
    pub answers: PathBuf,

    /// Dry run - show what would be done without executing
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Non-interactive mode (default for wizard apply)
    #[arg(long = "non-interactive")]
    pub non_interactive: bool,

    /// Output path for generated answers
    #[arg(long = "out")]
    pub out: Option<PathBuf>,
}

/// Answer document structure for greentic-bundle.wizard.main.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardAnswerDocument {
    pub wizard_id: String,
    #[serde(default)]
    pub schema_id: Option<String>,
    #[serde(default)]
    pub schema_version: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    pub answers: BundleWizardAnswers,
    #[serde(default)]
    pub locks: serde_json::Map<String, serde_json::Value>,
}

/// Launcher answer document structure for greentic-dev.wizard.launcher.main.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherAnswerDocument {
    pub wizard_id: String,
    #[serde(default)]
    pub schema_id: Option<String>,
    #[serde(default)]
    pub schema_version: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    pub answers: LauncherAnswers,
    #[serde(default)]
    pub locks: serde_json::Map<String, serde_json::Value>,
}

/// Launcher answers with delegate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherAnswers {
    #[serde(default)]
    pub selected_action: String,
    #[serde(default)]
    pub delegate_answer_document: Option<WizardAnswerDocument>,
}

/// Bundle wizard answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleWizardAnswers {
    #[serde(default = "default_action")]
    pub selected_action: String,
    pub bundle_path: String,
    #[serde(default)]
    pub bundle_name: Option<String>,
    #[serde(default)]
    pub pack_refs: Vec<String>,
}

fn default_action() -> String {
    "create".to_string()
}

/// Apply wizard from answer document.
pub fn apply(args: WizardApplyArgs, i18n: &CliI18n) -> Result<()> {
    // Read and parse answer document
    let answers_content = std::fs::read_to_string(&args.answers)
        .context(format!("Failed to read answer document: {}", args.answers.display()))?;

    // Parse to extract wizard_id first
    let raw: serde_json::Value = if args.answers.extension().is_some_and(|e| e == "yaml" || e == "yml") {
        serde_yaml_bw::from_str(&answers_content)
            .context("Failed to parse YAML answer document")?
    } else {
        serde_json::from_str(&answers_content)
            .context("Failed to parse JSON answer document")?
    };

    let wizard_id = raw.get("wizard_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Handle launcher format (greentic-dev.wizard.launcher.main)
    let doc: WizardAnswerDocument = if wizard_id == "greentic-dev.wizard.launcher.main" {
        let launcher: LauncherAnswerDocument = serde_json::from_value(raw)
            .context("Failed to parse launcher answer document")?;

        launcher.answers.delegate_answer_document
            .ok_or_else(|| anyhow::anyhow!("Launcher document missing delegate_answer_document"))?
    } else if wizard_id == "greentic-bundle.wizard.main" {
        serde_json::from_value(raw)
            .context("Failed to parse bundle wizard answer document")?
    } else {
        bail!(
            "Unsupported wizard_id: {} (expected greentic-bundle.wizard.main or greentic-dev.wizard.launcher.main)",
            wizard_id
        );
    };

    let bundle_path = PathBuf::from(&doc.answers.bundle_path);
    let bundle_name = doc.answers.bundle_name.as_deref().unwrap_or("Greentic Bundle");
    let action = &doc.answers.selected_action;

    println!("[greentic-setup] Bundle wizard apply");
    println!("[greentic-setup]   Action: {}", action);
    println!("[greentic-setup]   Path: {}", bundle_path.display());
    println!("[greentic-setup]   Name: {}", bundle_name);

    if args.dry_run {
        println!("[greentic-setup] [DRY-RUN] Would execute:");
        println!("  greentic-setup bundle init \"{}\" --name \"{}\"", bundle_path.display(), bundle_name);
        for pack_ref in &doc.answers.pack_refs {
            println!("  greentic-setup bundle add \"{}\" --bundle \"{}\"", pack_ref, bundle_path.display());
            println!("  [extract pack content to bundle]");
        }
        return Ok(());
    }

    match action.as_str() {
        "create" => apply_create(&bundle_path, bundle_name, &doc.answers.pack_refs, i18n)?,
        "update" => apply_update(&bundle_path, &doc.answers.pack_refs)?,
        _ => bail!("Unsupported action: {}", action),
    }

    Ok(())
}

/// Create a new bundle with packs.
fn apply_create(bundle_path: &Path, bundle_name: &str, pack_refs: &[String], i18n: &CliI18n) -> Result<()> {
    // Step 1: Initialize bundle
    println!("[greentic-setup] Initializing bundle...");

    // Remove existing bundle if present
    if bundle_path.exists() {
        println!("[greentic-setup] Removing existing bundle directory...");
        std::fs::remove_dir_all(bundle_path)
            .context("Failed to remove existing bundle")?;
    }

    bundle::create_demo_bundle_structure(bundle_path, Some(bundle_name))
        .context(i18n.t("cli.error.failed_create_bundle"))?;

    println!("[greentic-setup] \u{2713} Bundle initialized: {}", bundle_path.display());

    // Step 2: Add and extract packs
    let mut registry_items = Vec::new();
    for pack_ref in pack_refs {
        if pack_ref.is_empty() {
            continue;
        }

        println!("[greentic-setup] Adding pack: {}", pack_ref);

        // Add pack to bundle using SetupEngine
        add_pack_to_bundle(bundle_path, pack_ref)?;

        // Extract pack content to bundle
        extract_pack_to_bundle(pack_ref, bundle_path)?;

        // Collect registry item
        if let Some(provider_id) = get_provider_id_from_pack_ref(pack_ref) {
            registry_items.push(serde_json::json!({
                "id": provider_id,
                "label": {
                    "i18n_key": format!("provider.{}", provider_id.replace('-', ".")),
                    "fallback": provider_id.replace("messaging-", "").replace("events-", "")
                },
                "ref": pack_ref
            }));
        }

        println!("[greentic-setup] \u{2713} Added pack: {}", pack_ref);
    }

    // Create provider-registry.json
    if !registry_items.is_empty() {
        let registry = serde_json::json!({
            "registry_version": "providers@1",
            "items": registry_items
        });
        let registry_path = bundle_path.join("provider-registry.json");
        std::fs::write(&registry_path, serde_json::to_string_pretty(&registry)?)?;
        println!("[greentic-setup] \u{2713} Created provider-registry.json");
    }

    println!("[greentic-setup] \u{2713} Bundle created successfully!");
    Ok(())
}

/// Update an existing bundle with packs.
fn apply_update(bundle_path: &Path, pack_refs: &[String]) -> Result<()> {
    println!("[greentic-setup] Updating bundle...");

    if !bundle_path.exists() {
        bail!("Bundle not found: {}", bundle_path.display());
    }

    // Add packs to existing bundle
    for pack_ref in pack_refs {
        if pack_ref.is_empty() {
            continue;
        }

        println!("[greentic-setup] Adding pack: {}", pack_ref);

        // Add pack to bundle
        add_pack_to_bundle(bundle_path, pack_ref)?;

        // Extract pack content to bundle
        extract_pack_to_bundle(pack_ref, bundle_path)?;

        println!("[greentic-setup] \u{2713} Added pack: {}", pack_ref);
    }

    println!("[greentic-setup] \u{2713} Bundle updated successfully!");
    Ok(())
}

/// Add pack to bundle using SetupEngine (registers in manifest).
fn add_pack_to_bundle(bundle_path: &Path, pack_ref: &str) -> Result<()> {
    let config = SetupConfig {
        tenant: "demo".to_string(),
        team: None,
        env: "dev".to_string(),
        offline: false,
        verbose: false,
    };

    let engine = SetupEngine::new(config);

    let request = SetupRequest {
        bundle: bundle_path.to_path_buf(),
        pack_refs: vec![pack_ref.to_string()],
        tenants: vec![TenantSelection {
            tenant: "demo".to_string(),
            team: None,
            allow_paths: Vec::new(),
        }],
        ..Default::default()
    };

    let plan = engine
        .plan(SetupMode::Create, &request, false)
        .context("Failed to build pack add plan")?;

    engine
        .execute(&plan)
        .context("Failed to execute pack add")?;

    Ok(())
}

/// Run interactive wizard for bundle providers.
pub fn run_interactive(
    bundle_path: &Path,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    use serde_json::Value;

    let mut all_answers = serde_json::Map::new();

    // Discover packs in the bundle
    let discovered = discovery::discover(bundle_path)?;

    if discovered.providers.is_empty() {
        println!("No providers found in bundle. Nothing to configure.");
        return Ok(all_answers);
    }

    println!(
        "Found {} provider(s) to configure:",
        discovered.providers.len()
    );
    for provider in &discovered.providers {
        println!("  - {} ({})", provider.provider_id, provider.domain);
    }
    println!();

    // Run wizard for each provider
    for provider in &discovered.providers {
        let provider_id = &provider.provider_id;

        // Try to build FormSpec from setup.yaml or pack manifest
        let form_spec = setup_to_formspec::pack_to_form_spec(&provider.pack_path, provider_id);

        if let Some(spec) = form_spec {
            if spec.questions.is_empty() {
                println!("Provider {}: No configuration required.", provider_id);
                all_answers.insert(provider_id.clone(), Value::Object(serde_json::Map::new()));
                continue;
            }

            // Run interactive prompts for this provider
            let answers = qa_wizard::prompt_form_spec_answers(&spec, provider_id)?;
            all_answers.insert(provider_id.clone(), answers);
        } else {
            // No FormSpec available - provider uses flow-based setup or has no questions
            println!(
                "Provider {}: No setup questions found (may use flow-based setup).",
                provider_id
            );
            all_answers.insert(provider_id.clone(), Value::Object(serde_json::Map::new()));
        }

        println!();
    }

    Ok(all_answers)
}
