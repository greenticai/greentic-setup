//! Provider-declared setup state machines.
//!
//! This module is the first slice of the provider setup-machine work: pack
//! metadata loading, contract validation, stable setup-state paths, and the
//! first conservative execution slice for resumable provider setup.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::discovery;
use crate::doctor::{Diagnostic, DiagnosticSeverity, DoctorReport};

pub const SETUP_MACHINE_EXTENSION: &str = "greentic.setup.machine.v1";
pub const SETUP_MACHINE_SCHEMA_URL: &str = "https://schemas.greentic.ai/setup-machine/v1";
pub const SETUP_BACKEND_CONTRACT_EXTENSION: &str = "greentic.setup.backend-contract.v1";
pub const SETUP_WEB_COMPONENT_EXTENSION: &str = "greentic.setup.web-component.v1";
pub const SETUP_I18N_EXTENSION: &str = "greentic.setup.i18n.v1";
pub const HTTP_ROUTES_EXTENSION: &str = "greentic.http-routes.v1";
const SETUP_MACHINE_SCHEMA: &str = include_str!("../schemas/setup-machine.v1.schema.json");
const SETUP_BACKEND_CONTRACT_SCHEMA: &str =
    include_str!("../schemas/setup-backend-contract.v1.schema.json");

const TERMINAL_SUCCESS: &str = "complete";
const TERMINAL_FAILED: &str = "failed";
const SUPPORTED_SETUP_STATE_SCHEMA_VERSION: u32 = 1;

const SUPPORTED_STEP_KINDS: &[&str] = &[
    "qa_form",
    "platform_public_url",
    "cloudflare_tunnel",
    "oauth_device_code",
    "oauth_authorization_code",
    "http_probe",
    "http_json",
    "select_from_json",
    "generate_file",
    "download_file",
    "manual_action",
    "provider_component_call",
    "persist_runtime_config",
];

const SUPPORTED_BACKEND_EXECUTOR_KINDS: &[&str] = &[
    "oauth_device_code",
    "microsoft_graph_application",
    "provider_http",
    "microsoft_graph_teams_app_catalog_publish",
    "microsoft_graph_teams_app_user_install",
    "runtime_observation",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupMachine {
    pub version: u32,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub entry_step: String,
    #[serde(default)]
    pub steps: Vec<SetupMachineStep>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupMachineStep {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success: Option<String>,
    #[serde(default)]
    pub on_failure: Vec<Value>,
    #[serde(default)]
    pub recover: Vec<Value>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupMachineState {
    pub schema_version: u32,
    pub provider_id: String,
    pub tenant: String,
    pub team: String,
    pub machine_id: String,
    pub machine_version: u32,
    pub status: SetupMachineStatus,
    pub current_step: Option<String>,
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub failed_step: Option<String>,
    #[serde(default)]
    pub answers_hash: Option<String>,
    #[serde(default)]
    pub pack_fingerprint: Option<String>,
    #[serde(default)]
    pub outputs: Value,
    #[serde(default)]
    pub created_resources: Vec<Value>,
    #[serde(default)]
    pub last_error: Option<Value>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SetupMachineStatus {
    NotStarted,
    Running,
    Paused,
    Complete,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SetupMachineEvent {
    pub ts: String,
    pub level: String,
    pub event: String,
    pub provider_id: String,
    pub tenant: String,
    pub team: String,
    pub machine_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub fields: serde_json::Map<String, Value>,
}

pub fn load_setup_machine_from_pack(pack_path: &Path) -> anyhow::Result<Option<SetupMachine>> {
    let Some(extension) = discovery::read_pack_extension(pack_path, SETUP_MACHINE_EXTENSION)?
    else {
        return Ok(None);
    };
    let machine_value = resolve_machine_value(pack_path, extension)?;
    Ok(Some(serde_json::from_value(machine_value)?))
}

pub fn validate_setup_machine(machine: &SetupMachine) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    if machine.version == 0 {
        diagnostics.push(error(
            "setup.machine.version",
            "setup machine version must be greater than zero",
        ));
    }
    if machine.id.trim().is_empty() {
        diagnostics.push(error("setup.machine.id", "setup machine id is required"));
    }
    if machine.entry_step.trim().is_empty() {
        diagnostics.push(error(
            "setup.machine.entry_step",
            "setup machine entry_step is required",
        ));
    }
    if machine.steps.is_empty() {
        diagnostics.push(error(
            "setup.machine.steps",
            "setup machine must declare at least one step",
        ));
        return diagnostics;
    }

    let mut seen = BTreeSet::new();
    let mut step_ids = BTreeSet::new();
    for step in &machine.steps {
        if step.id.trim().is_empty() {
            diagnostics.push(error("setup.machine.step.id", "setup step id is required"));
            continue;
        }
        if !seen.insert(step.id.clone()) {
            diagnostics.push(error_with_actual(
                "setup.machine.step.unique_id",
                "setup step ids must be unique",
                &step.id,
            ));
        }
        step_ids.insert(step.id.clone());
        if step.kind.trim().is_empty() {
            diagnostics.push(error_with_actual(
                "setup.machine.step.kind",
                "setup step kind is required",
                &step.id,
            ));
        } else if !SUPPORTED_STEP_KINDS.contains(&step.kind.as_str()) {
            diagnostics.push(error_with_actual(
                "setup.machine.step.supported_kind",
                "setup step kind is not supported by this greentic-setup version",
                &step.kind,
            ));
        }
        for required in &step.requires {
            if required.trim().is_empty() {
                diagnostics.push(error_with_actual(
                    "setup.machine.step.requires",
                    "setup step requires entries must be non-empty",
                    &step.id,
                ));
            }
        }
    }

    if !step_ids.contains(&machine.entry_step) {
        diagnostics.push(error_with_actual(
            "setup.machine.entry_step.exists",
            "setup machine entry_step must reference an existing step",
            &machine.entry_step,
        ));
    }

    for step in &machine.steps {
        validate_transition(
            &mut diagnostics,
            &step_ids,
            &step.id,
            "setup.machine.step.on_success",
            step.on_success.as_deref(),
        );
        for rule in step.on_failure.iter().chain(step.recover.iter()) {
            for target in rule_targets(rule) {
                validate_transition(
                    &mut diagnostics,
                    &step_ids,
                    &step.id,
                    "setup.machine.step.rule_target",
                    Some(&target),
                );
            }
        }
    }

    diagnostics.extend(validate_reachability(machine, &step_ids));
    diagnostics.extend(validate_unbounded_cycles(machine, &step_ids));
    diagnostics.extend(validate_artifact_paths(machine));
    diagnostics.extend(validate_http_steps(machine));
    diagnostics.extend(validate_tunnel_steps(machine));
    diagnostics.extend(validate_oauth_steps(machine));
    diagnostics.extend(validate_data_mapping_steps(machine));
    diagnostics
}

pub fn run_provider_setup_machine_doctor(pack_path: &Path) -> DoctorReport {
    let mut diagnostics = Vec::new();
    if !pack_path.is_file() {
        diagnostics.push(Diagnostic {
            check_id: "setup.provider_pack.present".to_string(),
            severity: DiagnosticSeverity::Error,
            component: "provider_setup".to_string(),
            message: "provider pack does not exist or is not a file".to_string(),
            evidence: None,
            expected: Some(".gtpack file".to_string()),
            actual: Some(pack_path.display().to_string()),
            fix_hint: Some("pass a built provider .gtpack file".to_string()),
            related_file: Some(pack_path.display().to_string()),
            related_pack: None,
            related_component: None,
        });
        return report(pack_path, diagnostics);
    }

    let pack_id = discovery::read_pack_meta(pack_path)
        .ok()
        .flatten()
        .map(|meta| meta.pack_id)
        .or_else(|| {
            pack_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        });

    diagnostics.extend(validate_legacy_setup_actions_absent(
        pack_path,
        pack_id.as_deref(),
    ));

    let mut found_setup_contract = false;

    match load_setup_backend_contract_from_pack(pack_path, pack_id.as_deref()) {
        Ok(Some(contract)) => {
            found_setup_contract = true;
            diagnostics.push(info(
                "setup.backend_contract.extension.present",
                "provider pack declares a setup backend contract",
                SETUP_BACKEND_CONTRACT_EXTENSION,
                pack_path,
                contract.get("provider_id").and_then(Value::as_str),
            ));
            diagnostics.extend(validate_json_schema_diagnostics(
                "setup.backend_contract.schema",
                &contract,
                SETUP_BACKEND_CONTRACT_SCHEMA,
                pack_path,
            ));
            diagnostics.extend(validate_setup_backend_contract(&contract, pack_path));
            diagnostics.extend(validate_setup_web_component(pack_path));
            diagnostics.extend(validate_provider_http_routes(pack_path, &contract));
        }
        Ok(None) => {}
        Err(err) => diagnostics.push(load_error(
            "setup.backend_contract.load",
            "provider setup backend contract could not be loaded",
            "valid greentic.setup.backend-contract.v1 JSON",
            err,
            pack_path,
        )),
    }

    match load_setup_machine_from_pack(pack_path) {
        Ok(Some(machine)) => {
            found_setup_contract = true;
            diagnostics.push(info(
                "setup.machine.extension.present",
                "provider pack declares a setup state machine",
                SETUP_MACHINE_EXTENSION,
                pack_path,
                Some(&machine.id),
            ));
            if let Ok(machine_value) = serde_json::to_value(&machine) {
                diagnostics.extend(validate_json_schema_diagnostics(
                    "setup.machine.schema",
                    &machine_value,
                    SETUP_MACHINE_SCHEMA,
                    pack_path,
                ));
            }
            diagnostics.extend(validate_setup_machine(&machine));
            diagnostics.extend(validate_setup_machine_template_inputs(
                pack_path,
                pack_id.as_deref(),
                &machine,
            ));
            diagnostics.extend(validate_setup_i18n_message_keys(pack_path, &machine));
            diagnostics.extend(validate_setup_machine_fixtures(pack_path, &machine));
        }
        Ok(None) => {}
        Err(err) => diagnostics.push(load_error(
            "setup.machine.load",
            "provider setup state machine could not be loaded",
            "valid setup-machine.v1 JSON",
            err,
            pack_path,
        )),
    }

    if !found_setup_contract {
        diagnostics.push(Diagnostic {
            check_id: "setup.provider_contract.present".to_string(),
            severity: DiagnosticSeverity::Error,
            component: "provider_setup".to_string(),
            message: "provider pack does not declare a supported generic setup contract"
                .to_string(),
            evidence: None,
            expected: Some(format!(
                "{SETUP_BACKEND_CONTRACT_EXTENSION} or {SETUP_MACHINE_EXTENSION}"
            )),
            actual: Some("missing".to_string()),
            fix_hint: Some(format!(
                "add {SETUP_BACKEND_CONTRACT_EXTENSION} for the current generic setup contract or {SETUP_MACHINE_EXTENSION} for the future setup-machine contract"
            )),
            related_file: Some(pack_path.display().to_string()),
            related_pack: pack_id,
            related_component: None,
        });
    }

    report(pack_path, diagnostics)
}

fn validate_legacy_setup_actions_absent(
    pack_path: &Path,
    pack_id: Option<&str>,
) -> Vec<Diagnostic> {
    let Some(spec) = crate::setup_input::load_setup_spec(pack_path)
        .ok()
        .flatten()
    else {
        return Vec::new();
    };
    if spec.setup_actions.is_empty() {
        return Vec::new();
    }
    vec![Diagnostic {
        check_id: "setup.legacy_setup_actions.absent".to_string(),
        severity: DiagnosticSeverity::Error,
        component: "provider_setup".to_string(),
        message: "provider setup.yaml still declares legacy setup_actions".to_string(),
        evidence: Some(format!("{} setup action(s)", spec.setup_actions.len())),
        expected: Some(format!(
            "{SETUP_BACKEND_CONTRACT_EXTENSION} or {SETUP_MACHINE_EXTENSION} only"
        )),
        actual: Some("setup.yaml setup_actions".to_string()),
        fix_hint: Some(
            "move provider setup into the generic setup contract and remove setup_actions from setup.yaml"
                .to_string(),
        ),
        related_file: Some(pack_path.display().to_string()),
        related_pack: pack_id.map(str::to_string),
        related_component: None,
    }]
}

fn validate_json_schema_diagnostics(
    check_id: &str,
    value: &Value,
    schema_text: &str,
    pack_path: &Path,
) -> Vec<Diagnostic> {
    let schema = match serde_json::from_str::<Value>(schema_text) {
        Ok(schema) => schema,
        Err(err) => {
            return vec![load_error(
                check_id,
                "setup schema could not be parsed",
                "valid JSON Schema",
                err.into(),
                pack_path,
            )];
        }
    };
    crate::schema_validation::validate(value, &schema)
        .into_iter()
        .map(|error| Diagnostic {
            check_id: check_id.to_string(),
            severity: DiagnosticSeverity::Error,
            component: "provider_setup".to_string(),
            message: "setup contract does not match its JSON schema".to_string(),
            evidence: Some(error),
            expected: schema
                .get("$id")
                .and_then(Value::as_str)
                .map(str::to_string),
            actual: None,
            fix_hint: Some(
                "update provider setup metadata to match the published schema".to_string(),
            ),
            related_file: Some(pack_path.display().to_string()),
            related_pack: value
                .get("provider_id")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string),
            related_component: None,
        })
        .collect()
}

pub fn load_setup_backend_contract_from_pack(
    pack_path: &Path,
    expected_provider_id: Option<&str>,
) -> anyhow::Result<Option<Value>> {
    let Some(extension) =
        discovery::read_pack_extension(pack_path, SETUP_BACKEND_CONTRACT_EXTENSION)?
    else {
        return Ok(None);
    };
    let descriptor = extension_inline(&extension).cloned().unwrap_or(extension);
    validate_schema_id(
        &descriptor,
        SETUP_BACKEND_CONTRACT_EXTENSION,
        "setup backend contract descriptor",
    )?;

    let Some(asset) = descriptor.get("asset").and_then(Value::as_str) else {
        let provider_id = descriptor.get("provider_id").and_then(Value::as_str);
        validate_provider_id(provider_id, expected_provider_id)?;
        return Ok(Some(descriptor));
    };
    let mut contract = discovery::read_pack_json_asset(pack_path, asset)?;
    validate_schema_id(
        &contract,
        SETUP_BACKEND_CONTRACT_EXTENSION,
        "setup backend contract asset",
    )?;
    let provider_id = contract.get("provider_id").and_then(Value::as_str);
    validate_provider_id(provider_id, expected_provider_id)?;
    if let Some(map) = contract.as_object_mut() {
        map.insert(
            "descriptor".to_string(),
            serde_json::json!({
                "schema_id": descriptor.get("schema_id").cloned().unwrap_or(Value::Null),
                "provider_id": descriptor.get("provider_id").cloned().unwrap_or(Value::Null),
                "asset": descriptor.get("asset").cloned().unwrap_or(Value::Null),
            }),
        );
    }
    Ok(Some(contract))
}

fn validate_setup_backend_contract(contract: &Value, pack_path: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let provider_id = contract.get("provider_id").and_then(Value::as_str);

    if provider_id.is_none_or(|value| value.trim().is_empty()) {
        diagnostics.push(error(
            "setup.backend_contract.provider_id",
            "setup backend contract provider_id is required",
        ));
    }

    let required_order = string_array(contract.get("required_order"));
    if required_order.is_empty() {
        diagnostics.push(error(
            "setup.backend_contract.required_order",
            "setup backend contract must declare required_order",
        ));
    }
    diagnostics.extend(validate_unique_strings(
        "setup.backend_contract.required_order.unique",
        "required_order entries must be unique",
        &required_order,
    ));

    let actions = contract
        .get("actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if actions.is_empty() {
        diagnostics.push(error(
            "setup.backend_contract.actions",
            "setup backend contract must declare actions",
        ));
    }

    let server_owned = string_array(contract.get("server_owned_config_keys"));
    let mut action_ids = BTreeSet::new();
    for action in &actions {
        let Some(id) = action.get("id").and_then(Value::as_str) else {
            diagnostics.push(error(
                "setup.backend_contract.action.id",
                "setup backend action id is required",
            ));
            continue;
        };
        if id.trim().is_empty() {
            diagnostics.push(error(
                "setup.backend_contract.action.id",
                "setup backend action id is required",
            ));
            continue;
        }
        if !action_ids.insert(id.to_string()) {
            diagnostics.push(error_with_actual(
                "setup.backend_contract.action.unique_id",
                "setup backend action ids must be unique",
                id,
            ));
        }

        for required in string_array(action.get("requires")) {
            if !required_order.contains(&required) && !action_ids.contains(&required) {
                diagnostics.push(error_with_actual(
                    "setup.backend_contract.action.requires",
                    "setup backend action requires entry must reference another declared action",
                    &format!("{id} requires {required}"),
                ));
            }
        }

        validate_backend_executor(&mut diagnostics, id, action, pack_path, &server_owned);

        if action.get("completion").is_none() {
            diagnostics.push(error_with_actual(
                "setup.backend_contract.action.completion",
                "setup backend action should declare completion criteria",
                id,
            ));
        }
    }

    for step in &required_order {
        if !action_ids.contains(step) {
            diagnostics.push(error_with_actual(
                "setup.backend_contract.required_order.action",
                "required_order entry must have a matching action",
                step,
            ));
        }
    }

    diagnostics.extend(validate_unique_strings(
        "setup.backend_contract.server_owned_config_keys.unique",
        "server_owned_config_keys entries must be unique",
        &server_owned,
    ));

    diagnostics
}

fn validate_backend_executor(
    diagnostics: &mut Vec<Diagnostic>,
    action_id: &str,
    action: &Value,
    _pack_path: &Path,
    server_owned: &[String],
) {
    let Some(executor) = action.get("executor").and_then(Value::as_object) else {
        diagnostics.push(error_with_actual(
            "setup.backend_contract.action.executor",
            "setup backend action must declare executor",
            action_id,
        ));
        return;
    };
    let kind = executor
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind.is_empty() {
        diagnostics.push(error_with_actual(
            "setup.backend_contract.executor.kind",
            "setup backend executor kind is required",
            action_id,
        ));
        return;
    }
    if !SUPPORTED_BACKEND_EXECUTOR_KINDS.contains(&kind) {
        diagnostics.push(error_with_actual(
            "setup.backend_contract.executor.supported_kind",
            "setup backend executor kind is not supported by this greentic-setup version",
            kind,
        ));
        return;
    }

    match kind {
        "oauth_device_code" => {
            for key in [
                "authority_url_template",
                "client_id_config_key",
                "oauth_kind",
                "token_store_key",
            ] {
                require_executor_str(diagnostics, action_id, executor, key);
            }
            if string_array(executor.get("scopes")).is_empty() {
                diagnostics.push(error_with_actual(
                    "setup.backend_contract.executor.scopes",
                    "oauth_device_code executor requires at least one scope",
                    action_id,
                ));
            }
            validate_oauth_server_owned_keys(diagnostics, action_id, executor, server_owned);
        }
        "microsoft_graph_application" => {
            for key in [
                "graph_token_store_key",
                "display_name_config_key",
                "app_id_config_key",
                "client_secret_config_key",
            ] {
                require_executor_str(diagnostics, action_id, executor, key);
            }
        }
        "provider_http" => {
            let path_template = executor
                .get("path_template")
                .or_else(|| executor.get("target_path_template"))
                .and_then(Value::as_str);
            let url_template = executor
                .get("url_template")
                .or_else(|| executor.get("target_url_template"))
                .and_then(Value::as_str);
            if path_template.is_none() && url_template.is_none() {
                diagnostics.push(error_with_actual(
                    "setup.backend_contract.executor.provider_http.target",
                    "provider_http executor requires path_template or url_template",
                    action_id,
                ));
            }
            if let Some(path) = path_template
                && !path.starts_with('/')
            {
                diagnostics.push(error_with_actual(
                    "setup.backend_contract.executor.provider_http.path",
                    "provider_http path_template must be an absolute path",
                    path,
                ));
            }
        }
        "runtime_observation" => {
            for key in ["source", "event", "state_store_key"] {
                require_executor_str(diagnostics, action_id, executor, key);
            }
        }
        _ => {}
    }
}

fn validate_oauth_server_owned_keys(
    diagnostics: &mut Vec<Diagnostic>,
    action_id: &str,
    executor: &serde_json::Map<String, Value>,
    server_owned: &[String],
) {
    let oauth_kind_key = "oauth_kind";
    validate_server_owned_key(diagnostics, action_id, server_owned, oauth_kind_key);

    for key in [
        "token_store_key",
        "device_code_store_key",
        "user_code_store_key",
    ] {
        if let Some(value) = executor.get(key).and_then(Value::as_str) {
            validate_server_owned_key(diagnostics, action_id, server_owned, value);
        }
    }

    if !executor.contains_key("device_code_store_key") {
        validate_server_owned_key(diagnostics, action_id, server_owned, "oauth_device_code");
    }
    if !executor.contains_key("user_code_store_key") {
        validate_server_owned_key(diagnostics, action_id, server_owned, "oauth_user_code");
    }
}

fn validate_server_owned_key(
    diagnostics: &mut Vec<Diagnostic>,
    action_id: &str,
    server_owned: &[String],
    key: &str,
) {
    if server_owned.iter().any(|value| value == key) {
        return;
    }
    diagnostics.push(error_with_actual(
        "setup.backend_contract.server_owned_config_keys.oauth",
        "oauth_device_code executor output keys must be listed in server_owned_config_keys",
        &format!("{action_id}.{key}"),
    ));
}

fn validate_setup_web_component(pack_path: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    match discovery::read_pack_extension(pack_path, SETUP_WEB_COMPONENT_EXTENSION) {
        Ok(Some(extension)) => {
            let descriptor = extension_inline(&extension).unwrap_or(&extension);
            if let Err(err) = validate_schema_id(
                descriptor,
                SETUP_WEB_COMPONENT_EXTENSION,
                "setup web component descriptor",
            ) {
                diagnostics.push(load_error(
                    "setup.web_component.schema_id",
                    "setup web component descriptor has invalid schema_id",
                    SETUP_WEB_COMPONENT_EXTENSION,
                    err,
                    pack_path,
                ));
            }
            for key in ["provider_id", "tag_name", "module_asset"] {
                if descriptor
                    .get(key)
                    .and_then(Value::as_str)
                    .is_none_or(|value| value.trim().is_empty())
                {
                    diagnostics.push(error_with_actual(
                        "setup.web_component.required_field",
                        "setup web component descriptor is missing a required field",
                        key,
                    ));
                }
            }
            if let Some(module_asset) = descriptor.get("module_asset").and_then(Value::as_str) {
                match discovery::pack_asset_exists(pack_path, module_asset) {
                    Ok(true) => {}
                    Ok(false) => diagnostics.push(error_with_actual(
                        "setup.web_component.module_asset.exists",
                        "setup web component module_asset must exist in the pack",
                        module_asset,
                    )),
                    Err(err) => diagnostics.push(load_error(
                        "setup.web_component.module_asset.read",
                        "setup web component module_asset could not be checked",
                        "readable pack asset",
                        err,
                        pack_path,
                    )),
                }
            }
        }
        Ok(None) => diagnostics.push(Diagnostic {
            check_id: "setup.web_component.extension.present".to_string(),
            severity: DiagnosticSeverity::Error,
            component: "provider_setup".to_string(),
            message: "provider pack does not declare a setup web component".to_string(),
            evidence: None,
            expected: Some(SETUP_WEB_COMPONENT_EXTENSION.to_string()),
            actual: Some("missing".to_string()),
            fix_hint: Some(format!(
                "add {SETUP_WEB_COMPONENT_EXTENSION} to the pack manifest"
            )),
            related_file: Some(pack_path.display().to_string()),
            related_pack: None,
            related_component: None,
        }),
        Err(err) => diagnostics.push(load_error(
            "setup.web_component.load",
            "setup web component descriptor could not be loaded",
            SETUP_WEB_COMPONENT_EXTENSION,
            err,
            pack_path,
        )),
    }
    diagnostics
}

fn validate_provider_http_routes(pack_path: &Path, contract: &Value) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let provider_http_routes = provider_http_requirements(contract);
    if provider_http_routes.is_empty() {
        return diagnostics;
    }

    let routes_extension = match discovery::read_pack_extension(pack_path, HTTP_ROUTES_EXTENSION) {
        Ok(Some(extension)) => extension,
        Ok(None) => {
            diagnostics.push(Diagnostic {
                check_id: "setup.backend_contract.provider_http.routes_extension".to_string(),
                severity: DiagnosticSeverity::Error,
                component: "provider_setup".to_string(),
                message: "provider_http setup actions require greentic.http-routes.v1".to_string(),
                evidence: Some(
                    provider_http_routes
                        .iter()
                        .map(|route| route.path.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                expected: Some(HTTP_ROUTES_EXTENSION.to_string()),
                actual: Some("missing".to_string()),
                fix_hint: Some(format!(
                    "declare {HTTP_ROUTES_EXTENSION} routes for provider_http setup actions"
                )),
                related_file: Some(pack_path.display().to_string()),
                related_pack: contract
                    .get("provider_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                related_component: None,
            });
            return diagnostics;
        }
        Err(err) => {
            diagnostics.push(load_error(
                "setup.backend_contract.provider_http.routes_load",
                "greentic.http-routes.v1 could not be loaded",
                HTTP_ROUTES_EXTENSION,
                err,
                pack_path,
            ));
            return diagnostics;
        }
    };

    let routes = extension_inline(&routes_extension).unwrap_or(&routes_extension);
    let declared_routes: Vec<DeclaredHttpRoute> = routes
        .get("routes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(declared_http_route)
        .collect();
    if declared_routes.is_empty() {
        diagnostics.push(error(
            "setup.backend_contract.provider_http.routes",
            "greentic.http-routes.v1 must declare routes for provider_http setup actions",
        ));
        return diagnostics;
    }

    for required in provider_http_routes {
        let Some(route) = declared_routes
            .iter()
            .find(|route| route_pattern_covers_path(&route.pattern, &required.path))
        else {
            diagnostics.push(error_with_actual(
                "setup.backend_contract.provider_http.route_covered",
                "provider_http path_template is not covered by greentic.http-routes.v1",
                &required.path,
            ));
            continue;
        };
        if !route.methods.is_empty()
            && !route
                .methods
                .iter()
                .any(|method| method.eq_ignore_ascii_case(&required.method))
        {
            diagnostics.push(error_with_actual(
                "setup.backend_contract.provider_http.route_method",
                "provider_http method is not allowed by greentic.http-routes.v1",
                &format!("{} {}", required.method, required.path),
            ));
        }
    }

    diagnostics
}

#[derive(Clone, Debug)]
struct ProviderHttpRequirement {
    method: String,
    path: String,
}

#[derive(Clone, Debug)]
struct DeclaredHttpRoute {
    pattern: String,
    methods: Vec<String>,
}

fn provider_http_requirements(contract: &Value) -> Vec<ProviderHttpRequirement> {
    contract
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| action.get("executor"))
        .filter(|executor| executor.get("kind").and_then(Value::as_str) == Some("provider_http"))
        .filter_map(|executor| {
            let path = executor
                .get("path_template")
                .or_else(|| executor.get("target_path_template"))
                .and_then(Value::as_str)?;
            let method = executor
                .get("method")
                .or_else(|| executor.get("http_method"))
                .and_then(Value::as_str)
                .unwrap_or("POST");
            Some(ProviderHttpRequirement {
                method: method.trim().to_ascii_uppercase(),
                path: path.trim().to_string(),
            })
        })
        .filter(|route| !route.path.is_empty())
        .collect()
}

fn declared_http_route(record: &Value) -> Option<DeclaredHttpRoute> {
    let pattern = record.get("pattern").and_then(Value::as_str)?.trim();
    if pattern.is_empty() {
        return None;
    }
    let methods = record
        .get("methods")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_uppercase())
        .collect();
    Some(DeclaredHttpRoute {
        pattern: pattern.to_string(),
        methods,
    })
}

fn route_pattern_covers_path(pattern: &str, path: &str) -> bool {
    let pattern_segments: Vec<&str> = normalized_path_segments(pattern).collect();
    let path_segments: Vec<&str> = normalized_path_segments(path).collect();
    route_segments_cover(&pattern_segments, &path_segments)
}

fn normalized_path_segments(path: &str) -> impl Iterator<Item = &str> {
    path.trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
}

fn route_segments_cover(pattern: &[&str], path: &[&str]) -> bool {
    let mut p = 0;
    let mut s = 0;
    while p < pattern.len() {
        let pattern_segment = pattern[p];
        if is_route_splat(pattern_segment) {
            return true;
        }
        let Some(path_segment) = path.get(s) else {
            return false;
        };
        if !route_segment_covers(pattern_segment, path_segment) {
            return false;
        }
        p += 1;
        s += 1;
    }
    s == path.len()
}

fn route_segment_covers(pattern_segment: &str, path_segment: &str) -> bool {
    pattern_segment == path_segment
        || is_route_variable(pattern_segment) && is_route_variable(path_segment)
}

fn is_route_variable(segment: &str) -> bool {
    segment.starts_with('{') && segment.ends_with('}') && !segment.ends_with("*}")
}

fn is_route_splat(segment: &str) -> bool {
    segment.starts_with('{') && segment.ends_with("*}")
}

pub fn setup_machine_state_dir(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> PathBuf {
    bundle_root
        .join("state")
        .join("setup")
        .join(tenant)
        .join(team)
        .join(provider_id)
}

pub fn setup_machine_state_path(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> PathBuf {
    setup_machine_state_dir(bundle_root, tenant, team, provider_id).join("machine.json")
}

pub fn setup_machine_events_path(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> PathBuf {
    setup_machine_state_dir(bundle_root, tenant, team, provider_id).join("events.jsonl")
}

pub fn write_setup_machine_state(
    bundle_root: &Path,
    state: &SetupMachineState,
) -> anyhow::Result<PathBuf> {
    let path =
        setup_machine_state_path(bundle_root, &state.tenant, &state.team, &state.provider_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(state)?)?;
    Ok(path)
}

pub fn load_setup_machine_state(path: &Path) -> anyhow::Result<SetupMachineState> {
    let raw = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn append_setup_machine_event(
    bundle_root: &Path,
    event: &SetupMachineEvent,
) -> anyhow::Result<PathBuf> {
    let path =
        setup_machine_events_path(bundle_root, &event.tenant, &event.team, &event.provider_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(path)
}

pub fn load_or_init_setup_machine_state(
    bundle_root: &Path,
    provider_id: &str,
    tenant: &str,
    team: &str,
    machine: &SetupMachine,
) -> anyhow::Result<SetupMachineState> {
    load_or_init_setup_machine_state_with_context(
        bundle_root,
        None,
        provider_id,
        tenant,
        team,
        machine,
    )
}

pub fn load_or_init_setup_machine_state_with_context(
    bundle_root: &Path,
    pack_path: Option<&Path>,
    provider_id: &str,
    tenant: &str,
    team: &str,
    machine: &SetupMachine,
) -> anyhow::Result<SetupMachineState> {
    let current_pack_fingerprint = pack_path
        .map(setup_machine_hash_file)
        .transpose()
        .with_context(|| format!("hash setup provider pack for {provider_id}"))?;
    let current_answers_hash = setup_machine_answers_hash(bundle_root, provider_id)?;
    let path = setup_machine_state_path(bundle_root, tenant, team, provider_id);
    if path.is_file() {
        let mut state = load_setup_machine_state(&path)?;
        if state.schema_version != SUPPORTED_SETUP_STATE_SCHEMA_VERSION {
            state.status = SetupMachineStatus::Failed;
            state.last_error = Some(serde_json::json!({
                "error": "setup_state_schema_changed",
                "recoverable": false,
                "expected": SUPPORTED_SETUP_STATE_SCHEMA_VERSION,
                "actual": state.schema_version,
            }));
        }
        if state.provider_id != provider_id || state.tenant != tenant || state.team != team {
            state.status = SetupMachineStatus::Failed;
            state.last_error = Some(serde_json::json!({
                "error": "setup_state_scope_changed",
                "recoverable": false,
                "expected": {
                    "provider_id": provider_id,
                    "tenant": tenant,
                    "team": team,
                },
                "actual": {
                    "provider_id": state.provider_id,
                    "tenant": state.tenant,
                    "team": state.team,
                }
            }));
        }
        if state.machine_id != machine.id || state.machine_version != machine.version {
            state.status = SetupMachineStatus::Failed;
            state.last_error = Some(serde_json::json!({
                "error": "machine_identity_changed",
                "recoverable": false,
                "expected": {
                    "machine_id": machine.id,
                    "machine_version": machine.version,
                },
                "actual": {
                    "machine_id": state.machine_id,
                    "machine_version": state.machine_version,
                }
            }));
        }
        if let (Some(stored), Some(current)) = (
            state.pack_fingerprint.as_deref(),
            current_pack_fingerprint.as_deref(),
        ) && stored != current
        {
            state.status = SetupMachineStatus::Failed;
            state.last_error = Some(serde_json::json!({
                "error": "pack_fingerprint_changed",
                "recoverable": false,
                "expected": stored,
                "actual": current,
            }));
        } else if state.pack_fingerprint.is_none() {
            state.pack_fingerprint = current_pack_fingerprint;
        }
        if state.answers_hash.as_deref() != current_answers_hash.as_deref() {
            if let Some(stored) = state.answers_hash.as_deref() {
                state.status = SetupMachineStatus::Failed;
                state.last_error = Some(serde_json::json!({
                    "error": "setup_answers_changed",
                    "recoverable": false,
                    "expected": stored,
                    "actual": current_answers_hash.as_deref().unwrap_or("missing"),
                }));
            } else {
                state.answers_hash = current_answers_hash;
            }
        }
        return Ok(state);
    }

    Ok(SetupMachineState {
        schema_version: 1,
        provider_id: provider_id.to_string(),
        tenant: tenant.to_string(),
        team: team.to_string(),
        machine_id: machine.id.clone(),
        machine_version: machine.version,
        status: SetupMachineStatus::NotStarted,
        current_step: Some(machine.entry_step.clone()),
        completed_steps: Vec::new(),
        failed_step: None,
        answers_hash: current_answers_hash,
        pack_fingerprint: current_pack_fingerprint,
        outputs: serde_json::json!({}),
        created_resources: Vec::new(),
        last_error: None,
        updated_at: Some(now_setup_ts()),
    })
}

fn setup_machine_state_compatibility_error(state: &SetupMachineState) -> Option<String> {
    state
        .last_error
        .as_ref()
        .and_then(|error| error.get("error"))
        .and_then(Value::as_str)
        .filter(|error| {
            matches!(
                *error,
                "setup_state_schema_changed"
                    | "setup_state_scope_changed"
                    | "machine_identity_changed"
                    | "pack_fingerprint_changed"
                    | "setup_answers_changed"
            )
        })
        .map(str::to_string)
}

fn setup_machine_answers_hash(
    bundle_root: &Path,
    provider_id: &str,
) -> anyhow::Result<Option<String>> {
    let path = bundle_root
        .join("state/config")
        .join(provider_id)
        .join("setup-answers.json");
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(setup_machine_hash_file(&path)?))
}

fn setup_machine_hash_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!(
        "sha256:{}",
        setup_machine_hex_encode(&hasher.finalize())
    ))
}

fn setup_machine_hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn render_setup_machine_status(machine: &SetupMachine, state: &SetupMachineState) -> Value {
    let current = state
        .current_step
        .clone()
        .unwrap_or_else(|| machine.entry_step.clone());
    let completed: BTreeSet<&str> = state.completed_steps.iter().map(String::as_str).collect();
    let steps: Vec<Value> = machine
        .steps
        .iter()
        .map(|step| {
            let step_state = if completed.contains(step.id.as_str()) {
                "done"
            } else if Some(step.id.as_str()) == state.failed_step.as_deref() {
                "failed"
            } else if step.id == current {
                "current"
            } else {
                "pending"
            };
            serde_json::json!({
                "id": step.id,
                "kind": step.kind,
                "title": step.title,
                "state": step_state,
            })
        })
        .collect();
    serde_json::json!({
        "ok": state.status == SetupMachineStatus::Complete,
        "provider_id": state.provider_id,
        "tenant": state.tenant,
        "team": state.team,
        "machine_id": state.machine_id,
        "machine_version": state.machine_version,
        "status": state.status,
        "current_step": current,
        "completed_steps": state.completed_steps,
        "failed_step": state.failed_step,
        "last_error": state.last_error,
        "steps": steps,
    })
}

pub async fn complete_setup_machine_oauth_authorization_code_with_token_response(
    bundle_root: &Path,
    env: &str,
    oauth_state: &crate::setup_actions::OAuthStatePayload,
    token_response: &Value,
) -> anyhow::Result<crate::oauth_callback::OAuthCallbackReport> {
    let discovered = discovery::discover(bundle_root)
        .context("failed to discover providers for setup-machine OAuth callback")?;
    let provider = discovered
        .find_setup_target(&oauth_state.provider_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider not found for setup-machine OAuth callback: {}",
                oauth_state.provider_id
            )
        })?;
    let machine = load_setup_machine_from_pack(&provider.pack_path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "provider does not declare {SETUP_MACHINE_EXTENSION}: {}",
            oauth_state.provider_id
        )
    })?;
    complete_setup_machine_oauth_authorization_code_for_machine(
        bundle_root,
        env,
        Some(&provider.pack_path),
        &machine,
        oauth_state,
        token_response,
    )
    .await
}

pub async fn complete_setup_machine_oauth_authorization_code(
    bundle_root: &Path,
    env: &str,
    oauth_state: &crate::setup_actions::OAuthStatePayload,
    code: &str,
) -> anyhow::Result<crate::oauth_callback::OAuthCallbackReport> {
    if code.trim().is_empty() {
        anyhow::bail!("OAuth callback missing code");
    }
    let discovered = discovery::discover(bundle_root)
        .context("failed to discover providers for setup-machine OAuth callback")?;
    let provider = discovered
        .find_setup_target(&oauth_state.provider_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "provider not found for setup-machine OAuth callback: {}",
                oauth_state.provider_id
            )
        })?;
    let machine = load_setup_machine_from_pack(&provider.pack_path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "provider does not declare {SETUP_MACHINE_EXTENSION}: {}",
            oauth_state.provider_id
        )
    })?;
    let token_response = exchange_setup_machine_oauth_authorization_code(
        bundle_root,
        &machine,
        oauth_state,
        code.trim(),
    )?;
    complete_setup_machine_oauth_authorization_code_for_machine(
        bundle_root,
        env,
        Some(&provider.pack_path),
        &machine,
        oauth_state,
        &token_response,
    )
    .await
}

pub async fn complete_setup_machine_oauth_authorization_code_for_machine(
    bundle_root: &Path,
    env: &str,
    pack_path: Option<&Path>,
    machine: &SetupMachine,
    oauth_state: &crate::setup_actions::OAuthStatePayload,
    token_response: &Value,
) -> anyhow::Result<crate::oauth_callback::OAuthCallbackReport> {
    let step = machine
        .steps
        .iter()
        .find(|candidate| candidate.id == oauth_state.action_id)
        .ok_or_else(|| anyhow::anyhow!("setup-machine step not found: {}", oauth_state.action_id))?
        .clone();
    if step.kind != "oauth_authorization_code" {
        anyhow::bail!(
            "setup-machine step is not an oauth_authorization_code step: {}",
            step.id
        );
    }

    let mut state = load_or_init_setup_machine_state(
        bundle_root,
        &oauth_state.provider_id,
        &oauth_state.tenant,
        &oauth_state.team,
        machine,
    )?;
    if state.completed_steps.iter().any(|id| id == &step.id) {
        anyhow::bail!("setup-machine OAuth step is already complete");
    }
    if !matches!(
        state.status,
        SetupMachineStatus::Running | SetupMachineStatus::Paused | SetupMachineStatus::NotStarted
    ) {
        anyhow::bail!("setup-machine OAuth step is not pending");
    }
    if state.current_step.as_deref() != Some(step.id.as_str()) {
        anyhow::bail!(
            "setup-machine OAuth callback step mismatch: current={:?} callback={}",
            state.current_step,
            step.id
        );
    }

    let config = map_oauth_authorization_token_response(&step, token_response)?;
    let persisted_keys = crate::qa::persist::persist_all_config_as_secrets(
        bundle_root,
        env,
        &oauth_state.tenant,
        Some(&oauth_state.team),
        &oauth_state.provider_id,
        &Value::Object(config.clone()),
        pack_path,
    )
    .await?;

    ensure_outputs_object(&mut state).insert(
        step.extra
            .get("output_key")
            .and_then(Value::as_str)
            .unwrap_or("oauth_authorization")
            .to_string(),
        serde_json::json!({
            "ok": true,
            "persisted_keys": persisted_keys,
        }),
    );
    mark_machine_step_complete(machine, &step, &mut state);
    state.updated_at = Some(now_setup_ts());
    write_setup_machine_state(bundle_root, &state)?;
    append_machine_step_event(
        bundle_root,
        &state,
        "setup.step.oauth_callback_complete",
        Some(&step.id),
        Some(serde_json::json!({
            "persisted_keys": persisted_keys,
        })),
    )?;

    Ok(crate::oauth_callback::OAuthCallbackReport {
        provider_id: oauth_state.provider_id.clone(),
        tenant: oauth_state.tenant.clone(),
        team: oauth_state.team.clone(),
        action_id: oauth_state.action_id.clone(),
        persisted_secret_keys: persisted_keys,
    })
}

pub fn advance_setup_machine(
    bundle_root: &Path,
    provider_id: &str,
    tenant: &str,
    team: &str,
    machine: &SetupMachine,
    dry_run: bool,
) -> anyhow::Result<Value> {
    advance_setup_machine_with_pack(
        bundle_root,
        None,
        provider_id,
        tenant,
        team,
        machine,
        dry_run,
    )
}

pub fn advance_setup_machine_with_pack(
    bundle_root: &Path,
    pack_path: Option<&Path>,
    provider_id: &str,
    tenant: &str,
    team: &str,
    machine: &SetupMachine,
    dry_run: bool,
) -> anyhow::Result<Value> {
    let mut state = load_or_init_setup_machine_state_with_context(
        bundle_root,
        pack_path,
        provider_id,
        tenant,
        team,
        machine,
    )?;
    let before = render_setup_machine_status(machine, &state);
    if state.status == SetupMachineStatus::Complete {
        return Ok(serde_json::json!({
            "provider_id": provider_id,
            "tenant": tenant,
            "team": team,
            "machine_id": machine.id,
            "ok": true,
            "step": "complete",
            "dry_run": dry_run,
            "result": {
                "ok": true,
                "complete": true,
            },
            "status": before,
        }));
    }
    if let Some(error) = setup_machine_state_compatibility_error(&state) {
        state.updated_at = Some(now_setup_ts());
        let event_path = if dry_run {
            None
        } else {
            write_setup_machine_state(bundle_root, &state)?;
            Some(append_machine_step_event(
                bundle_root,
                &state,
                "setup.machine.incompatible_state",
                state.current_step.as_deref(),
                state.last_error.clone(),
            )?)
        };
        let step_id = state
            .current_step
            .clone()
            .unwrap_or_else(|| machine.entry_step.clone());
        return Ok(machine_step_output(
            machine,
            &state,
            &step_id,
            dry_run,
            false,
            &format!("{error}; run setup-migrate or setup-reset before continuing"),
            event_path,
        ));
    }

    let step_id = state
        .current_step
        .clone()
        .unwrap_or_else(|| machine.entry_step.clone());
    let Some(step) = machine
        .steps
        .iter()
        .find(|candidate| candidate.id == step_id)
    else {
        state.status = SetupMachineStatus::Failed;
        state.failed_step = Some(step_id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "missing_current_step",
            "step": step_id,
            "recoverable": false,
        }));
        state.updated_at = Some(now_setup_ts());
        if !dry_run {
            write_setup_machine_state(bundle_root, &state)?;
            append_machine_step_event(
                bundle_root,
                &state,
                "setup.step.failed",
                Some(&step_id),
                None,
            )?;
        }
        return Ok(machine_step_output(
            machine,
            &state,
            &step_id,
            dry_run,
            false,
            "missing_current_step",
            None,
        ));
    };

    let mut state_after = state.clone();
    state_after.status = SetupMachineStatus::Running;
    let result = execute_machine_step(bundle_root, pack_path, machine, step, &mut state_after);
    let recovery_step = apply_declared_failure_transition(machine, step, &mut state_after, &result);
    state_after.updated_at = Some(now_setup_ts());
    let event_name = if recovery_step.is_some() {
        "setup.step.recovery_scheduled"
    } else if result.ok {
        "setup.step.completed"
    } else if state_after.status == SetupMachineStatus::Paused {
        "setup.step.paused"
    } else {
        "setup.step.failed"
    };
    let mut result_detail = result.detail;
    if let Some(recovery_step) = recovery_step.as_deref()
        && let Some(obj) = result_detail.as_object_mut()
    {
        obj.insert(
            "recovery_step".to_string(),
            Value::String(recovery_step.to_string()),
        );
    }
    let event_path = if dry_run {
        None
    } else {
        write_setup_machine_state(bundle_root, &state_after)?;
        Some(append_machine_step_event(
            bundle_root,
            &state_after,
            event_name,
            Some(&step.id),
            Some(result_detail.clone()),
        )?)
    };
    let next_text = recovery_step
        .as_deref()
        .map(|step| format!("recovery scheduled: {step}"))
        .unwrap_or_else(|| result.next.clone());
    let mut output = machine_step_output(
        machine,
        &state_after,
        &step.id,
        dry_run,
        result.ok,
        &next_text,
        event_path,
    );
    if let Some(result_obj) = output.get_mut("result").and_then(Value::as_object_mut) {
        result_obj.insert("detail".to_string(), result_detail);
    }
    Ok(output)
}

pub fn reset_setup_machine_state(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    reason: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let state_path = setup_machine_state_path(bundle_root, tenant, team, provider_id);
    let archive_path = if state_path.is_file() {
        let archive_dir =
            setup_machine_state_dir(bundle_root, tenant, team, provider_id).join("archive");
        std::fs::create_dir_all(&archive_dir)?;
        let archive_path = archive_dir.join(format!(
            "machine-{}-{}.json",
            safe_archive_segment(reason),
            now_setup_ms()
        ));
        std::fs::copy(&state_path, &archive_path)?;
        std::fs::remove_file(&state_path)?;
        Some(archive_path)
    } else {
        None
    };
    append_setup_machine_event(
        bundle_root,
        &SetupMachineEvent {
            ts: now_setup_ts(),
            level: "info".to_string(),
            event: "setup.machine.reset".to_string(),
            provider_id: provider_id.to_string(),
            tenant: tenant.to_string(),
            team: team.to_string(),
            machine_id: provider_id.to_string(),
            step_id: None,
            correlation_id: Some(format!("setup-{}", now_setup_ms())),
            fields: serde_json::json!({
                "reason": reason,
                "archive_path": archive_path,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        },
    )?;
    Ok(archive_path)
}

#[derive(Clone, Debug)]
pub struct SetupMachineMigration {
    pub legacy_backend_path: PathBuf,
    pub legacy_setup_actions_path: PathBuf,
    pub state_path: PathBuf,
    pub backend_archive_path: Option<PathBuf>,
    pub setup_actions_archive_path: Option<PathBuf>,
    pub event_path: PathBuf,
    pub source: String,
    pub legacy_backend_removed: bool,
    pub setup_actions_removed: bool,
    pub initialized: bool,
    pub empty: bool,
}

pub fn migrate_setup_machine_state(
    bundle_root: &Path,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
    machine: &SetupMachine,
) -> anyhow::Result<SetupMachineMigration> {
    let state_path = setup_machine_state_path(bundle_root, tenant, team, provider_id);
    let legacy_backend_path = crate::setup_backend_contract::legacy_backend_state_path(
        bundle_root,
        env,
        tenant,
        team,
        provider_id,
    )?;
    let legacy_setup_actions_path =
        crate::setup_actions::setup_actions_state_path(bundle_root, tenant, team, provider_id);
    let had_machine_state = state_path.is_file();
    let had_legacy_backend = legacy_backend_path.is_file();
    let had_setup_actions = legacy_setup_actions_path.is_file();

    let mut state =
        load_or_init_setup_machine_state(bundle_root, provider_id, tenant, team, machine)?;
    if !had_machine_state {
        state.status = SetupMachineStatus::NotStarted;
        state.current_step = Some(machine.entry_step.clone());
        state.updated_at = Some(now_setup_ts());
        write_setup_machine_state(bundle_root, &state)?;
    }

    let backend_archive_path = if legacy_backend_path.is_file() {
        archive_setup_machine_file(
            bundle_root,
            tenant,
            team,
            provider_id,
            &legacy_backend_path,
            "legacy-backend",
        )?
    } else {
        None
    };
    let legacy_backend_removed = if legacy_backend_path.is_file() {
        std::fs::remove_file(&legacy_backend_path).with_context(|| {
            format!(
                "remove legacy setup backend {}",
                legacy_backend_path.display()
            )
        })?;
        true
    } else {
        false
    };
    let setup_actions_archive_path = if legacy_setup_actions_path.is_file() {
        archive_setup_machine_file(
            bundle_root,
            tenant,
            team,
            provider_id,
            &legacy_setup_actions_path,
            "legacy-setup-actions",
        )?
    } else {
        None
    };
    let setup_actions_removed = if legacy_setup_actions_path.is_file() {
        std::fs::remove_file(&legacy_setup_actions_path).with_context(|| {
            format!(
                "remove legacy setup actions {}",
                legacy_setup_actions_path.display()
            )
        })?;
        true
    } else {
        false
    };
    let source = if had_machine_state {
        "machine"
    } else if had_legacy_backend || had_setup_actions {
        "legacy-archived"
    } else {
        "empty"
    };
    let event_path = append_setup_machine_event(
        bundle_root,
        &SetupMachineEvent {
            ts: now_setup_ts(),
            level: "info".to_string(),
            event: "setup.machine.legacy_migrated".to_string(),
            provider_id: provider_id.to_string(),
            tenant: tenant.to_string(),
            team: team.to_string(),
            machine_id: machine.id.clone(),
            step_id: None,
            correlation_id: Some(format!("setup-{}", now_setup_ms())),
            fields: serde_json::json!({
                "legacy_backend_path": legacy_backend_path,
                "legacy_setup_actions_path": legacy_setup_actions_path,
                "state_path": state_path,
                "backend_archive_path": backend_archive_path,
                "setup_actions_archive_path": setup_actions_archive_path,
                "source": source,
                "legacy_backend_removed": legacy_backend_removed,
                "setup_actions_removed": setup_actions_removed,
                "initialized": !had_machine_state,
                "empty": !had_machine_state && !had_legacy_backend && !had_setup_actions,
            })
            .as_object()
            .cloned()
            .unwrap_or_default(),
        },
    )?;

    Ok(SetupMachineMigration {
        legacy_backend_path,
        legacy_setup_actions_path,
        state_path,
        backend_archive_path,
        setup_actions_archive_path,
        event_path,
        source: source.to_string(),
        legacy_backend_removed,
        setup_actions_removed,
        initialized: !had_machine_state,
        empty: !had_machine_state && !had_legacy_backend && !had_setup_actions,
    })
}

fn archive_setup_machine_file(
    bundle_root: &Path,
    tenant: &str,
    team: &str,
    provider_id: &str,
    source_path: &Path,
    label: &str,
) -> anyhow::Result<Option<PathBuf>> {
    if !source_path.is_file() {
        return Ok(None);
    }
    let archive_dir =
        setup_machine_state_dir(bundle_root, tenant, team, provider_id).join("archive");
    std::fs::create_dir_all(&archive_dir)?;
    let archive_path = archive_dir.join(format!(
        "{}-{}.json",
        safe_archive_segment(label),
        now_setup_ms()
    ));
    std::fs::copy(source_path, &archive_path)?;
    Ok(Some(archive_path))
}

pub fn retry_setup_machine_step(
    bundle_root: &Path,
    provider_id: &str,
    tenant: &str,
    team: &str,
    machine: &SetupMachine,
    step: Option<&str>,
) -> anyhow::Result<Value> {
    let mut state =
        load_or_init_setup_machine_state(bundle_root, provider_id, tenant, team, machine)?;
    let selected_step = step
        .map(str::to_string)
        .or_else(|| state.failed_step.clone())
        .or_else(|| state.current_step.clone())
        .unwrap_or_else(|| machine.entry_step.clone());
    if !machine
        .steps
        .iter()
        .any(|candidate| candidate.id == selected_step)
    {
        anyhow::bail!("setup-machine step not found: {selected_step}");
    }
    let changed = state.last_error.is_some()
        || state.failed_step.as_deref() == Some(selected_step.as_str())
        || state.status == SetupMachineStatus::Paused
        || state.status == SetupMachineStatus::Failed;
    state.current_step = Some(selected_step.clone());
    state.failed_step = None;
    state.last_error = None;
    if state.status != SetupMachineStatus::Complete {
        state.status = SetupMachineStatus::Running;
    }
    state.updated_at = Some(now_setup_ts());
    write_setup_machine_state(bundle_root, &state)?;
    let event_path = append_machine_step_event(
        bundle_root,
        &state,
        "setup.step.retry_requested",
        Some(&selected_step),
        Some(serde_json::json!({
            "changed": changed,
        })),
    )?;
    Ok(serde_json::json!({
        "provider_id": provider_id,
        "tenant": tenant,
        "team": team,
        "machine_id": machine.id,
        "step": selected_step,
        "changed": changed,
        "event_path": event_path,
        "status": render_setup_machine_status(machine, &state),
    }))
}

#[derive(Clone, Debug)]
struct MachineStepResult {
    ok: bool,
    next: String,
    detail: Value,
}

fn execute_machine_step(
    bundle_root: &Path,
    pack_path: Option<&Path>,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    if step.extra.get("auto_complete").and_then(Value::as_bool) == Some(true) {
        mark_machine_step_complete(machine, step, state);
        return MachineStepResult {
            ok: true,
            next: "step completed".to_string(),
            detail: serde_json::json!({
                "kind": step.kind,
                "auto_complete": true,
            }),
        };
    }

    match step.kind.as_str() {
        "platform_public_url" => {
            execute_platform_public_url_step(bundle_root, machine, step, state)
        }
        "cloudflare_tunnel" => execute_cloudflare_tunnel_step(bundle_root, machine, step, state),
        "oauth_device_code" => execute_oauth_device_code_step(bundle_root, machine, step, state),
        "oauth_authorization_code" => {
            execute_oauth_authorization_code_step(bundle_root, machine, step, state)
        }
        "http_probe" | "http_json" => execute_http_step(machine, step, state),
        "select_from_json" => execute_select_from_json_step(machine, step, state),
        "generate_file" => execute_generate_file_step(bundle_root, machine, step, state),
        "persist_runtime_config" => {
            execute_persist_runtime_config_step(bundle_root, pack_path, machine, step, state)
        }
        "provider_component_call" => {
            execute_provider_component_call_step(bundle_root, pack_path, machine, step, state)
        }
        "manual_action" | "download_file" | "qa_form" => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "manual_action_required",
                "step": step.id,
                "recoverable": true,
            }));
            MachineStepResult {
                ok: false,
                next: "operator action required".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "manual_action_required",
                    "kind": step.kind,
                    "title": step.title,
                }),
            }
        }
        _ => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "unsupported_step_kind",
                "step": step.id,
                "kind": step.kind,
                "recoverable": true,
            }));
            MachineStepResult {
                ok: false,
                next: "setup-machine step kind is not supported by this greentic-setup build"
                    .to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "unsupported_step_kind",
                    "kind": step.kind,
                }),
            }
        }
    }
}

fn apply_declared_failure_transition(
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
    result: &MachineStepResult,
) -> Option<String> {
    if result.ok {
        return None;
    }
    let error = result_error_code(state, result)?;
    let target = step
        .recover
        .iter()
        .chain(step.on_failure.iter())
        .find(|rule| failure_rule_matches(rule, &error))
        .and_then(|rule| rule_targets(rule).into_iter().next())?;

    if target == TERMINAL_FAILED {
        state.status = SetupMachineStatus::Failed;
        state.current_step = Some(step.id.clone());
        state.failed_step = Some(step.id.clone());
        state.last_error = Some(merge_failure_recovery_detail(
            state.last_error.clone(),
            &error,
            &target,
        ));
        return Some(target);
    }
    if target == TERMINAL_SUCCESS {
        state.status = SetupMachineStatus::Complete;
        state.current_step = None;
        state.failed_step = None;
        state.last_error = None;
        return Some(target);
    }
    if machine.steps.iter().any(|candidate| candidate.id == target) {
        state.status = SetupMachineStatus::Running;
        state.current_step = Some(target.clone());
        state.failed_step = None;
        state.last_error = Some(merge_failure_recovery_detail(
            state.last_error.clone(),
            &error,
            &target,
        ));
        return Some(target);
    }

    state.status = SetupMachineStatus::Failed;
    state.current_step = Some(step.id.clone());
    state.failed_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": "invalid_recovery_target",
        "step": step.id,
        "target": target,
        "recoverable": false,
    }));
    None
}

fn result_error_code(state: &SetupMachineState, result: &MachineStepResult) -> Option<String> {
    result
        .detail
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| {
            state
                .last_error
                .as_ref()
                .and_then(|error| error.get("error"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn failure_rule_matches(rule: &Value, error: &str) -> bool {
    let Some(obj) = rule.as_object() else {
        return false;
    };
    for key in ["when", "error", "code"] {
        match obj.get(key) {
            Some(Value::String(value)) if value == error || value == "*" => return true,
            Some(Value::Array(values))
                if values
                    .iter()
                    .any(|value| value.as_str() == Some(error) || value.as_str() == Some("*")) =>
            {
                return true;
            }
            _ => {}
        }
    }
    !obj.contains_key("when") && !obj.contains_key("error") && !obj.contains_key("code")
}

fn merge_failure_recovery_detail(base: Option<Value>, error: &str, recovery_step: &str) -> Value {
    let mut object = base
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    object.insert("error".to_string(), Value::String(error.to_string()));
    object.insert(
        "recovery_step".to_string(),
        Value::String(recovery_step.to_string()),
    );
    object.insert("recoverable".to_string(), Value::Bool(true));
    Value::Object(object)
}

fn execute_provider_component_call_step(
    bundle_root: &Path,
    pack_path: Option<&Path>,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let Some(pack_path) = pack_path else {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "missing_prerequisite",
            "step": step.id,
            "missing": "pack_path",
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "run setup-machine with provider pack provenance, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "missing_prerequisite",
                "missing_config_key": "pack_path",
            }),
        };
    };
    let Some(component_ref) = step
        .extra
        .get("component_ref")
        .or_else(|| step.extra.get("component"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return pause_provider_component_contract_error(
            step,
            state,
            "provider_component_call requires component_ref",
        );
    };
    let Some(op) = step
        .extra
        .get("op")
        .or_else(|| step.extra.get("operation"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return pause_provider_component_contract_error(
            step,
            state,
            "provider_component_call requires op",
        );
    };
    let request = step
        .extra
        .get("request")
        .or_else(|| step.extra.get("body"))
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "tenant": state.tenant,
                "team": state.team,
                "provider_id": state.provider_id,
                "outputs": state.outputs,
            })
        });
    let request = expand_machine_value_templates(request, state);
    let env = step
        .extra
        .get("env")
        .and_then(Value::as_str)
        .unwrap_or("dev");
    let config = crate::engine::SetupConfig {
        tenant: state.tenant.clone(),
        team: Some(state.team.clone()),
        env: env.to_string(),
        offline: false,
        verbose: false,
    };
    let result = match crate::engine::invoke_setup_component_operation(
        bundle_root,
        pack_path,
        component_ref,
        op,
        &request,
        &config,
    ) {
        Ok(result) => result,
        Err(err) => {
            return pause_provider_component_contract_error(step, state, &err.to_string());
        }
    };
    if result.get("ok").and_then(Value::as_bool) == Some(false) {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "provider_contract_error",
            "step": step.id,
            "detail": result,
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "provider setup component returned ok:false".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "provider_contract_error",
                "result": result,
            }),
        };
    }
    let output_key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or(step.id.as_str());
    ensure_outputs_object(state).insert(output_key.to_string(), result.clone());
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "provider component setup operation completed".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "component_ref": component_ref,
            "op": op,
            "output_key": output_key,
            "result": redact_large_json_detail(&result),
        }),
    }
}

fn pause_provider_component_contract_error(
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
    detail: &str,
) -> MachineStepResult {
    state.status = SetupMachineStatus::Paused;
    state.current_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": "provider_contract_error",
        "step": step.id,
        "detail": detail,
        "recoverable": true,
    }));
    MachineStepResult {
        ok: false,
        next: "fix provider component setup declaration, then retry".to_string(),
        detail: serde_json::json!({
            "blocked": true,
            "retryable": true,
            "error": "provider_contract_error",
            "detail": detail,
        }),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SetupMachineOAuthDeviceSession {
    step_id: String,
    provider_id: String,
    tenant: String,
    team: String,
    device_code: String,
    client_id: String,
    token_url: String,
    token_store_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token_store_key: Option<String>,
    interval: u64,
    expires_at: u64,
    created_at: u64,
}

fn execute_oauth_device_code_step(
    bundle_root: &Path,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    match load_setup_machine_oauth_device_session(bundle_root, state, &step.id) {
        Ok(Some(session)) => {
            execute_oauth_device_code_poll(bundle_root, machine, step, state, session)
        }
        Ok(None) => execute_oauth_device_code_start(bundle_root, step, state),
        Err(err) => pause_oauth_device_error(
            step,
            state,
            "provider_contract_error",
            "OAuth device-code session could not be loaded",
            serde_json::json!({"detail": err.to_string()}),
        ),
    }
}

fn execute_oauth_device_code_start(
    bundle_root: &Path,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let client_id = match oauth_step_value(step, state, "client_id", "client_id_output") {
        Ok(Some(value)) if !value.trim().is_empty() => value,
        Ok(_) => {
            return pause_oauth_device_error(
                step,
                state,
                "missing_prerequisite",
                "set OAuth client_id, then retry",
                serde_json::json!({"missing_config_key": "client_id"}),
            );
        }
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "invalid_answer",
                "fix OAuth client_id template, then retry",
                serde_json::json!({"detail": err.to_string()}),
            );
        }
    };
    let scopes = match oauth_scope(step) {
        Ok(Some(scopes)) if !scopes.trim().is_empty() => scopes,
        Ok(_) => {
            return pause_oauth_device_error(
                step,
                state,
                "missing_prerequisite",
                "declare OAuth device-code scopes, then retry",
                serde_json::json!({"missing_config_key": "scopes"}),
            );
        }
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "invalid_answer",
                "fix OAuth scopes, then retry",
                serde_json::json!({"detail": err.to_string()}),
            );
        }
    };
    let device_code_url = match oauth_device_endpoint(step, "device_code_url", "devicecode") {
        Ok(url) => url,
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "missing_prerequisite",
                "declare device_code_url or authority_url_template, then retry",
                serde_json::json!({"detail": err.to_string()}),
            );
        }
    };
    let token_url = match oauth_device_endpoint(step, "token_url", "token") {
        Ok(url) => url,
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "missing_prerequisite",
                "declare token_url or authority_url_template, then retry",
                serde_json::json!({"detail": err.to_string()}),
            );
        }
    };
    if let Err(err) =
        validate_http_step_url(&device_code_url).and_then(|_| validate_http_step_url(&token_url))
    {
        return pause_oauth_device_error(
            step,
            state,
            "invalid_answer",
            "fix OAuth device-code URLs, then retry",
            serde_json::json!({"detail": err.to_string()}),
        );
    }

    let mut response = match crate::http_client::api_agent()
        .post(&device_code_url)
        .send_form([
            ("client_id", client_id.as_str()),
            ("scope", scopes.as_str()),
        ]) {
        Ok(response) => response,
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "external_timeout",
                "OAuth device-code endpoint is not reachable",
                serde_json::json!({"detail": err.to_string(), "target": device_code_url}),
            );
        }
    };
    let body = match response.body_mut().read_json::<Value>() {
        Ok(body) => body,
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "provider_contract_error",
                "OAuth device-code response was not valid JSON",
                serde_json::json!({"detail": err.to_string()}),
            );
        }
    };
    let device_code = match body
        .get("device_code")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => {
            return pause_oauth_device_error(
                step,
                state,
                "provider_contract_error",
                "OAuth device-code response did not include device_code",
                serde_json::json!({"body": public_oauth_device_response(&body)}),
            );
        }
    };
    let user_code = body
        .get("user_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string();
    let verification_uri = body
        .get("verification_uri")
        .or_else(|| body.get("verification_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| step.extra.get("verification_uri").and_then(Value::as_str))
        .unwrap_or_default()
        .to_string();
    if user_code.is_empty() || verification_uri.is_empty() {
        return pause_oauth_device_error(
            step,
            state,
            "provider_contract_error",
            "OAuth device-code response did not include user_code or verification_uri",
            serde_json::json!({"body": public_oauth_device_response(&body)}),
        );
    }
    let now = crate::setup_actions::current_epoch_secs();
    let expires_in = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(900);
    let interval = body
        .get("interval")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .max(1);
    let token_store_key = step
        .extra
        .get("token_store_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("oauth_access_token")
        .to_string();
    let refresh_token_store_key = step
        .extra
        .get("refresh_token_store_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let session = SetupMachineOAuthDeviceSession {
        step_id: step.id.clone(),
        provider_id: state.provider_id.clone(),
        tenant: state.tenant.clone(),
        team: state.team.clone(),
        device_code,
        client_id,
        token_url,
        token_store_key,
        refresh_token_store_key,
        interval,
        expires_at: now + expires_in,
        created_at: now,
    };
    if let Err(err) = write_setup_machine_oauth_device_session(bundle_root, state, &session) {
        return pause_oauth_device_error(
            step,
            state,
            "provider_contract_error",
            "OAuth device-code session could not be saved",
            serde_json::json!({"detail": err.to_string()}),
        );
    }

    let output_key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_device_login");
    ensure_outputs_object(state).insert(
        output_key.to_string(),
        serde_json::json!({
            "verification_uri": verification_uri,
            "verification_uri_complete": body.get("verification_uri_complete").cloned().unwrap_or(Value::Null),
            "user_code": user_code,
            "expires_at": session.expires_at,
            "interval": session.interval,
        }),
    );
    state.status = SetupMachineStatus::Paused;
    state.current_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": "manual_action_required",
        "reason": "oauth_device_code_pending",
        "step": step.id,
        "recoverable": true,
    }));
    MachineStepResult {
        ok: false,
        next: "open device-code URL, enter the code, then run setup-next again".to_string(),
        detail: serde_json::json!({
            "blocked": true,
            "retryable": true,
            "error": "manual_action_required",
            "reason": "oauth_device_code_pending",
            "login": state.outputs.get(output_key).cloned().unwrap_or(Value::Null),
            "output_key": output_key,
        }),
    }
}

fn execute_oauth_device_code_poll(
    bundle_root: &Path,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
    session: SetupMachineOAuthDeviceSession,
) -> MachineStepResult {
    if crate::setup_actions::current_epoch_secs() >= session.expires_at {
        let _ = remove_setup_machine_oauth_device_session(bundle_root, state, &step.id);
        return pause_oauth_device_error(
            step,
            state,
            "oauth_token_expired",
            "OAuth device code expired; retry to start a new login",
            serde_json::json!({"expired": true}),
        );
    }
    let agent = crate::http_client::api_agent_any_status();
    let mut response = match agent.post(&session.token_url).send_form([
        ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ("client_id", session.client_id.as_str()),
        ("device_code", session.device_code.as_str()),
    ]) {
        Ok(response) => response,
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "external_timeout",
                "OAuth token endpoint is not reachable",
                serde_json::json!({"detail": err.to_string(), "target": session.token_url}),
            );
        }
    };
    let status = response.status().as_u16();
    let body = match response.body_mut().read_json::<Value>() {
        Ok(body) => body,
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "provider_contract_error",
                "OAuth token response was not valid JSON",
                serde_json::json!({"detail": err.to_string(), "status": status}),
            );
        }
    };
    if let Some(error) = body.get("error").and_then(Value::as_str)
        && matches!(error, "authorization_pending" | "slow_down")
    {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "oauth_pending",
            "step": step.id,
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "OAuth authorization is still pending".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "oauth_pending",
                "interval": session.interval,
            }),
        };
    }
    if status >= 400 || body.get("access_token").and_then(Value::as_str).is_none() {
        return pause_oauth_device_error(
            step,
            state,
            "oauth_refresh_failed",
            "OAuth token polling failed",
            serde_json::json!({"status": status, "body": public_oauth_device_response(&body)}),
        );
    }
    let mut config = serde_json::Map::new();
    if let Some(access_token) = body.get("access_token").and_then(Value::as_str) {
        config.insert(
            session.token_store_key.clone(),
            Value::String(access_token.to_string()),
        );
    }
    if let Some(refresh_key) = session.refresh_token_store_key.as_deref()
        && let Some(refresh_token) = body.get("refresh_token").and_then(Value::as_str)
    {
        config.insert(
            refresh_key.to_string(),
            Value::String(refresh_token.to_string()),
        );
    }
    if config.is_empty() {
        return pause_oauth_device_error(
            step,
            state,
            "provider_contract_error",
            "OAuth token response had no persistable tokens",
            serde_json::json!({"status": status}),
        );
    }
    let config = Value::Object(config);
    let env = step
        .extra
        .get("env")
        .and_then(Value::as_str)
        .unwrap_or("dev");
    let persist_result = tokio::runtime::Runtime::new()
        .map_err(|err| err.to_string())
        .and_then(|runtime| {
            runtime
                .block_on(crate::qa::persist::persist_all_config_as_secrets(
                    bundle_root,
                    env,
                    &state.tenant,
                    Some(&state.team),
                    &state.provider_id,
                    &config,
                    None,
                ))
                .map_err(|err| err.to_string())
        });
    let persisted_keys = match persist_result {
        Ok(keys) => keys,
        Err(err) => {
            return pause_oauth_device_error(
                step,
                state,
                "provider_contract_error",
                "OAuth token secrets could not be persisted",
                serde_json::json!({"detail": err}),
            );
        }
    };
    let _ = remove_setup_machine_oauth_device_session(bundle_root, state, &step.id);
    ensure_outputs_object(state).insert(
        step.extra
            .get("output_key")
            .and_then(Value::as_str)
            .unwrap_or("oauth_device_login")
            .to_string(),
        serde_json::json!({
            "ok": true,
            "token_store_key": session.token_store_key,
            "persisted_keys": persisted_keys,
        }),
    );
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "OAuth device-code step completed".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "persisted_keys": persisted_keys,
        }),
    }
}

fn pause_oauth_device_error(
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
    error: &str,
    next: &str,
    detail: Value,
) -> MachineStepResult {
    state.status = SetupMachineStatus::Paused;
    state.current_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": error,
        "step": step.id,
        "detail": detail,
        "recoverable": true,
    }));
    let mut detail_obj = serde_json::json!({
        "blocked": true,
        "retryable": true,
        "error": error,
    });
    if let (Some(obj), Some(extra)) = (detail_obj.as_object_mut(), detail.as_object()) {
        for (key, value) in extra {
            obj.insert(key.clone(), value.clone());
        }
    }
    MachineStepResult {
        ok: false,
        next: next.to_string(),
        detail: detail_obj,
    }
}

fn oauth_device_endpoint(
    step: &SetupMachineStep,
    direct_key: &str,
    authority_suffix: &str,
) -> anyhow::Result<String> {
    if let Some(value) = step
        .extra
        .get(direct_key)
        .or_else(|| step.extra.get(&format!("{direct_key}_template")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(value.to_string());
    }
    let authority = step
        .extra
        .get("authority_url_template")
        .or_else(|| step.extra.get("authority_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing {direct_key}"))?;
    Ok(format!(
        "{}/oauth2/v2.0/{}",
        authority.trim_end_matches('/'),
        authority_suffix
    ))
}

fn public_oauth_device_response(body: &Value) -> Value {
    let mut public = serde_json::Map::new();
    for key in [
        "user_code",
        "verification_uri",
        "verification_url",
        "verification_uri_complete",
        "expires_in",
        "interval",
        "message",
        "error",
        "error_description",
    ] {
        if let Some(value) = body.get(key) {
            public.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(public)
}

fn setup_machine_oauth_device_session_path(
    bundle_root: &Path,
    state: &SetupMachineState,
    step_id: &str,
) -> PathBuf {
    setup_machine_state_dir(bundle_root, &state.tenant, &state.team, &state.provider_id)
        .join("oauth-device-sessions")
        .join(format!("{step_id}.json"))
}

fn load_setup_machine_oauth_device_session(
    bundle_root: &Path,
    state: &SetupMachineState,
    step_id: &str,
) -> anyhow::Result<Option<SetupMachineOAuthDeviceSession>> {
    let path = setup_machine_oauth_device_session_path(bundle_root, state, step_id);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

fn write_setup_machine_oauth_device_session(
    bundle_root: &Path,
    state: &SetupMachineState,
    session: &SetupMachineOAuthDeviceSession,
) -> anyhow::Result<PathBuf> {
    let path = setup_machine_oauth_device_session_path(bundle_root, state, &session.step_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(session)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(&path, permissions)?;
    }
    Ok(path)
}

fn remove_setup_machine_oauth_device_session(
    bundle_root: &Path,
    state: &SetupMachineState,
    step_id: &str,
) -> anyhow::Result<()> {
    let path = setup_machine_oauth_device_session_path(bundle_root, state, step_id);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn execute_oauth_authorization_code_step(
    bundle_root: &Path,
    _machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let authorization_url = match build_oauth_authorization_url(bundle_root, step, state) {
        Ok(url) => url,
        Err(OauthAuthorizationError::Missing(key)) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "missing_prerequisite",
                "step": step.id,
                "missing": key,
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "provide OAuth authorization prerequisites, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "missing_prerequisite",
                    "missing_config_key": key,
                }),
            };
        }
        Err(OauthAuthorizationError::Invalid(detail)) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "invalid_answer",
                "step": step.id,
                "detail": detail,
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "fix OAuth authorization URL inputs, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "invalid_answer",
                    "detail": detail,
                }),
            };
        }
    };

    let output_key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or("authorization_url");
    ensure_outputs_object(state).insert(
        output_key.to_string(),
        Value::String(authorization_url.clone()),
    );
    state.status = SetupMachineStatus::Paused;
    state.current_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": "manual_action_required",
        "reason": "oauth_authorization_required",
        "step": step.id,
        "recoverable": true,
    }));
    MachineStepResult {
        ok: false,
        next: "open authorization URL and complete OAuth callback".to_string(),
        detail: serde_json::json!({
            "blocked": true,
            "retryable": true,
            "error": "manual_action_required",
            "reason": "oauth_authorization_required",
            "authorization_url": authorization_url,
            "output_key": output_key,
            "kind": step.kind,
        }),
    }
}

#[derive(Debug)]
enum OauthAuthorizationError {
    Missing(&'static str),
    Invalid(String),
}

impl std::fmt::Display for OauthAuthorizationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(key) => write!(f, "missing {key}"),
            Self::Invalid(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for OauthAuthorizationError {}

fn build_oauth_authorization_url(
    bundle_root: &Path,
    step: &SetupMachineStep,
    state: &SetupMachineState,
) -> Result<String, OauthAuthorizationError> {
    let template = step
        .extra
        .get("authorize_url")
        .or_else(|| step.extra.get("authorization_url"))
        .or_else(|| step.extra.get("auth_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(OauthAuthorizationError::Missing("authorize_url"))?;
    let expanded = expand_machine_template(template, state)
        .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()))?;
    let mut parsed = url::Url::parse(&expanded)
        .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()))?;
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        return Err(OauthAuthorizationError::Invalid(
            "authorize_url must be an https URL with a host".to_string(),
        ));
    }

    let key = crate::setup_actions::load_or_create_signing_key(bundle_root)
        .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()))?;
    let ttl = step
        .extra
        .get("state_ttl_secs")
        .and_then(Value::as_u64)
        .unwrap_or(15 * 60);
    let payload = crate::setup_actions::OAuthStatePayload {
        provider_id: state.provider_id.clone(),
        tenant: state.tenant.clone(),
        team: state.team.clone(),
        action_id: step.id.clone(),
        nonce: oauth_nonce(),
        expires_at: crate::setup_actions::current_epoch_secs() + ttl.max(1),
    };
    let signed_state = crate::setup_actions::sign_oauth_state(&payload, &key)
        .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()))?;

    append_oauth_query_param(&mut parsed, "response_type", oauth_response_type(step));
    if let Some(client_id) = oauth_step_value(step, state, "client_id", "client_id_output")? {
        append_oauth_query_param(&mut parsed, "client_id", &client_id);
    }
    if let Some(redirect_uri) = oauth_redirect_uri(bundle_root, step, state)? {
        append_oauth_query_param(&mut parsed, "redirect_uri", &redirect_uri);
    }
    if let Some(scope) = oauth_scope(step)? {
        append_oauth_query_param(&mut parsed, "scope", &scope);
    }
    append_oauth_query_param(&mut parsed, "state", &signed_state);
    if let Some(extra_params) = step.extra.get("params").and_then(Value::as_object) {
        let expanded = expand_machine_value_templates(Value::Object(extra_params.clone()), state);
        if let Some(params) = expanded.as_object() {
            for (key, value) in params {
                if let Some(value) = value.as_str() {
                    append_oauth_query_param(&mut parsed, key, value);
                }
            }
        }
    }
    Ok(parsed.to_string())
}

fn exchange_setup_machine_oauth_authorization_code(
    bundle_root: &Path,
    machine: &SetupMachine,
    oauth_state: &crate::setup_actions::OAuthStatePayload,
    code: &str,
) -> anyhow::Result<Value> {
    let state = load_or_init_setup_machine_state(
        bundle_root,
        &oauth_state.provider_id,
        &oauth_state.tenant,
        &oauth_state.team,
        machine,
    )?;
    let step = machine
        .steps
        .iter()
        .find(|candidate| candidate.id == oauth_state.action_id)
        .ok_or_else(|| {
            anyhow::anyhow!("setup-machine step not found: {}", oauth_state.action_id)
        })?;
    if step.kind != "oauth_authorization_code" {
        anyhow::bail!(
            "setup-machine step is not an oauth_authorization_code step: {}",
            step.id
        );
    }
    let token_url = oauth_authorization_token_url(step, &state)?;
    let redirect_uri = oauth_redirect_uri(bundle_root, step, &state)?
        .ok_or(OauthAuthorizationError::Missing("redirect_uri"))?;
    let client_id = oauth_step_value(step, &state, "client_id", "client_id_output")?
        .ok_or(OauthAuthorizationError::Missing("client_id"))?;
    let client_secret = oauth_step_value(step, &state, "client_secret", "client_secret_output")?;

    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), redirect_uri),
        ("client_id".to_string(), client_id),
    ];
    if let Some(client_secret) = client_secret {
        form.push(("client_secret".to_string(), client_secret));
    }
    let agent = crate::http_client::api_agent_any_status();
    let mut response = agent
        .post(&token_url)
        .send_form(form)
        .context("OAuth token exchange failed")?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json::<Value>()
        .context("failed to parse OAuth token response")?;
    if status >= 400 {
        anyhow::bail!("OAuth token exchange failed with status {status}: {body}");
    }
    Ok(body)
}

fn oauth_nonce() -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>())
}

fn oauth_response_type(step: &SetupMachineStep) -> &str {
    step.extra
        .get("response_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("code")
}

fn oauth_step_value(
    step: &SetupMachineStep,
    state: &SetupMachineState,
    literal_key: &'static str,
    output_key_key: &'static str,
) -> Result<Option<String>, OauthAuthorizationError> {
    if let Some(value) = step
        .extra
        .get(literal_key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return expand_machine_template(value, state)
            .map(Some)
            .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()));
    }
    let output_key = step
        .extra
        .get(output_key_key)
        .and_then(Value::as_str)
        .unwrap_or(literal_key);
    Ok(output_string(&state.outputs, output_key))
}

fn oauth_redirect_uri(
    bundle_root: &Path,
    step: &SetupMachineStep,
    state: &SetupMachineState,
) -> Result<Option<String>, OauthAuthorizationError> {
    if let Some(value) = step
        .extra
        .get("redirect_uri")
        .or_else(|| step.extra.get("redirect_uri_template"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return expand_machine_template(value, state)
            .map(Some)
            .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()));
    }
    let Some(callback_path) = step
        .extra
        .get("callback_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let public_base_url = output_string(&state.outputs, "public_base_url")
        .or_else(|| {
            crate::platform_setup::load_effective_static_routes_defaults(
                bundle_root,
                &state.tenant,
                Some(&state.team),
            )
            .ok()
            .flatten()
            .and_then(|policy| policy.public_base_url)
        })
        .ok_or(OauthAuthorizationError::Missing("public_base_url"))?;
    Ok(Some(format!(
        "{}{}",
        public_base_url.trim_end_matches('/'),
        ensure_leading_slash(callback_path)
    )))
}

fn oauth_authorization_token_url(
    step: &SetupMachineStep,
    state: &SetupMachineState,
) -> Result<String, OauthAuthorizationError> {
    let template = step
        .extra
        .get("token_url")
        .or_else(|| step.extra.get("token_url_template"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(OauthAuthorizationError::Missing("token_url"))?;
    let token_url = expand_machine_template(template, state)
        .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()))?;
    let parsed = url::Url::parse(&token_url)
        .map_err(|err| OauthAuthorizationError::Invalid(err.to_string()))?;
    if !(parsed.scheme() == "https" && parsed.host_str().is_some()
        || parsed.scheme() == "http"
            && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1")))
    {
        return Err(OauthAuthorizationError::Invalid(
            "token_url must be https or loopback http".to_string(),
        ));
    }
    Ok(token_url)
}

fn oauth_scope(step: &SetupMachineStep) -> Result<Option<String>, OauthAuthorizationError> {
    if let Some(scope) = step.extra.get("scope").and_then(Value::as_str) {
        let scope = scope.trim();
        return Ok((!scope.is_empty()).then(|| scope.to_string()));
    }
    if let Some(scopes) = step.extra.get("scopes").and_then(Value::as_array) {
        let scope = scopes
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        return Ok((!scope.is_empty()).then_some(scope));
    }
    Ok(None)
}

fn append_oauth_query_param(parsed: &mut url::Url, key: &str, value: &str) {
    if value.trim().is_empty() || parsed.query_pairs().any(|(existing, _)| existing == key) {
        return;
    }
    parsed.query_pairs_mut().append_pair(key, value);
}

fn map_oauth_authorization_token_response(
    step: &SetupMachineStep,
    token_response: &Value,
) -> anyhow::Result<serde_json::Map<String, Value>> {
    let mut config = serde_json::Map::new();
    if let Some(map) = step
        .extra
        .get("response_secret_map")
        .and_then(Value::as_object)
    {
        for (response_key, secret_key) in map {
            let Some(secret_key) = secret_key
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                anyhow::bail!(
                    "setup-machine OAuth response_secret_map value must be a non-empty string: {response_key}"
                );
            };
            if let Some(value) = token_response.get(response_key).and_then(Value::as_str)
                && !value.trim().is_empty()
            {
                config.insert(secret_key.to_string(), Value::String(value.to_string()));
            }
        }
    } else {
        insert_oauth_token_field(
            &mut config,
            token_response,
            "access_token",
            step.extra
                .get("token_store_key")
                .and_then(Value::as_str)
                .unwrap_or("oauth_access_token"),
        );
        if let Some(secret_key) = step
            .extra
            .get("refresh_token_store_key")
            .and_then(Value::as_str)
        {
            insert_oauth_token_field(&mut config, token_response, "refresh_token", secret_key);
        }
        if let Some(secret_key) = step.extra.get("id_token_store_key").and_then(Value::as_str) {
            insert_oauth_token_field(&mut config, token_response, "id_token", secret_key);
        }
    }

    if config.is_empty() {
        anyhow::bail!("OAuth token response had no persistable tokens");
    }
    Ok(config)
}

fn insert_oauth_token_field(
    config: &mut serde_json::Map<String, Value>,
    token_response: &Value,
    response_key: &str,
    secret_key: &str,
) {
    let secret_key = secret_key.trim();
    if secret_key.is_empty() {
        return;
    }
    if let Some(value) = token_response.get(response_key).and_then(Value::as_str)
        && !value.trim().is_empty()
    {
        config.insert(secret_key.to_string(), Value::String(value.to_string()));
    }
}

fn ensure_leading_slash(value: &str) -> String {
    if value.starts_with('/') {
        value.to_string()
    } else {
        format!("/{value}")
    }
}

fn execute_select_from_json_step(
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let Some(source_output) = step
        .extra
        .get("source_output")
        .or_else(|| step.extra.get("source"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return pause_select_from_json(
            step,
            state,
            "missing_prerequisite",
            "source_output",
            "declare source_output, then retry",
        );
    };
    let Some(source) = state.outputs.get(source_output) else {
        return pause_select_from_json(
            step,
            state,
            "missing_prerequisite",
            source_output,
            "run prerequisite JSON step, then retry",
        );
    };
    let selected_scope = match select_json_scope(source, step) {
        Some(value) => value,
        None => {
            return pause_select_from_json(
                step,
                state,
                "missing_prerequisite",
                "json_path",
                "fix source JSON path, then retry",
            );
        }
    };

    let selected = match choose_json_selection(selected_scope, step, state) {
        Ok(selected) => selected,
        Err(SelectFromJsonError::SelectionRequired(count)) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "manual_action_required",
                "reason": "selection_required",
                "step": step.id,
                "options": count,
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "operator selection required".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "manual_action_required",
                    "reason": "selection_required",
                    "options": count,
                    "source_output": source_output,
                }),
            };
        }
        Err(SelectFromJsonError::InvalidSelection(detail)) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "invalid_answer",
                "step": step.id,
                "detail": detail,
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "fix selected JSON value, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "invalid_answer",
                    "detail": detail,
                }),
            };
        }
    };

    let output_key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or(step.id.as_str());
    let value_key = step.extra.get("value_key").and_then(Value::as_str);
    let stored = value_key
        .and_then(|key| selected.get(key))
        .cloned()
        .unwrap_or_else(|| selected.clone());
    let outputs = ensure_outputs_object(state);
    outputs.insert(output_key.to_string(), stored.clone());
    if selected.is_object() {
        outputs.insert(format!("{output_key}_item"), selected.clone());
    }
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "JSON selection completed".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "source_output": source_output,
            "output_key": output_key,
            "selected": redact_large_json_detail(&stored),
        }),
    }
}

#[derive(Debug)]
enum SelectFromJsonError {
    SelectionRequired(usize),
    InvalidSelection(String),
}

fn pause_select_from_json(
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
    error: &str,
    missing: &str,
    next: &str,
) -> MachineStepResult {
    state.status = SetupMachineStatus::Paused;
    state.current_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": error,
        "step": step.id,
        "missing": missing,
        "recoverable": true,
    }));
    MachineStepResult {
        ok: false,
        next: next.to_string(),
        detail: serde_json::json!({
            "blocked": true,
            "retryable": true,
            "error": error,
            "missing_config_key": missing,
        }),
    }
}

fn select_json_scope<'a>(source: &'a Value, step: &SetupMachineStep) -> Option<&'a Value> {
    let Some(path) = step
        .extra
        .get("json_pointer")
        .or_else(|| step.extra.get("path"))
        .or_else(|| step.extra.get("json_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Some(source);
    };
    if path.starts_with('/') {
        source.pointer(path)
    } else {
        let mut current = source;
        for segment in path.split('.').filter(|segment| !segment.is_empty()) {
            current = current.get(segment)?;
        }
        Some(current)
    }
}

fn choose_json_selection(
    value: &Value,
    step: &SetupMachineStep,
    state: &SetupMachineState,
) -> Result<Value, SelectFromJsonError> {
    let Some(items) = value.as_array() else {
        return Ok(value.clone());
    };
    if items.is_empty() {
        return Err(SelectFromJsonError::InvalidSelection(
            "source JSON selection array is empty".to_string(),
        ));
    }
    if let Some(index) = step.extra.get("selected_index").and_then(Value::as_u64) {
        return items.get(index as usize).cloned().ok_or_else(|| {
            SelectFromJsonError::InvalidSelection(format!("selected_index {index} is out of range"))
        });
    }
    if let Some(value) = step
        .extra
        .get("selected_value")
        .or_else(|| step.extra.get("default_value"))
    {
        return find_json_selection_by_value(items, step, value).ok_or_else(|| {
            SelectFromJsonError::InvalidSelection(
                "selected_value did not match any option".to_string(),
            )
        });
    }
    let choice_key = step
        .extra
        .get("choice_output_key")
        .and_then(Value::as_str)
        .unwrap_or(step.id.as_str());
    if let Some(value) = state.outputs.get(choice_key) {
        return find_json_selection_by_value(items, step, value).ok_or_else(|| {
            SelectFromJsonError::InvalidSelection(format!(
                "choice output `{choice_key}` did not match any option"
            ))
        });
    }
    if items.len() == 1 {
        return Ok(items[0].clone());
    }
    Err(SelectFromJsonError::SelectionRequired(items.len()))
}

fn find_json_selection_by_value(
    items: &[Value],
    step: &SetupMachineStep,
    selected_value: &Value,
) -> Option<Value> {
    let value_key = step.extra.get("value_key").and_then(Value::as_str);
    items.iter().find_map(|item| {
        let candidate = value_key.and_then(|key| item.get(key)).unwrap_or(item);
        values_match_for_selection(candidate, selected_value).then(|| item.clone())
    })
}

fn values_match_for_selection(candidate: &Value, selected_value: &Value) -> bool {
    if candidate == selected_value {
        return true;
    }
    match (candidate, selected_value) {
        (Value::String(left), Value::String(right)) => left == right,
        (Value::String(left), other) => other.as_str() == Some(left.as_str()),
        (other, Value::String(right)) => other.as_str() == Some(right.as_str()),
        _ => false,
    }
}

fn redact_large_json_detail(value: &Value) -> Value {
    match value {
        Value::Array(items) if items.len() > 10 => serde_json::json!({
            "kind": "array",
            "len": items.len(),
        }),
        Value::Object(map) if map.len() > 20 => serde_json::json!({
            "kind": "object",
            "keys": map.len(),
        }),
        other => other.clone(),
    }
}

fn execute_generate_file_step(
    bundle_root: &Path,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let Some(artifact_path) = setup_artifact_relative_path(step) else {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "missing_prerequisite",
            "step": step.id,
            "missing": "artifact_path",
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "declare artifact_path, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "missing_prerequisite",
                "missing_config_key": "artifact_path",
            }),
        };
    };
    if !is_safe_setup_artifact_path(artifact_path) {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "provider_contract_error",
            "step": step.id,
            "artifact_path": artifact_path,
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "fix setup artifact path, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "provider_contract_error",
                "artifact_path": artifact_path,
            }),
        };
    }

    let rendered = match render_generate_file_content(step, state) {
        Ok(rendered) => rendered,
        Err(err) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "missing_prerequisite",
                "step": step.id,
                "detail": err.to_string(),
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "provide file content inputs, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "missing_prerequisite",
                    "detail": err.to_string(),
                }),
            };
        }
    };

    let artifact_file = resolve_setup_artifact_path(bundle_root, state, artifact_path);
    if let Some(parent) = artifact_file.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return pause_generate_file_write_error(step, state, artifact_path, err);
    }
    if let Err(err) = std::fs::write(&artifact_file, rendered.as_bytes()) {
        return pause_generate_file_write_error(step, state, artifact_path, err);
    }

    let output_key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or(step.id.as_str());
    ensure_outputs_object(state).insert(
        output_key.to_string(),
        Value::String(artifact_path.to_string()),
    );
    state.created_resources.push(serde_json::json!({
        "kind": "setup_artifact",
        "step": step.id,
        "path": artifact_path,
    }));
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "setup artifact generated".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "artifact_path": artifact_path,
            "bytes": rendered.len(),
            "output_key": output_key,
        }),
    }
}

fn execute_persist_runtime_config_step(
    bundle_root: &Path,
    pack_path: Option<&Path>,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let config = match render_runtime_config(machine, step, state) {
        Ok(config) => config,
        Err(err) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "missing_prerequisite",
                "step": step.id,
                "detail": err.to_string(),
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "provide runtime config inputs, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "missing_prerequisite",
                    "detail": err.to_string(),
                }),
            };
        }
    };

    let config_dir = bundle_root
        .join("state")
        .join("config")
        .join(&state.provider_id);
    let config_path = config_dir.join("setup-answers.json");
    if let Some(parent) = config_path.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        return pause_runtime_config_write_error(step, state, "setup_answers", err);
    }
    let bytes = match serde_json::to_vec_pretty(&config) {
        Ok(bytes) => bytes,
        Err(err) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "provider_contract_error",
                "step": step.id,
                "detail": err.to_string(),
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "runtime config could not be serialized".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "provider_contract_error",
                    "detail": err.to_string(),
                }),
            };
        }
    };
    if let Err(err) = std::fs::write(&config_path, bytes) {
        return pause_runtime_config_write_error(step, state, "setup_answers", err);
    }

    let mut envelope_path = Value::Null;
    let mut envelope_skipped = false;
    if let Some(pack_path) = pack_path {
        match crate::config_envelope::write_provider_config_envelope(
            &bundle_root.join(".providers"),
            &state.provider_id,
            &step.id,
            &config,
            pack_path,
            true,
        ) {
            Ok(path) => envelope_path = Value::String(path.display().to_string()),
            Err(err) => {
                state.status = SetupMachineStatus::Paused;
                state.current_step = Some(step.id.clone());
                state.last_error = Some(serde_json::json!({
                    "error": "provider_contract_error",
                    "step": step.id,
                    "detail": err.to_string(),
                    "recoverable": true,
                }));
                return MachineStepResult {
                    ok: false,
                    next: "provider config envelope could not be written".to_string(),
                    detail: serde_json::json!({
                        "blocked": true,
                        "retryable": true,
                        "error": "provider_contract_error",
                        "detail": err.to_string(),
                    }),
                };
            }
        }
    } else {
        envelope_skipped = true;
    }

    state.created_resources.push(serde_json::json!({
        "kind": "runtime_config",
        "step": step.id,
        "path": config_path.display().to_string(),
        "envelope_path": envelope_path,
    }));
    let output_key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or("runtime_config_path");
    ensure_outputs_object(state).insert(
        output_key.to_string(),
        Value::String(config_path.display().to_string()),
    );
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "runtime config persisted".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "config_path": config_path,
            "envelope_path": envelope_path,
            "envelope_skipped": envelope_skipped,
        }),
    }
}

fn render_runtime_config(
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &SetupMachineState,
) -> anyhow::Result<Value> {
    let mut config = if let Some(config) = step.extra.get("config") {
        expand_machine_value_templates(config.clone(), state)
    } else if let Some(mappings) = step.extra.get("mappings") {
        expand_machine_value_templates(mappings.clone(), state)
    } else {
        anyhow::bail!("persist_runtime_config requires config or mappings")
    };
    let server_owned = setup_machine_server_owned_keys(machine, step);
    if let Some(config) = config.as_object_mut() {
        for key in server_owned {
            config.remove(&key);
        }
    } else {
        anyhow::bail!("persist_runtime_config config must render to an object");
    }
    Ok(config)
}

fn setup_machine_server_owned_keys(
    machine: &SetupMachine,
    step: &SetupMachineStep,
) -> BTreeSet<String> {
    machine
        .extra
        .get("server_owned_config_keys")
        .into_iter()
        .chain(step.extra.get("server_owned_config_keys"))
        .flat_map(|value| value.as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn pause_runtime_config_write_error(
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
    target: &str,
    err: std::io::Error,
) -> MachineStepResult {
    state.status = SetupMachineStatus::Paused;
    state.current_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": "provider_contract_error",
        "step": step.id,
        "target": target,
        "detail": err.to_string(),
        "recoverable": true,
    }));
    MachineStepResult {
        ok: false,
        next: "runtime config could not be written".to_string(),
        detail: serde_json::json!({
            "blocked": true,
            "retryable": true,
            "error": "provider_contract_error",
            "target": target,
            "detail": err.to_string(),
        }),
    }
}

fn pause_generate_file_write_error(
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
    artifact_path: &str,
    err: std::io::Error,
) -> MachineStepResult {
    state.status = SetupMachineStatus::Paused;
    state.current_step = Some(step.id.clone());
    state.last_error = Some(serde_json::json!({
        "error": "artifact_write_failed",
        "step": step.id,
        "artifact_path": artifact_path,
        "detail": err.to_string(),
        "recoverable": true,
    }));
    MachineStepResult {
        ok: false,
        next: "setup artifact could not be written".to_string(),
        detail: serde_json::json!({
            "blocked": true,
            "retryable": true,
            "error": "artifact_write_failed",
            "artifact_path": artifact_path,
            "detail": err.to_string(),
        }),
    }
}

fn setup_artifact_relative_path(step: &SetupMachineStep) -> Option<&str> {
    step.extra
        .get("artifact_path")
        .or_else(|| step.extra.get("path"))
        .or_else(|| step.extra.get("output_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn is_safe_setup_artifact_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains("..")
        && (path.starts_with("artifacts/") || path.starts_with("state/setup/"))
}

fn resolve_setup_artifact_path(
    bundle_root: &Path,
    state: &SetupMachineState,
    artifact_path: &str,
) -> PathBuf {
    if artifact_path.starts_with("state/setup/") {
        bundle_root.join(artifact_path)
    } else {
        setup_machine_state_dir(bundle_root, &state.tenant, &state.team, &state.provider_id)
            .join(artifact_path)
    }
}

fn render_generate_file_content(
    step: &SetupMachineStep,
    state: &SetupMachineState,
) -> anyhow::Result<String> {
    if let Some(template) = step.extra.get("template").and_then(Value::as_str) {
        return expand_machine_template(template, state);
    }
    if let Some(content) = step.extra.get("content") {
        return render_generated_value(content.clone(), state);
    }
    if let Some(template_json) = step.extra.get("template_json") {
        return render_generated_value(template_json.clone(), state);
    }
    anyhow::bail!("generate_file requires template, content, or template_json")
}

fn render_generated_value(value: Value, state: &SetupMachineState) -> anyhow::Result<String> {
    match value {
        Value::String(text) => expand_machine_template(&text, state),
        other => Ok(serde_json::to_string_pretty(
            &expand_machine_value_templates(other, state),
        )?),
    }
}

fn execute_http_step(
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let Some(template) = step
        .extra
        .get("url_template")
        .or_else(|| step.extra.get("url"))
        .and_then(Value::as_str)
    else {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "missing_prerequisite",
            "step": step.id,
            "missing": "url_template",
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "declare url_template, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "missing_prerequisite",
                "missing_config_key": "url_template",
            }),
        };
    };
    let target = match expand_machine_template(template, state) {
        Ok(target) => target,
        Err(err) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "missing_prerequisite",
                "step": step.id,
                "detail": err.to_string(),
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "provide missing template values, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "missing_prerequisite",
                    "detail": err.to_string(),
                }),
            };
        }
    };
    if let Err(err) = validate_http_step_url(&target) {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "invalid_answer",
            "step": step.id,
            "detail": err.to_string(),
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "fix setup HTTP URL, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "invalid_answer",
                "detail": err.to_string(),
            }),
        };
    }

    let method = step
        .extra
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let expected_status = step
        .extra
        .get("expected_status")
        .or_else(|| step.extra.get("status"))
        .and_then(Value::as_u64)
        .unwrap_or(200) as u16;
    let response = match method.as_str() {
        "GET" => crate::http_client::api_agent().get(&target).call(),
        "POST" => {
            let payload = step
                .extra
                .get("body")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let payload = expand_machine_value_templates(payload, state);
            crate::http_client::api_agent()
                .post(&target)
                .send_json(payload)
        }
        _ => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "unsupported_http_method",
                "step": step.id,
                "method": method,
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "fix setup HTTP method, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "unsupported_http_method",
                    "method": method,
                }),
            };
        }
    };
    let mut response = match response {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(status)) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "provider_contract_error",
                "step": step.id,
                "status": status,
                "expected_status": expected_status,
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "setup HTTP endpoint returned an unexpected status".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "provider_contract_error",
                    "status": status,
                    "expected_status": expected_status,
                    "target": target,
                }),
            };
        }
        Err(err) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "external_timeout",
                "step": step.id,
                "detail": err.to_string(),
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "setup HTTP endpoint is not reachable".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "external_timeout",
                    "detail": err.to_string(),
                    "target": target,
                }),
            };
        }
    };
    let status = response.status().as_u16();
    if status != expected_status {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "provider_contract_error",
            "step": step.id,
            "status": status,
            "expected_status": expected_status,
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "setup HTTP endpoint returned an unexpected status".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "provider_contract_error",
                "status": status,
                "expected_status": expected_status,
                "target": target,
            }),
        };
    }

    let body = response
        .body_mut()
        .read_json::<Value>()
        .unwrap_or(Value::Null);
    if step.kind == "http_json" {
        let output_key = step
            .extra
            .get("output_key")
            .and_then(Value::as_str)
            .unwrap_or(step.id.as_str());
        ensure_outputs_object(state).insert(output_key.to_string(), body.clone());
    }
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "HTTP setup step completed".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "method": method,
            "status": status,
            "target": target,
            "body": if step.kind == "http_json" { body } else { Value::Null },
        }),
    }
}

fn execute_platform_public_url_step(
    bundle_root: &Path,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or("public_base_url");
    let candidate = output_string(&state.outputs, key)
        .or_else(|| {
            step.extra
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            step.extra
                .get("default")
                .or_else(|| step.extra.get("default_url"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            crate::platform_setup::load_effective_static_routes_defaults(
                bundle_root,
                &state.tenant,
                Some(&state.team),
            )
            .ok()
            .flatten()
            .and_then(|policy| policy.public_base_url)
        })
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.trim().is_empty());

    let Some(public_base_url) = candidate else {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "missing_prerequisite",
            "step": step.id,
            "missing": key,
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "configure public_base_url, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "missing_prerequisite",
                "missing_config_key": key,
            }),
        };
    };

    if url::Url::parse(&public_base_url).is_err() {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "invalid_answer",
            "step": step.id,
            "field": key,
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "fix public_base_url, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "invalid_answer",
                "field": key,
            }),
        };
    }

    ensure_outputs_object(state).insert(key.to_string(), Value::String(public_base_url.clone()));
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "public URL resolved".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "output_key": key,
            "public_base_url": public_base_url,
        }),
    }
}

fn execute_cloudflare_tunnel_step(
    bundle_root: &Path,
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) -> MachineStepResult {
    let key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .unwrap_or("public_base_url");
    let candidate = output_string(&state.outputs, key)
        .or_else(|| {
            step.extra
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            step.extra
                .get("public_base_url")
                .or_else(|| step.extra.get("default"))
                .or_else(|| step.extra.get("default_url"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            crate::platform_setup::load_effective_static_routes_defaults(
                bundle_root,
                &state.tenant,
                Some(&state.team),
            )
            .ok()
            .flatten()
            .and_then(|policy| policy.public_base_url)
        })
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.trim().is_empty());

    let Some(public_base_url) = candidate else {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "tunnel_unreachable",
            "step": step.id,
            "missing": key,
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "start or configure a setup tunnel, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "tunnel_unreachable",
                "missing_config_key": key,
            }),
        };
    };

    let parsed = match url::Url::parse(&public_base_url) {
        Ok(parsed) => parsed,
        Err(err) => {
            state.status = SetupMachineStatus::Paused;
            state.current_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "invalid_answer",
                "step": step.id,
                "field": key,
                "detail": err.to_string(),
                "recoverable": true,
            }));
            return MachineStepResult {
                ok: false,
                next: "fix setup tunnel URL, then retry".to_string(),
                detail: serde_json::json!({
                    "blocked": true,
                    "retryable": true,
                    "error": "invalid_answer",
                    "field": key,
                    "detail": err.to_string(),
                }),
            };
        }
    };
    if parsed.scheme() != "https" || parsed.host_str().is_none() {
        state.status = SetupMachineStatus::Paused;
        state.current_step = Some(step.id.clone());
        state.last_error = Some(serde_json::json!({
            "error": "invalid_answer",
            "step": step.id,
            "field": key,
            "detail": "setup tunnel URL must be an https URL with a host",
            "recoverable": true,
        }));
        return MachineStepResult {
            ok: false,
            next: "fix setup tunnel URL, then retry".to_string(),
            detail: serde_json::json!({
                "blocked": true,
                "retryable": true,
                "error": "invalid_answer",
                "field": key,
                "detail": "setup tunnel URL must be an https URL with a host",
            }),
        };
    }

    ensure_outputs_object(state).insert(key.to_string(), Value::String(public_base_url.clone()));
    mark_machine_step_complete(machine, step, state);
    MachineStepResult {
        ok: true,
        next: "setup tunnel URL resolved".to_string(),
        detail: serde_json::json!({
            "kind": step.kind,
            "output_key": key,
            "public_base_url": public_base_url,
            "ephemeral": crate::setup_tunnel::is_ephemeral_tunnel_url(&public_base_url),
        }),
    }
}

fn ensure_outputs_object(state: &mut SetupMachineState) -> &mut serde_json::Map<String, Value> {
    if !state.outputs.is_object() {
        state.outputs = serde_json::json!({});
    }
    state.outputs.as_object_mut().expect("outputs object")
}

fn output_string(outputs: &Value, key: &str) -> Option<String> {
    outputs
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn validate_http_step_url(value: &str) -> anyhow::Result<()> {
    let url = url::Url::parse(value)?;
    match url.scheme() {
        "http" if url.host_str().is_some_and(is_loopback_host) => Ok(()),
        "https" => Ok(()),
        _ => anyhow::bail!("setup HTTP URL must use https or loopback http"),
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn expand_machine_template(template: &str, state: &SetupMachineState) -> anyhow::Result<String> {
    let mut expanded = template
        .replace("{tenant}", &state.tenant)
        .replace("{team}", &state.team)
        .replace("{provider_id}", &state.provider_id);
    let mut start = 0;
    while let Some(open) = expanded[start..].find('{') {
        let open = start + open;
        let Some(close_rel) = expanded[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + close_rel;
        let key = &expanded[open + 1..close];
        let Some(value) = output_string(&state.outputs, key) else {
            anyhow::bail!("missing setup-machine output `{key}`");
        };
        expanded.replace_range(open..=close, &value);
        start = open + value.len();
    }
    Ok(expanded)
}

fn expand_machine_value_templates(value: Value, state: &SetupMachineState) -> Value {
    match value {
        Value::String(text) => expand_machine_template(&text, state)
            .map(Value::String)
            .unwrap_or(Value::String(text)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| expand_machine_value_templates(value, state))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, expand_machine_value_templates(value, state)))
                .collect(),
        ),
        other => other,
    }
}

fn mark_machine_step_complete(
    machine: &SetupMachine,
    step: &SetupMachineStep,
    state: &mut SetupMachineState,
) {
    if !state.completed_steps.iter().any(|id| id == &step.id) {
        state.completed_steps.push(step.id.clone());
    }
    state.last_error = None;
    state.failed_step = None;
    match step.on_success.as_deref() {
        Some(TERMINAL_SUCCESS) | None => {
            state.status = SetupMachineStatus::Complete;
            state.current_step = None;
        }
        Some(TERMINAL_FAILED) => {
            state.status = SetupMachineStatus::Failed;
            state.current_step = None;
            state.failed_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "transitioned_to_failed",
                "step": step.id,
            }));
        }
        Some(next) if machine.steps.iter().any(|candidate| candidate.id == next) => {
            state.status = SetupMachineStatus::Running;
            state.current_step = Some(next.to_string());
        }
        Some(next) => {
            state.status = SetupMachineStatus::Failed;
            state.current_step = Some(step.id.clone());
            state.failed_step = Some(step.id.clone());
            state.last_error = Some(serde_json::json!({
                "error": "invalid_transition_target",
                "step": step.id,
                "target": next,
            }));
        }
    }
}

fn machine_step_output(
    machine: &SetupMachine,
    state: &SetupMachineState,
    step_id: &str,
    dry_run: bool,
    ok: bool,
    next: &str,
    event_path: Option<PathBuf>,
) -> Value {
    serde_json::json!({
        "provider_id": state.provider_id,
        "tenant": state.tenant,
        "team": state.team,
        "machine_id": machine.id,
        "dry_run": dry_run,
        "step": step_id,
        "ok": ok,
        "next": next,
        "result": {
            "ok": ok,
            "status": state.status,
        },
        "event_path": event_path,
        "status": render_setup_machine_status(machine, state),
    })
}

fn append_machine_step_event(
    bundle_root: &Path,
    state: &SetupMachineState,
    event_name: &str,
    step_id: Option<&str>,
    detail: Option<Value>,
) -> anyhow::Result<PathBuf> {
    let mut fields = serde_json::Map::new();
    if let Some(detail) = detail {
        fields.insert("detail".to_string(), detail);
    }
    append_setup_machine_event(
        bundle_root,
        &SetupMachineEvent {
            ts: now_setup_ts(),
            level: "info".to_string(),
            event: event_name.to_string(),
            provider_id: state.provider_id.clone(),
            tenant: state.tenant.clone(),
            team: state.team.clone(),
            machine_id: state.machine_id.clone(),
            step_id: step_id.map(str::to_string),
            correlation_id: Some(format!("setup-{}", now_setup_ms())),
            fields,
        },
    )
}

fn now_setup_ts() -> String {
    now_setup_ms().to_string()
}

fn now_setup_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn safe_archive_segment(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "archive".to_string()
    } else {
        out.to_string()
    }
}

fn resolve_machine_value(pack_path: &Path, extension: Value) -> anyhow::Result<Value> {
    if let Some(inline) = extension.get("inline") {
        return Ok(inline.clone());
    }
    if let Some(path) = extension.get("path").and_then(Value::as_str) {
        return discovery::read_pack_json_asset(pack_path, path);
    }
    Ok(extension)
}

fn extension_inline(extension: &Value) -> Option<&Value> {
    extension.get("inline").or(Some(extension))
}

fn validate_schema_id(value: &Value, expected: &str, label: &str) -> anyhow::Result<()> {
    let schema_id = value
        .get("schema_id")
        .and_then(Value::as_str)
        .unwrap_or(expected);
    if schema_id != expected {
        anyhow::bail!("{label} has schema_id {schema_id}, expected {expected}");
    }
    Ok(())
}

fn validate_provider_id(
    provider_id: Option<&str>,
    expected_provider_id: Option<&str>,
) -> anyhow::Result<()> {
    let Some(expected) = expected_provider_id else {
        return Ok(());
    };
    let Some(actual) = provider_id else {
        anyhow::bail!("setup backend contract provider_id is missing");
    };
    if actual != expected {
        anyhow::bail!(
            "setup backend contract provider_id {actual} does not match pack id {expected}"
        );
    }
    Ok(())
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_unique_strings(check_id: &str, message: &str, values: &[String]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            diagnostics.push(error_with_actual(check_id, message, value));
        }
    }
    diagnostics
}

fn require_executor_str(
    diagnostics: &mut Vec<Diagnostic>,
    action_id: &str,
    executor: &serde_json::Map<String, Value>,
    key: &str,
) {
    if executor
        .get(key)
        .and_then(Value::as_str)
        .is_none_or(|value| value.trim().is_empty())
    {
        diagnostics.push(error_with_actual(
            "setup.backend_contract.executor.required_field",
            "setup backend executor is missing a required field",
            &format!("{action_id}.{key}"),
        ));
    }
}

fn validate_transition(
    diagnostics: &mut Vec<Diagnostic>,
    step_ids: &BTreeSet<String>,
    source_step: &str,
    check_id: &str,
    target: Option<&str>,
) {
    let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    if matches!(target, TERMINAL_SUCCESS | TERMINAL_FAILED) || step_ids.contains(target) {
        return;
    }
    diagnostics.push(Diagnostic {
        check_id: check_id.to_string(),
        severity: DiagnosticSeverity::Error,
        component: "provider_setup".to_string(),
        message: "setup transition target must reference an existing step or terminal state"
            .to_string(),
        evidence: Some(format!("{source_step} -> {target}")),
        expected: Some("existing step id, complete, or failed".to_string()),
        actual: Some(target.to_string()),
        fix_hint: Some("fix the transition target or add the missing setup step".to_string()),
        related_file: None,
        related_pack: None,
        related_component: None,
    });
}

fn rule_targets(rule: &Value) -> Vec<String> {
    let Some(obj) = rule.as_object() else {
        return Vec::new();
    };
    ["step", "next", "target", "on_success"]
        .iter()
        .filter_map(|key| obj.get(*key).and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn validate_reachability(machine: &SetupMachine, step_ids: &BTreeSet<String>) -> Vec<Diagnostic> {
    if !step_ids.contains(&machine.entry_step) {
        return Vec::new();
    }

    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([machine.entry_step.clone()]);
    while let Some(step_id) = queue.pop_front() {
        if !seen.insert(step_id.clone()) {
            continue;
        }
        let Some(step) = machine.steps.iter().find(|step| step.id == step_id) else {
            continue;
        };
        if let Some(target) = step.on_success.as_deref()
            && step_ids.contains(target)
        {
            queue.push_back(target.to_string());
        }
        for rule in step.on_failure.iter().chain(step.recover.iter()) {
            for target in rule_targets(rule) {
                if step_ids.contains(&target) {
                    queue.push_back(target);
                }
            }
        }
    }

    step_ids
        .difference(&seen)
        .map(|step_id| {
            error_with_actual(
                "setup.machine.step.reachable",
                "setup step is not reachable from entry_step",
                step_id,
            )
        })
        .collect()
}

fn validate_unbounded_cycles(
    machine: &SetupMachine,
    step_ids: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let steps: BTreeMap<&str, &SetupMachineStep> = machine
        .steps
        .iter()
        .map(|step| (step.id.as_str(), step))
        .collect();
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for step in &machine.steps {
        let mut targets = Vec::new();
        if let Some(target) = step.on_success.as_deref()
            && step_ids.contains(target)
        {
            targets.push(target.to_string());
        }
        for rule in step.on_failure.iter().chain(step.recover.iter()) {
            for target in rule_targets(rule) {
                if step_ids.contains(&target) {
                    targets.push(target);
                }
            }
        }
        edges.insert(step.id.clone(), targets);
    }

    let mut diagnostics = Vec::new();
    let mut visited = BTreeSet::new();
    let mut stack = Vec::<String>::new();
    let mut reported = BTreeSet::new();
    for step_id in step_ids {
        detect_cycle(
            step_id,
            &edges,
            &steps,
            &mut visited,
            &mut stack,
            &mut reported,
            &mut diagnostics,
        );
    }
    diagnostics
}

fn validate_artifact_paths(machine: &SetupMachine) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for step in &machine.steps {
        if !matches!(step.kind.as_str(), "generate_file" | "download_file") {
            continue;
        }
        let path = step
            .extra
            .get("artifact_path")
            .or_else(|| step.extra.get("path"))
            .or_else(|| step.extra.get("output_path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(path) = path else {
            diagnostics.push(error_with_actual(
                "setup.machine.artifact.path",
                "setup artifact steps must declare artifact_path",
                &step.id,
            ));
            continue;
        };
        if path.starts_with('/')
            || path.contains("..")
            || !(path.starts_with("artifacts/") || path.starts_with("state/setup/"))
        {
            diagnostics.push(error_with_actual(
                "setup.machine.artifact.path.safe",
                "setup artifact path must be relative and stay under setup artifacts",
                &format!("{}: {}", step.id, path),
            ));
        }
    }
    diagnostics
}

fn validate_http_steps(machine: &SetupMachine) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for step in &machine.steps {
        if !matches!(step.kind.as_str(), "http_probe" | "http_json") {
            continue;
        }
        let url = step
            .extra
            .get("url_template")
            .or_else(|| step.extra.get("url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let Some(url) = url else {
            diagnostics.push(error_with_actual(
                "setup.machine.http.url",
                "setup HTTP steps must declare url_template",
                &step.id,
            ));
            continue;
        };
        if !url.contains('{')
            && let Err(err) = validate_http_step_url(url)
        {
            diagnostics.push(error_with_actual(
                "setup.machine.http.url.safe",
                "setup HTTP URL must use https or loopback http",
                &format!("{}: {url} ({err})", step.id),
            ));
        }
        let method = step
            .extra
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_ascii_uppercase();
        if !matches!(method.as_str(), "GET" | "POST") {
            diagnostics.push(error_with_actual(
                "setup.machine.http.method",
                "setup HTTP method must be GET or POST",
                &format!("{}: {method}", step.id),
            ));
        }
        if step.kind == "http_json" && method != "GET" && !has_idempotency_key(step) {
            diagnostics.push(error_with_actual(
                "setup.machine.step.idempotency_key",
                "side-effecting setup steps must declare idempotency_key",
                &step.id,
            ));
        }
        if let Some(status) = step
            .extra
            .get("expected_status")
            .or_else(|| step.extra.get("status"))
            .and_then(Value::as_u64)
            && !(100..=599).contains(&status)
        {
            diagnostics.push(error_with_actual(
                "setup.machine.http.expected_status",
                "setup HTTP expected_status must be a valid HTTP status code",
                &format!("{}: {status}", step.id),
            ));
        }
    }
    diagnostics
}

fn validate_tunnel_steps(machine: &SetupMachine) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for step in &machine.steps {
        if step.kind != "cloudflare_tunnel" {
            continue;
        }
        if let Some(mode) = step.extra.get("mode").and_then(Value::as_str)
            && !matches!(mode, "cloudflared" | "cloudflare" | "external")
        {
            diagnostics.push(error_with_actual(
                "setup.machine.cloudflare_tunnel.mode",
                "cloudflare_tunnel mode must be cloudflared, cloudflare, or external",
                &format!("{}: {}", step.id, mode),
            ));
        }
        if let Some(url) = step
            .extra
            .get("public_base_url")
            .or_else(|| step.extra.get("default"))
            .or_else(|| step.extra.get("default_url"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && !url.contains('{')
        {
            match url::Url::parse(url) {
                Ok(parsed) if parsed.scheme() == "https" && parsed.host_str().is_some() => {}
                Ok(_) | Err(_) => diagnostics.push(error_with_actual(
                    "setup.machine.cloudflare_tunnel.public_base_url",
                    "cloudflare_tunnel declared public URL must be https with a host",
                    &format!("{}: {}", step.id, url),
                )),
            }
        }
    }
    diagnostics
}

fn validate_oauth_steps(machine: &SetupMachine) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for step in &machine.steps {
        match step.kind.as_str() {
            "oauth_device_code" => validate_oauth_device_step(&mut diagnostics, step),
            "oauth_authorization_code" => validate_oauth_authorization_step(&mut diagnostics, step),
            _ => {}
        }
    }
    diagnostics
}

fn validate_oauth_device_step(diagnostics: &mut Vec<Diagnostic>, step: &SetupMachineStep) {
    let has_device_url = step
        .extra
        .get("device_code_url")
        .or_else(|| step.extra.get("device_code_url_template"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let has_token_url = step
        .extra
        .get("token_url")
        .or_else(|| step.extra.get("token_url_template"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let has_authority = step
        .extra
        .get("authority_url_template")
        .or_else(|| step.extra.get("authority_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    if !has_authority && (!has_device_url || !has_token_url) {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_device_code.endpoints",
            "oauth_device_code steps must declare authority_url_template or both device_code_url and token_url",
            &step.id,
        ));
    }
    for key in ["device_code_url", "token_url", "authority_url"] {
        if let Some(url) = step
            .extra
            .get(key)
            .or_else(|| step.extra.get(&format!("{key}_template")))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            && !url.contains('{')
            && let Err(err) = validate_http_step_url(url)
        {
            diagnostics.push(error_with_actual(
                "setup.machine.oauth_device_code.url.safe",
                "oauth_device_code URLs must use https or loopback http",
                &format!("{}: {url} ({err})", step.id),
            ));
        }
    }
    let has_client_id = step
        .extra
        .get("client_id")
        .or_else(|| step.extra.get("client_id_output"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    if !has_client_id {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_device_code.client_id",
            "oauth_device_code steps must declare client_id or client_id_output",
            &step.id,
        ));
    }
    if step.extra.get("scope").is_none() && step.extra.get("scopes").is_none() {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_device_code.scopes",
            "oauth_device_code steps must declare scope or scopes",
            &step.id,
        ));
    }
    if let Some(scopes) = step.extra.get("scopes")
        && !scopes.is_array()
    {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_device_code.scopes",
            "oauth_device_code scopes must be an array",
            &step.id,
        ));
    }
}

fn validate_oauth_authorization_step(diagnostics: &mut Vec<Diagnostic>, step: &SetupMachineStep) {
    let authorize_url = step
        .extra
        .get("authorize_url")
        .or_else(|| step.extra.get("authorization_url"))
        .or_else(|| step.extra.get("auth_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(authorize_url) = authorize_url else {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_authorization_code.authorize_url",
            "oauth_authorization_code steps must declare authorize_url",
            &step.id,
        ));
        return;
    };
    if !authorize_url.contains('{') {
        match url::Url::parse(authorize_url) {
            Ok(parsed) if parsed.scheme() == "https" && parsed.host_str().is_some() => {}
            Ok(_) | Err(_) => diagnostics.push(error_with_actual(
                "setup.machine.oauth_authorization_code.authorize_url.safe",
                "oauth_authorization_code authorize_url must be https with a host",
                &format!("{}: {}", step.id, authorize_url),
            )),
        }
    }
    let token_url = step
        .extra
        .get("token_url")
        .or_else(|| step.extra.get("token_url_template"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match token_url {
        Some(token_url) if !token_url.contains('{') => match url::Url::parse(token_url) {
            Ok(parsed)
                if (parsed.host_str().is_some() && parsed.scheme() == "https")
                    || (parsed.scheme() == "http"
                        && matches!(
                            parsed.host_str(),
                            Some("127.0.0.1" | "localhost" | "::1")
                        )) => {}
            Ok(_) | Err(_) => diagnostics.push(error_with_actual(
                "setup.machine.oauth_authorization_code.token_url.safe",
                "oauth_authorization_code token_url must be https or loopback http",
                &format!("{}: {}", step.id, token_url),
            )),
        },
        Some(_) => {}
        None => diagnostics.push(error_with_actual(
            "setup.machine.oauth_authorization_code.token_url",
            "oauth_authorization_code steps must declare token_url",
            &step.id,
        )),
    }
    if let Some(scopes) = step.extra.get("scopes")
        && !scopes.is_array()
    {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_authorization_code.scopes",
            "oauth_authorization_code scopes must be an array",
            &step.id,
        ));
    }
    if let Some(params) = step.extra.get("params")
        && !params.is_object()
    {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_authorization_code.params",
            "oauth_authorization_code params must be a JSON object",
            &step.id,
        ));
    }
    if let Some(ttl) = step.extra.get("state_ttl_secs").and_then(Value::as_u64)
        && ttl == 0
    {
        diagnostics.push(error_with_actual(
            "setup.machine.oauth_authorization_code.state_ttl_secs",
            "oauth_authorization_code state_ttl_secs must be greater than zero",
            &step.id,
        ));
    }
}

fn validate_data_mapping_steps(machine: &SetupMachine) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for step in &machine.steps {
        match step.kind.as_str() {
            "select_from_json" => {
                if step
                    .extra
                    .get("source_output")
                    .or_else(|| step.extra.get("source"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    diagnostics.push(error_with_actual(
                        "setup.machine.select_from_json.source_output",
                        "select_from_json steps must declare source_output",
                        &step.id,
                    ));
                }
                if let Some(pointer) = step.extra.get("json_pointer").and_then(Value::as_str)
                    && !pointer.starts_with('/')
                {
                    diagnostics.push(error_with_actual(
                        "setup.machine.select_from_json.json_pointer",
                        "select_from_json json_pointer must use JSON Pointer syntax",
                        &format!("{}: {}", step.id, pointer),
                    ));
                }
            }
            "persist_runtime_config" => {
                let config = step.extra.get("config");
                let mappings = step.extra.get("mappings");
                if config.is_none() && mappings.is_none() {
                    diagnostics.push(error_with_actual(
                        "setup.machine.persist_runtime_config.input",
                        "persist_runtime_config steps must declare config or mappings",
                        &step.id,
                    ));
                }
                for (name, value) in [("config", config), ("mappings", mappings)] {
                    if let Some(value) = value
                        && !value.is_object()
                    {
                        diagnostics.push(error_with_actual(
                            "setup.machine.persist_runtime_config.object",
                            "persist_runtime_config config and mappings must be JSON objects",
                            &format!("{}: {}", step.id, name),
                        ));
                    }
                }
                if let Some(server_owned) = step.extra.get("server_owned_config_keys")
                    && !server_owned.is_array()
                {
                    diagnostics.push(error_with_actual(
                        "setup.machine.persist_runtime_config.server_owned_config_keys",
                        "persist_runtime_config server_owned_config_keys must be an array",
                        &step.id,
                    ));
                }
                let server_owned = setup_machine_server_owned_keys(machine, step);
                for (name, value) in [("config", config), ("mappings", mappings)] {
                    if let Some(object) = value.and_then(Value::as_object) {
                        for key in object.keys() {
                            if server_owned.contains(key) {
                                diagnostics.push(error_with_actual(
                                    "setup.machine.persist_runtime_config.server_owned_output",
                                    "persist_runtime_config config and mappings must not write server-owned keys",
                                    &format!("{}: {}.{}", step.id, name, key),
                                ));
                            }
                        }
                    }
                }
            }
            "provider_component_call" => {
                if !has_idempotency_key(step) {
                    diagnostics.push(error_with_actual(
                        "setup.machine.step.idempotency_key",
                        "side-effecting setup steps must declare idempotency_key",
                        &step.id,
                    ));
                }
                if step
                    .extra
                    .get("component_ref")
                    .or_else(|| step.extra.get("component"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    diagnostics.push(error_with_actual(
                        "setup.machine.provider_component_call.component_ref",
                        "provider_component_call steps must declare component_ref",
                        &step.id,
                    ));
                }
                if step
                    .extra
                    .get("op")
                    .or_else(|| step.extra.get("operation"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .is_none()
                {
                    diagnostics.push(error_with_actual(
                        "setup.machine.provider_component_call.op",
                        "provider_component_call steps must declare op",
                        &step.id,
                    ));
                }
                if let Some(request) = step.extra.get("request").or_else(|| step.extra.get("body"))
                    && !request.is_object()
                {
                    diagnostics.push(error_with_actual(
                        "setup.machine.provider_component_call.request",
                        "provider_component_call request/body must be a JSON object",
                        &step.id,
                    ));
                }
            }
            _ => {}
        }
    }
    diagnostics
}

fn validate_setup_machine_template_inputs(
    pack_path: &Path,
    provider_id: Option<&str>,
    machine: &SetupMachine,
) -> Vec<Diagnostic> {
    let mut available = BTreeSet::from([
        "tenant".to_string(),
        "team".to_string(),
        "provider_id".to_string(),
    ]);

    if let Some(provider_id) = provider_id
        && let Some(form_spec) = crate::setup_to_formspec::pack_to_form_spec(pack_path, provider_id)
    {
        for question in form_spec.questions {
            if !question.id.trim().is_empty() {
                available.insert(question.id);
            }
        }
    }

    for step in &machine.steps {
        available.extend(setup_machine_step_output_keys(step));
    }

    let mut diagnostics = Vec::new();
    for step in &machine.steps {
        for (path, key) in setup_machine_step_template_references(step) {
            if !available.contains(&key) {
                diagnostics.push(error_with_actual(
                    "setup.machine.template.input",
                    "setup-machine template placeholder must reference a built-in value, setup answer, or declared setup output",
                    &format!("{}: {} -> {}", step.id, path, key),
                ));
            }
        }
    }
    diagnostics
}

fn setup_machine_step_template_references(step: &SetupMachineStep) -> Vec<(String, String)> {
    let mut references = Vec::new();
    for (key, value) in &step.extra {
        collect_template_references(value, key, &mut references);
    }
    references
}

fn collect_template_references(value: &Value, path: &str, references: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) => {
            for key in template_placeholders(text) {
                references.push((path.to_string(), key));
            }
        }
        Value::Array(values) => {
            for (idx, value) in values.iter().enumerate() {
                collect_template_references(value, &format!("{path}[{idx}]"), references);
            }
        }
        Value::Object(map) => {
            for (key, value) in map {
                collect_template_references(value, &format!("{path}.{key}"), references);
            }
        }
        _ => {}
    }
}

fn template_placeholders(text: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut start = 0;
    while let Some(open_rel) = text[start..].find('{') {
        let open = start + open_rel;
        let Some(close_rel) = text[open + 1..].find('}') else {
            break;
        };
        let close = open + 1 + close_rel;
        let key = text[open + 1..close].trim();
        if !key.is_empty() {
            keys.push(key.to_string());
        }
        start = close + 1;
    }
    keys
}

fn setup_machine_step_output_keys(step: &SetupMachineStep) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for output in &step.outputs {
        match output {
            Value::String(key) if !key.trim().is_empty() => {
                keys.insert(key.trim().to_string());
            }
            Value::Object(object) => {
                for field in ["key", "name", "id"] {
                    if let Some(key) = object
                        .get(field)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                    {
                        keys.insert(key.to_string());
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    let output_key = step
        .extra
        .get("output_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match step.kind.as_str() {
        "platform_public_url" | "cloudflare_tunnel" => {
            keys.insert(output_key.unwrap_or("public_base_url").to_string());
        }
        "oauth_device_code" => {
            keys.insert(output_key.unwrap_or("oauth_device_login").to_string());
        }
        "oauth_authorization_code" => {
            keys.insert(output_key.unwrap_or("authorization_url").to_string());
        }
        "http_json" | "select_from_json" | "generate_file" | "provider_component_call" => {
            let key = output_key.unwrap_or(step.id.as_str()).to_string();
            if step.kind == "select_from_json" {
                keys.insert(format!("{key}_item"));
            }
            keys.insert(key);
        }
        "persist_runtime_config" => {
            keys.insert(output_key.unwrap_or("runtime_config_path").to_string());
        }
        _ => {}
    }
    keys
}

fn has_idempotency_key(step: &SetupMachineStep) -> bool {
    step.extra
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

fn validate_setup_i18n_message_keys(pack_path: &Path, machine: &SetupMachine) -> Vec<Diagnostic> {
    let extension = match discovery::read_pack_extension(pack_path, SETUP_I18N_EXTENSION) {
        Ok(Some(extension)) => extension,
        Ok(None) => return Vec::new(),
        Err(err) => {
            return vec![load_error(
                "setup.i18n.load",
                "setup i18n descriptor could not be loaded",
                SETUP_I18N_EXTENSION,
                err,
                pack_path,
            )];
        }
    };
    let mut diagnostics = Vec::new();
    let declared_keys = match collect_setup_i18n_keys(pack_path, &extension) {
        Ok(keys) => keys,
        Err(err) => {
            diagnostics.push(load_error(
                "setup.i18n.messages.load",
                "setup i18n messages could not be loaded",
                "valid setup i18n JSON",
                err,
                pack_path,
            ));
            return diagnostics;
        }
    };
    if declared_keys.is_empty() {
        diagnostics.push(Diagnostic {
            check_id: "setup.i18n.messages.present".to_string(),
            severity: DiagnosticSeverity::Error,
            component: "provider_setup".to_string(),
            message: "setup i18n extension does not declare any message keys".to_string(),
            evidence: Some(SETUP_I18N_EXTENSION.to_string()),
            expected: Some("inline messages or message asset with at least one key".to_string()),
            actual: Some("empty".to_string()),
            fix_hint: Some(
                "add localized setup messages or remove the setup i18n extension".to_string(),
            ),
            related_file: Some(pack_path.display().to_string()),
            related_pack: Some(machine.id.clone()),
            related_component: None,
        });
        return diagnostics;
    }

    for key in collect_setup_message_key_references(machine) {
        if !declared_keys.contains(&key) {
            diagnostics.push(error_with_actual(
                "setup.i18n.message_key.exists",
                "setup message_key must exist in greentic.setup.i18n.v1 messages",
                &key,
            ));
        }
    }
    diagnostics
}

fn validate_setup_machine_fixtures(pack_path: &Path, machine: &SetupMachine) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let fixture_paths = setup_machine_fixture_paths(machine);
    if fixture_paths.is_empty() {
        return diagnostics;
    }

    let step_ids = machine
        .steps
        .iter()
        .map(|step| step.id.as_str())
        .collect::<BTreeSet<_>>();
    for path in fixture_paths {
        let value = match discovery::read_pack_json_asset(pack_path, &path) {
            Ok(value) => value,
            Err(err) => {
                diagnostics.push(load_error(
                    "setup.machine.fixture.load",
                    "setup-machine fixture could not be loaded",
                    "JSON fixture asset inside the provider pack",
                    err,
                    pack_path,
                ));
                continue;
            }
        };
        let state_value = value.get("state").cloned().unwrap_or(value);
        let state: SetupMachineState = match serde_json::from_value(state_value) {
            Ok(state) => state,
            Err(err) => {
                diagnostics.push(error_with_actual(
                    "setup.machine.fixture.state",
                    "setup-machine fixture must contain a valid setup-machine state",
                    &format!("{path}: {err}"),
                ));
                continue;
            }
        };
        if state.machine_id != machine.id {
            diagnostics.push(error_with_actual(
                "setup.machine.fixture.machine_id",
                "setup-machine fixture machine_id must match the current machine",
                &format!("{}: {}", path, state.machine_id),
            ));
        }
        if state.machine_version != machine.version {
            diagnostics.push(error_with_actual(
                "setup.machine.fixture.machine_version",
                "setup-machine fixture machine_version must match the current machine version",
                &format!("{}: {}", path, state.machine_version),
            ));
        }
        if let Some(current_step) = state.current_step.as_deref()
            && !step_ids.contains(current_step)
        {
            diagnostics.push(error_with_actual(
                "setup.machine.fixture.current_step",
                "setup-machine fixture current_step must reference a declared step",
                &format!("{path}: {current_step}"),
            ));
        }
        if let Some(failed_step) = state.failed_step.as_deref()
            && !step_ids.contains(failed_step)
        {
            diagnostics.push(error_with_actual(
                "setup.machine.fixture.failed_step",
                "setup-machine fixture failed_step must reference a declared step",
                &format!("{path}: {failed_step}"),
            ));
        }
        for completed_step in &state.completed_steps {
            if !step_ids.contains(completed_step.as_str()) {
                diagnostics.push(error_with_actual(
                    "setup.machine.fixture.completed_step",
                    "setup-machine fixture completed_steps must reference declared steps",
                    &format!("{path}: {completed_step}"),
                ));
            }
        }
    }

    diagnostics
}

fn setup_machine_fixture_paths(machine: &SetupMachine) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(fixtures) = machine.extra.get("fixtures").and_then(Value::as_array) {
        for fixture in fixtures {
            if let Some(path) = fixture.as_str() {
                paths.push(path.to_string());
            } else if let Some(path) = fixture.get("path").and_then(Value::as_str) {
                paths.push(path.to_string());
            }
        }
    }
    if let Some(fixture_paths) = machine.extra.get("fixture_paths").and_then(Value::as_array) {
        for path in fixture_paths.iter().filter_map(Value::as_str) {
            paths.push(path.to_string());
        }
    }
    paths
        .into_iter()
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .collect()
}

fn collect_setup_i18n_keys(
    pack_path: &Path,
    extension: &Value,
) -> anyhow::Result<BTreeSet<String>> {
    let descriptor = extension_inline(extension).unwrap_or(extension);
    let mut keys = BTreeSet::new();
    collect_message_keys_from_value(&mut keys, descriptor);

    for key in ["asset", "path", "messages_asset", "messages_path"] {
        if let Some(path) = descriptor.get(key).and_then(Value::as_str) {
            let value = discovery::read_pack_json_asset(pack_path, path)?;
            collect_message_keys_from_value(&mut keys, &value);
        }
    }

    for key in ["assets", "paths", "locales"] {
        if let Some(map) = descriptor.get(key).and_then(Value::as_object) {
            for value in map.values() {
                if let Some(path) = value.as_str() {
                    let messages = discovery::read_pack_json_asset(pack_path, path)?;
                    collect_message_keys_from_value(&mut keys, &messages);
                } else {
                    collect_message_keys_from_value(&mut keys, value);
                }
            }
        }
    }

    Ok(keys)
}

fn collect_message_keys_from_value(keys: &mut BTreeSet<String>, value: &Value) {
    if let Some(messages) = value.get("messages").and_then(Value::as_object) {
        collect_message_keys_from_object(keys, messages);
    }
    if let Some(translations) = value.get("translations").and_then(Value::as_object) {
        collect_message_keys_from_object(keys, translations);
    }
    if let Some(obj) = value.as_object() {
        collect_message_keys_from_object(keys, obj);
    }
}

fn collect_message_keys_from_object(
    keys: &mut BTreeSet<String>,
    obj: &serde_json::Map<String, Value>,
) {
    for (key, value) in obj {
        if [
            "schema_id",
            "provider_id",
            "default_locale",
            "asset",
            "path",
            "messages",
            "translations",
            "assets",
            "paths",
            "locales",
        ]
        .contains(&key.as_str())
        {
            continue;
        }
        match value {
            Value::String(_) => {
                keys.insert(key.clone());
            }
            Value::Object(map) if map.values().all(|candidate| candidate.as_str().is_some()) => {
                keys.insert(key.clone());
            }
            Value::Object(_) => collect_message_keys_from_value(keys, value),
            _ => {}
        }
    }
}

fn collect_setup_message_key_references(machine: &SetupMachine) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for step in &machine.steps {
        collect_message_key_from_value(&mut keys, &Value::Object(step.extra.clone()));
        for rule in step.on_failure.iter().chain(step.recover.iter()) {
            collect_message_key_from_value(&mut keys, rule);
        }
    }
    keys
}

fn collect_message_key_from_value(keys: &mut BTreeSet<String>, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                if (key == "message_key" || key == "title_key" || key == "description_key")
                    && let Some(message_key) = nested.as_str()
                    && !message_key.trim().is_empty()
                {
                    keys.insert(message_key.to_string());
                }
                collect_message_key_from_value(keys, nested);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_message_key_from_value(keys, nested);
            }
        }
        _ => {}
    }
}

fn detect_cycle(
    step_id: &str,
    edges: &BTreeMap<String, Vec<String>>,
    steps: &BTreeMap<&str, &SetupMachineStep>,
    visited: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
    reported: &mut BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(position) = stack.iter().position(|candidate| candidate == step_id) {
        let cycle = stack[position..].to_vec();
        let key = cycle.join(" -> ");
        if reported.insert(key.clone())
            && !cycle.iter().any(|id| {
                steps.get(id.as_str()).is_some_and(|step| {
                    step.extra.get("retry").is_some() || step.extra.get("timeout").is_some()
                })
            })
        {
            diagnostics.push(error_with_actual(
                "setup.machine.step.cycle",
                "setup step cycle must declare bounded retry or timeout policy",
                &key,
            ));
        }
        return;
    }
    if !visited.insert(step_id.to_string()) {
        return;
    }
    stack.push(step_id.to_string());
    for target in edges.get(step_id).into_iter().flatten() {
        detect_cycle(target, edges, steps, visited, stack, reported, diagnostics);
    }
    stack.pop();
}

fn error(check_id: &str, message: &str) -> Diagnostic {
    Diagnostic {
        check_id: check_id.to_string(),
        severity: DiagnosticSeverity::Error,
        component: "provider_setup".to_string(),
        message: message.to_string(),
        evidence: None,
        expected: None,
        actual: None,
        fix_hint: None,
        related_file: None,
        related_pack: None,
        related_component: None,
    }
}

fn error_with_actual(check_id: &str, message: &str, actual: &str) -> Diagnostic {
    Diagnostic {
        check_id: check_id.to_string(),
        severity: DiagnosticSeverity::Error,
        component: "provider_setup".to_string(),
        message: message.to_string(),
        evidence: None,
        expected: None,
        actual: Some(actual.to_string()),
        fix_hint: None,
        related_file: None,
        related_pack: None,
        related_component: None,
    }
}

fn info(
    check_id: &str,
    message: &str,
    evidence: &str,
    pack_path: &Path,
    related_pack: Option<&str>,
) -> Diagnostic {
    Diagnostic {
        check_id: check_id.to_string(),
        severity: DiagnosticSeverity::Info,
        component: "provider_setup".to_string(),
        message: message.to_string(),
        evidence: Some(evidence.to_string()),
        expected: None,
        actual: None,
        fix_hint: None,
        related_file: Some(pack_path.display().to_string()),
        related_pack: related_pack.map(str::to_string),
        related_component: None,
    }
}

fn load_error(
    check_id: &str,
    message: &str,
    expected: &str,
    err: anyhow::Error,
    pack_path: &Path,
) -> Diagnostic {
    Diagnostic {
        check_id: check_id.to_string(),
        severity: DiagnosticSeverity::Error,
        component: "provider_setup".to_string(),
        message: message.to_string(),
        evidence: Some(err.to_string()),
        expected: Some(expected.to_string()),
        actual: None,
        fix_hint: None,
        related_file: Some(pack_path.display().to_string()),
        related_pack: None,
        related_component: None,
    }
}

fn report(pack_path: &Path, diagnostics: Vec<Diagnostic>) -> DoctorReport {
    let error_count = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .count();
    let warn_count = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Warn)
        .count();
    let info_count = diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Info)
        .count();
    DoctorReport {
        bundle: pack_path.display().to_string(),
        status: if error_count > 0 {
            "error".to_string()
        } else if warn_count > 0 {
            "warn".to_string()
        } else {
            "ok".to_string()
        },
        error_count,
        warn_count,
        info_count,
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic_secrets_lib::SecretsStore;
    use std::io::Read;
    use std::io::Write;
    use zip::write::{FileOptions, ZipWriter};

    fn valid_machine() -> SetupMachine {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "messaging-teams-default",
            "entry_step": "collect",
            "steps": [
                {
                    "id": "collect",
                    "kind": "qa_form",
                    "on_success": "login"
                },
                {
                    "id": "login",
                    "kind": "oauth_device_code",
                    "device_code_url": "https://login.example.com/device",
                    "token_url": "https://login.example.com/token",
                    "client_id": "client-123",
                    "scopes": ["User.Read"],
                    "recover": [
                        {"when": "refresh_token_invalid", "action": "reauthorize"}
                    ],
                    "on_success": "complete"
                }
            ]
        }))
        .unwrap()
    }

    fn serve_one_json(status: u16, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 2048];
            let _ = stream.read(&mut buffer);
            let wire = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(wire.as_bytes()).unwrap();
        });
        format!("http://{addr}")
    }

    fn serve_json_sequence(responses: Vec<(u16, &'static str)>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0_u8; 4096];
                let _ = stream.read(&mut buffer);
                let wire = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(wire.as_bytes()).unwrap();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn validates_valid_machine() {
        let diagnostics = validate_setup_machine(&valid_machine());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn rejects_missing_transition_target_and_unreachable_step() {
        let mut machine = valid_machine();
        machine.steps[0].on_success = Some("missing".to_string());
        machine.steps.push(SetupMachineStep {
            id: "never".to_string(),
            title: None,
            kind: "qa_form".to_string(),
            requires: vec![],
            outputs: vec![],
            on_success: None,
            on_failure: vec![],
            recover: vec![],
            extra: serde_json::Map::new(),
        });

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.step.on_success"));
        assert!(ids.contains("setup.machine.step.reachable"));
    }

    #[test]
    fn rejects_unbounded_setup_machine_cycles() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "cycle-machine",
            "entry_step": "a",
            "steps": [
                {"id": "a", "kind": "qa_form", "on_success": "b"},
                {"id": "b", "kind": "manual_action", "on_success": "a"}
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.step.cycle"));
    }

    #[test]
    fn allows_bounded_setup_machine_cycles() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "cycle-machine",
            "entry_step": "a",
            "steps": [
                {"id": "a", "kind": "qa_form", "on_success": "b"},
                {
                    "id": "b",
                    "kind": "manual_action",
                    "retry": {"max_attempts": 3},
                    "on_success": "a"
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(!ids.contains("setup.machine.step.cycle"));
    }

    #[test]
    fn rejects_unsafe_setup_machine_artifact_paths() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "artifact-machine",
            "entry_step": "generate",
            "steps": [
                {"id": "generate", "kind": "generate_file", "artifact_path": "../secret.json"},
                {"id": "download", "kind": "download_file", "on_success": "complete"}
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.artifact.path.safe"));
        assert!(ids.contains("setup.machine.artifact.path"));
    }

    #[test]
    fn accepts_setup_machine_artifact_paths_under_artifacts() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "artifact-machine",
            "entry_step": "generate",
            "steps": [
                {
                    "id": "generate",
                    "kind": "generate_file",
                    "artifact_path": "artifacts/teams/app.zip",
                    "on_success": "download"
                },
                {
                    "id": "download",
                    "kind": "download_file",
                    "artifact_path": "artifacts/teams/app.zip",
                    "on_success": "complete"
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(!ids.contains("setup.machine.artifact.path"));
        assert!(!ids.contains("setup.machine.artifact.path.safe"));
    }

    #[test]
    fn rejects_invalid_setup_machine_http_steps() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "http-machine",
            "entry_step": "missing_url",
            "steps": [
                {"id": "missing_url", "kind": "http_probe", "on_success": "bad_url"},
                {
                    "id": "bad_url",
                    "kind": "http_json",
                    "url_template": "http://example.com/setup",
                    "method": "PUT",
                    "expected_status": 999
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.http.url"));
        assert!(ids.contains("setup.machine.http.url.safe"));
        assert!(ids.contains("setup.machine.http.method"));
        assert!(ids.contains("setup.machine.http.expected_status"));
        assert!(ids.contains("setup.machine.step.idempotency_key"));
    }

    #[test]
    fn rejects_invalid_setup_machine_tunnel_steps() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "tunnel-machine",
            "entry_step": "tunnel",
            "steps": [
                {
                    "id": "tunnel",
                    "kind": "cloudflare_tunnel",
                    "mode": "unsupported",
                    "public_base_url": "http://example.com"
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.cloudflare_tunnel.mode"));
        assert!(ids.contains("setup.machine.cloudflare_tunnel.public_base_url"));
    }

    #[test]
    fn rejects_invalid_setup_machine_oauth_authorization_steps() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "oauth-machine",
            "entry_step": "oauth",
            "steps": [
                {
                    "id": "oauth",
                    "kind": "oauth_authorization_code",
                    "authorize_url": "http://login.example.com/auth",
                    "token_url": "http://login.example.com/token",
                    "scopes": "User.Read",
                    "params": ["bad"],
                    "state_ttl_secs": 0
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.oauth_authorization_code.authorize_url.safe"));
        assert!(ids.contains("setup.machine.oauth_authorization_code.token_url.safe"));
        assert!(ids.contains("setup.machine.oauth_authorization_code.scopes"));
        assert!(ids.contains("setup.machine.oauth_authorization_code.params"));
        assert!(ids.contains("setup.machine.oauth_authorization_code.state_ttl_secs"));
    }

    #[test]
    fn rejects_invalid_setup_machine_oauth_device_steps() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "oauth-machine",
            "entry_step": "device",
            "steps": [
                {
                    "id": "device",
                    "kind": "oauth_device_code",
                    "device_code_url": "http://example.com/device",
                    "scopes": "User.Read"
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.oauth_device_code.endpoints"));
        assert!(ids.contains("setup.machine.oauth_device_code.url.safe"));
        assert!(ids.contains("setup.machine.oauth_device_code.client_id"));
        assert!(ids.contains("setup.machine.oauth_device_code.scopes"));
    }

    #[test]
    fn rejects_invalid_setup_machine_data_mapping_steps() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "mapping-machine",
            "entry_step": "select",
            "steps": [
                {
                    "id": "select",
                    "kind": "select_from_json",
                    "json_pointer": "value",
                    "on_success": "persist"
                },
                {
                    "id": "persist",
                    "kind": "persist_runtime_config",
                    "config": ["not", "object"],
                    "server_owned_config_keys": "graph_access_token"
                },
                {
                    "id": "component",
                    "kind": "provider_component_call",
                    "request": ["not", "object"]
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(ids.contains("setup.machine.select_from_json.source_output"));
        assert!(ids.contains("setup.machine.select_from_json.json_pointer"));
        assert!(ids.contains("setup.machine.persist_runtime_config.object"));
        assert!(ids.contains("setup.machine.persist_runtime_config.server_owned_config_keys"));
        assert!(ids.contains("setup.machine.provider_component_call.component_ref"));
        assert!(ids.contains("setup.machine.provider_component_call.op"));
        assert!(ids.contains("setup.machine.provider_component_call.request"));
        assert!(ids.contains("setup.machine.step.idempotency_key"));
    }

    #[test]
    fn accepts_valid_setup_machine_data_mapping_steps() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "mapping-machine",
            "entry_step": "select",
            "steps": [
                {
                    "id": "select",
                    "kind": "select_from_json",
                    "source_output": "teams",
                    "json_pointer": "/value",
                    "value_key": "id",
                    "on_success": "persist"
                },
                {
                    "id": "persist",
                    "kind": "persist_runtime_config",
                    "config": {"team_id": "{select}"},
                    "server_owned_config_keys": ["graph_access_token"],
                    "on_success": "component"
                },
                {
                    "id": "component",
                    "kind": "provider_component_call",
                    "component_ref": "messaging-example",
                    "op": "register",
                    "idempotency_key": "register:{select}",
                    "request": {"team_id": "{select}"}
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(!ids.contains("setup.machine.select_from_json.source_output"));
        assert!(!ids.contains("setup.machine.select_from_json.json_pointer"));
        assert!(!ids.contains("setup.machine.persist_runtime_config.input"));
        assert!(!ids.contains("setup.machine.persist_runtime_config.object"));
        assert!(!ids.contains("setup.machine.provider_component_call.component_ref"));
        assert!(!ids.contains("setup.machine.provider_component_call.op"));
        assert!(!ids.contains("setup.machine.provider_component_call.request"));
        assert!(!ids.contains("setup.machine.step.idempotency_key"));
    }

    #[test]
    fn rejects_persist_runtime_config_writing_server_owned_keys() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "mapping-machine",
            "entry_step": "persist",
            "server_owned_config_keys": ["tenant_secret"],
            "steps": [
                {
                    "id": "persist",
                    "kind": "persist_runtime_config",
                    "config": {
                        "graph_access_token": "{token}",
                        "team_id": "{team}"
                    },
                    "mappings": {
                        "tenant_secret": "{secret}"
                    },
                    "server_owned_config_keys": ["graph_access_token"]
                }
            ]
        }))
        .unwrap();

        let diagnostics = validate_setup_machine(&machine);
        let actual: BTreeSet<_> = diagnostics
            .iter()
            .filter(|diagnostic| {
                diagnostic.check_id == "setup.machine.persist_runtime_config.server_owned_output"
            })
            .filter_map(|diagnostic| diagnostic.actual.as_deref())
            .collect();
        assert!(actual.contains("persist: config.graph_access_token"));
        assert!(actual.contains("persist: mappings.tenant_secret"));
    }

    #[test]
    fn accepts_templated_setup_machine_http_steps() {
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "http-machine",
            "entry_step": "probe",
            "steps": [
                {
                    "id": "probe",
                    "kind": "http_probe",
                    "url_template": "{public_base_url}/health",
                    "method": "GET",
                    "expected_status": 204,
                    "on_success": "register"
                },
                {
                    "id": "register",
                    "kind": "http_json",
                    "url_template": "{public_base_url}/register",
                    "method": "POST",
                    "expected_status": 200,
                    "idempotency_key": "register:{tenant}",
                    "on_success": "complete"
                }
            ]
        }))
        .unwrap();

        let ids: BTreeSet<_> = validate_setup_machine(&machine)
            .into_iter()
            .map(|d| d.check_id)
            .collect();
        assert!(!ids.contains("setup.machine.http.url"));
        assert!(!ids.contains("setup.machine.http.url.safe"));
        assert!(!ids.contains("setup.machine.http.method"));
        assert!(!ids.contains("setup.machine.http.expected_status"));
        assert!(!ids.contains("setup.machine.step.idempotency_key"));
    }

    fn write_setup_machine_pack(
        pack: &Path,
        provider_id: &str,
        machine: &Value,
        setup_yaml: Option<&str>,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": provider_id,
                "extensions": {
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        if let Some(setup_yaml) = setup_yaml {
            writer.start_file("assets/setup.yaml", options)?;
            writer.write_all(setup_yaml.as_bytes())?;
        }
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(serde_json::to_string(machine)?.as_bytes())?;
        writer.finish()?;
        Ok(())
    }

    #[test]
    fn doctor_accepts_setup_machine_template_inputs_from_answers_and_outputs() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-example.gtpack");
        let machine = serde_json::json!({
            "version": 1,
            "id": "messaging-example-machine",
            "entry_step": "public_url",
            "steps": [
                {
                    "id": "public_url",
                    "kind": "platform_public_url",
                    "default": "https://example.test",
                    "on_success": "persist"
                },
                {
                    "id": "persist",
                    "kind": "persist_runtime_config",
                    "config": {
                        "bot_app_id": "{bot_app_id}",
                        "public_base_url": "{public_base_url}",
                        "tenant_id": "{tenant}",
                        "provider": "{provider_id}"
                    }
                }
            ]
        });
        write_setup_machine_pack(
            &pack,
            "messaging-example",
            &machine,
            Some("title: Example\nquestions:\n  - name: bot_app_id\n    required: true\n"),
        )?;

        let report = run_provider_setup_machine_doctor(&pack);
        assert_eq!(report.status, "ok");
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.check_id == "setup.machine.template.input")
        );
        Ok(())
    }

    #[test]
    fn doctor_rejects_setup_machine_unknown_template_input() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-example.gtpack");
        let machine = serde_json::json!({
            "version": 1,
            "id": "messaging-example-machine",
            "entry_step": "persist",
            "steps": [
                {
                    "id": "persist",
                    "kind": "persist_runtime_config",
                    "config": {
                        "bot_app_id": "{bot_ap_id}"
                    }
                }
            ]
        });
        write_setup_machine_pack(
            &pack,
            "messaging-example",
            &machine,
            Some("title: Example\nquestions:\n  - name: bot_app_id\n    required: true\n"),
        )?;

        let report = run_provider_setup_machine_doctor(&pack);
        assert_eq!(report.status, "error");
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.check_id == "setup.machine.template.input"
                && diagnostic
                    .actual
                    .as_deref()
                    .is_some_and(|actual| actual.contains("bot_ap_id"))
        }));
        Ok(())
    }

    #[test]
    fn loads_machine_from_pack_asset_extension() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(serde_json::to_string(&valid_machine())?.as_bytes())?;
        writer.finish()?;

        let machine = load_setup_machine_from_pack(&pack)?.expect("machine");
        assert_eq!(machine.id, "messaging-teams-default");
        let report = run_provider_setup_machine_doctor(&pack);
        assert_eq!(report.status, "ok");
        Ok(())
    }

    #[test]
    fn doctor_rejects_legacy_setup_actions_in_migrated_pack() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(serde_json::to_string(&valid_machine())?.as_bytes())?;
        writer.start_file("assets/setup.yaml", options)?;
        writer.write_all(
            br#"
title: Teams
questions: []
setup_actions:
  - id: legacy_install
    label: Legacy install
    kind: oauth_install_button
    authorize_url: https://login.example/install
"#,
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);

        assert_eq!(report.status, "error");
        assert!(report.diagnostics.iter().any(|d| {
            d.check_id == "setup.legacy_setup_actions.absent"
                && d.severity == DiagnosticSeverity::Error
        }));
        Ok(())
    }

    #[test]
    fn doctor_rejects_missing_setup_i18n_message_key() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    },
                    SETUP_I18N_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_I18N_EXTENSION,
                            "provider_id": "messaging-teams",
                            "messages": {
                                "setup.present": "Present"
                            }
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(
            serde_json::json!({
                "version": 1,
                "id": "messaging-teams-default",
                "entry_step": "approve",
                "steps": [
                    {
                        "id": "approve",
                        "kind": "manual_action",
                        "message_key": "setup.missing"
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);

        assert_eq!(report.status, "error");
        assert!(report.diagnostics.iter().any(|d| {
            d.check_id == "setup.i18n.message_key.exists"
                && d.actual.as_deref() == Some("setup.missing")
        }));
        Ok(())
    }

    #[test]
    fn doctor_accepts_setup_i18n_message_key_from_asset() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    },
                    SETUP_I18N_EXTENSION: {
                        "path": "assets/setup/i18n/en.json"
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(
            serde_json::json!({
                "version": 1,
                "id": "messaging-teams-default",
                "entry_step": "approve",
                "steps": [
                    {
                        "id": "approve",
                        "kind": "manual_action",
                        "on_failure": [
                            {
                                "when": "manual_action_required",
                                "action": "prompt",
                                "message_key": "setup.approve"
                            }
                        ]
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/i18n/en.json", options)?;
        writer.write_all(
            serde_json::json!({
                "messages": {
                    "setup.approve": "Approve setup"
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);

        assert_eq!(report.status, "ok", "{report:#?}");
        Ok(())
    }

    #[test]
    fn doctor_accepts_setup_machine_fixture_state() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(
            serde_json::json!({
                "version": 1,
                "id": "messaging-teams-default",
                "entry_step": "start",
                "fixture_paths": ["assets/setup/fixtures/paused.json"],
                "steps": [
                    {
                        "id": "start",
                        "kind": "manual_action",
                        "on_success": "complete"
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/fixtures/paused.json", options)?;
        writer.write_all(
            serde_json::json!({
                "state": {
                    "schema_version": 1,
                    "provider_id": "messaging-teams",
                    "tenant": "demo",
                    "team": "default",
                    "machine_id": "messaging-teams-default",
                    "machine_version": 1,
                    "status": "paused",
                    "current_step": "start",
                    "completed_steps": [],
                    "outputs": {},
                    "created_resources": []
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);

        assert_eq!(report.status, "ok", "{report:#?}");
        Ok(())
    }

    #[test]
    fn doctor_rejects_setup_machine_fixture_with_missing_step() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(
            serde_json::json!({
                "version": 1,
                "id": "messaging-teams-default",
                "entry_step": "start",
                "fixtures": [{"path": "assets/setup/fixtures/bad.json"}],
                "steps": [
                    {
                        "id": "start",
                        "kind": "manual_action",
                        "on_success": "complete"
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/fixtures/bad.json", options)?;
        writer.write_all(
            serde_json::json!({
                "schema_version": 1,
                "provider_id": "messaging-teams",
                "tenant": "demo",
                "team": "default",
                "machine_id": "messaging-teams-default",
                "machine_version": 1,
                "status": "paused",
                "current_step": "missing",
                "completed_steps": ["start"],
                "outputs": {},
                "created_resources": []
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);

        assert_eq!(report.status, "error");
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.check_id == "setup.machine.fixture.current_step"
                && diagnostic.actual.as_deref() == Some("assets/setup/fixtures/bad.json: missing")
        }));
        Ok(())
    }

    #[test]
    fn doctor_accepts_backend_contract_without_setup_machine() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_WEB_COMPONENT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_WEB_COMPONENT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "tag_name": "greentic-teams-setup-v4",
                            "module_asset": "assets/setup/greentic-teams-setup.js"
                        }
                    },
                    SETUP_BACKEND_CONTRACT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "asset": "assets/setup/backend-contract.json"
                        }
                    },
                    HTTP_ROUTES_EXTENSION: {
                        "inline": {
                            "routes": [
                                {
                                    "pattern": "/v1/setup/messaging-teams/{tenant}/{team}/register",
                                    "methods": ["POST"],
                                    "setup_component_ref": "messaging-teams",
                                    "setup_op": "register"
                                }
                            ]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/greentic-teams-setup.js", options)?;
        writer.write_all(b"export default class GreenticTeamsSetup extends HTMLElement {}\n")?;
        writer.start_file("assets/setup/backend-contract.json", options)?;
        writer.write_all(
            serde_json::json!({
                "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                "provider_id": "messaging-teams",
                "server_owned_config_keys": [
                    "oauth_kind",
                    "oauth_device_code",
                    "oauth_user_code",
                    "graph_access_token"
                ],
                "required_order": ["graph_admin_consent", "register_endpoint", "first_message"],
                "actions": [
                    {
                        "id": "graph_admin_consent",
                        "executor": {
                            "kind": "oauth_device_code",
                            "authority_url_template": "https://login.microsoftonline.com/{authority_tenant}",
                            "client_id_config_key": "graph_setup_client_id",
                            "oauth_kind": "graph",
                            "token_store_key": "graph_access_token",
                            "scopes": ["https://graph.microsoft.com/User.Read"]
                        },
                        "completion": {"state_path": "oauth.graph.ok", "exists": true}
                    },
                    {
                        "id": "register_endpoint",
                        "requires": ["graph_admin_consent"],
                        "executor": {
                            "kind": "provider_http",
                            "path_template": "/v1/setup/messaging-teams/{tenant}/{team}/register"
                        },
                        "completion": {"state_path": "last_reconcile.ok", "equals": true}
                    },
                    {
                        "id": "first_message",
                        "requires": ["register_endpoint"],
                        "executor": {
                            "kind": "runtime_observation",
                            "source": "greentic-start",
                            "event": "bot_framework_activity_received",
                            "state_store_key": "last_activity"
                        },
                        "completion": {"state_path": "last_activity", "exists": true}
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);
        assert_eq!(report.status, "ok", "{report:#?}");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.check_id == "setup.backend_contract.extension.present")
        );
        Ok(())
    }

    #[test]
    fn doctor_rejects_invalid_setup_machine_even_with_backend_contract() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_WEB_COMPONENT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_WEB_COMPONENT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "tag_name": "greentic-teams-setup-v4",
                            "module_asset": "assets/setup/greentic-teams-setup.js"
                        }
                    },
                    SETUP_BACKEND_CONTRACT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "asset": "assets/setup/backend-contract.json"
                        }
                    },
                    SETUP_MACHINE_EXTENSION: {
                        "path": "assets/setup/setup-machine.v1.json",
                        "schema": SETUP_MACHINE_SCHEMA_URL
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/greentic-teams-setup.js", options)?;
        writer.write_all(b"export default class GreenticTeamsSetup extends HTMLElement {}\n")?;
        writer.start_file("assets/setup/backend-contract.json", options)?;
        writer.write_all(
            serde_json::json!({
                "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                "provider_id": "messaging-teams",
                "server_owned_config_keys": [
                    "oauth_kind",
                    "oauth_device_code",
                    "oauth_user_code",
                    "graph_access_token"
                ],
                "required_order": ["graph_admin_consent"],
                "actions": [
                    {
                        "id": "graph_admin_consent",
                        "executor": {
                            "kind": "oauth_device_code",
                            "authority_url_template": "https://login.microsoftonline.com/{authority_tenant}",
                            "client_id_config_key": "graph_setup_client_id",
                            "oauth_kind": "graph",
                            "token_store_key": "graph_access_token",
                            "scopes": ["https://graph.microsoft.com/User.Read"]
                        },
                        "completion": {"state_path": "oauth.graph.ok", "exists": true}
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/setup-machine.v1.json", options)?;
        writer.write_all(
            serde_json::json!({
                "version": 1,
                "id": "messaging-teams-broken",
                "entry_step": "missing",
                "steps": [
                    {
                        "id": "only",
                        "kind": "auto_complete",
                        "on_success": "complete"
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);

        assert_eq!(report.status, "error");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.check_id == "setup.backend_contract.extension.present")
        );
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.check_id == "setup.machine.extension.present")
        );
        assert!(report.diagnostics.iter().any(|d| {
            d.severity == DiagnosticSeverity::Error && d.check_id.starts_with("setup.machine.")
        }));
        Ok(())
    }

    #[test]
    fn route_pattern_matches_provider_http_templates() {
        assert!(route_pattern_covers_path(
            "/v1/setup/messaging-teams/{tenant}/{team}/register",
            "/v1/setup/messaging-teams/{tenant}/{team}/register",
        ));
        assert!(route_pattern_covers_path(
            "/v1/messaging/setup/messaging-teams/{tenant}/{action*}",
            "/v1/messaging/setup/messaging-teams/{tenant}/oauth/{kind}/start",
        ));
        assert!(!route_pattern_covers_path(
            "/v1/setup/messaging-teams/{tenant}/{team}/publish",
            "/v1/setup/messaging-teams/{tenant}/{team}/register",
        ));
    }

    #[test]
    fn backend_contract_doctor_rejects_uncovered_provider_http_route() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_WEB_COMPONENT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_WEB_COMPONENT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "tag_name": "greentic-teams-setup-v4",
                            "module_asset": "assets/setup/greentic-teams-setup.js"
                        }
                    },
                    SETUP_BACKEND_CONTRACT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "asset": "assets/setup/backend-contract.json"
                        }
                    },
                    HTTP_ROUTES_EXTENSION: {
                        "inline": {
                            "routes": [
                                {
                                    "pattern": "/v1/setup/messaging-teams/{tenant}/{team}/other",
                                    "methods": ["POST"],
                                    "setup_component_ref": "messaging-teams",
                                    "setup_op": "other"
                                }
                            ]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/greentic-teams-setup.js", options)?;
        writer.write_all(b"export default class GreenticTeamsSetup extends HTMLElement {}\n")?;
        writer.start_file("assets/setup/backend-contract.json", options)?;
        writer.write_all(
            serde_json::json!({
                "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                "provider_id": "messaging-teams",
                "required_order": ["register_endpoint"],
                "actions": [
                    {
                        "id": "register_endpoint",
                        "executor": {
                            "kind": "provider_http",
                            "path_template": "/v1/setup/messaging-teams/{tenant}/{team}/register"
                        },
                        "completion": {"state_path": "last_reconcile.ok", "equals": true}
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);
        assert_eq!(report.status, "error");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| { d.check_id == "setup.backend_contract.provider_http.route_covered" })
        );
        Ok(())
    }

    #[test]
    fn backend_contract_doctor_rejects_provider_http_method_mismatch() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_WEB_COMPONENT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_WEB_COMPONENT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "tag_name": "greentic-teams-setup-v4",
                            "module_asset": "assets/setup/greentic-teams-setup.js"
                        }
                    },
                    SETUP_BACKEND_CONTRACT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "asset": "assets/setup/backend-contract.json"
                        }
                    },
                    HTTP_ROUTES_EXTENSION: {
                        "inline": {
                            "routes": [
                                {
                                    "pattern": "/v1/setup/messaging-teams/{tenant}/{team}/register",
                                    "methods": ["GET"],
                                    "setup_component_ref": "messaging-teams",
                                    "setup_op": "register"
                                }
                            ]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/greentic-teams-setup.js", options)?;
        writer.write_all(b"export default class GreenticTeamsSetup extends HTMLElement {}\n")?;
        writer.start_file("assets/setup/backend-contract.json", options)?;
        writer.write_all(
            serde_json::json!({
                "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                "provider_id": "messaging-teams",
                "required_order": ["register_endpoint"],
                "actions": [
                    {
                        "id": "register_endpoint",
                        "executor": {
                            "kind": "provider_http",
                            "method": "POST",
                            "path_template": "/v1/setup/messaging-teams/{tenant}/{team}/register"
                        },
                        "completion": {"state_path": "last_reconcile.ok", "equals": true}
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);
        assert_eq!(report.status, "error");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.check_id == "setup.backend_contract.provider_http.route_method")
        );
        Ok(())
    }

    #[test]
    fn backend_contract_doctor_rejects_oauth_token_not_server_owned() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let pack = temp.path().join("messaging-teams.gtpack");
        let file = std::fs::File::create(&pack)?;
        let mut writer = ZipWriter::new(file);
        let options: FileOptions<'_, ()> =
            FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("pack.manifest.json", options)?;
        writer.write_all(
            serde_json::json!({
                "pack_id": "messaging-teams",
                "extensions": {
                    SETUP_WEB_COMPONENT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_WEB_COMPONENT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "tag_name": "greentic-teams-setup-v4",
                            "module_asset": "assets/setup/greentic-teams-setup.js"
                        }
                    },
                    SETUP_BACKEND_CONTRACT_EXTENSION: {
                        "inline": {
                            "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                            "provider_id": "messaging-teams",
                            "asset": "assets/setup/backend-contract.json"
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.start_file("assets/setup/greentic-teams-setup.js", options)?;
        writer.write_all(b"export default class GreenticTeamsSetup extends HTMLElement {}\n")?;
        writer.start_file("assets/setup/backend-contract.json", options)?;
        writer.write_all(
            serde_json::json!({
                "schema_id": SETUP_BACKEND_CONTRACT_EXTENSION,
                "provider_id": "messaging-teams",
                "server_owned_config_keys": ["oauth_kind", "oauth_device_code", "oauth_user_code"],
                "required_order": ["graph_admin_consent"],
                "actions": [
                    {
                        "id": "graph_admin_consent",
                        "executor": {
                            "kind": "oauth_device_code",
                            "authority_url_template": "https://login.microsoftonline.com/{authority_tenant}",
                            "client_id_config_key": "graph_setup_client_id",
                            "oauth_kind": "graph",
                            "token_store_key": "graph_access_token",
                            "scopes": ["https://graph.microsoft.com/User.Read"]
                        },
                        "completion": {"state_path": "oauth.graph.ok", "exists": true}
                    }
                ]
            })
            .to_string()
            .as_bytes(),
        )?;
        writer.finish()?;

        let report = run_provider_setup_machine_doctor(&pack);
        assert_eq!(report.status, "error");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| { d.check_id == "setup.backend_contract.server_owned_config_keys.oauth" })
        );
        Ok(())
    }

    #[test]
    fn persists_state_and_appends_events() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-teams".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "messaging-teams-default".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Running,
            current_step: Some("login".to_string()),
            completed_steps: vec!["collect".to_string()],
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({}),
            created_resources: vec![],
            last_error: None,
            updated_at: None,
        };
        let path = write_setup_machine_state(temp.path(), &state)?;
        let loaded = load_setup_machine_state(&path)?;
        assert_eq!(loaded.current_step.as_deref(), Some("login"));

        let event = SetupMachineEvent {
            ts: "2026-06-17T00:00:00Z".to_string(),
            level: "info".to_string(),
            event: "setup.step.started".to_string(),
            provider_id: "messaging-teams".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "messaging-teams-default".to_string(),
            step_id: Some("login".to_string()),
            correlation_id: Some("corr".to_string()),
            fields: serde_json::Map::new(),
        };
        let event_path = append_setup_machine_event(temp.path(), &event)?;
        let events = std::fs::read_to_string(event_path)?;
        assert!(events.contains("setup.step.started"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_blocks_incompatible_persisted_machine_state() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine = valid_machine();
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-teams".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: machine.id.clone(),
            machine_version: 0,
            status: SetupMachineStatus::Running,
            current_step: Some(machine.entry_step.clone()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({}),
            created_resources: Vec::new(),
            last_error: None,
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-teams",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert!(
            output["next"]
                .as_str()
                .unwrap()
                .contains("machine_identity_changed")
        );
        let loaded = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
        ))?;
        assert_eq!(loaded.status, SetupMachineStatus::Failed);
        assert_eq!(
            loaded
                .last_error
                .as_ref()
                .and_then(|error| error.get("error"))
                .and_then(Value::as_str),
            Some("machine_identity_changed")
        );
        let events = std::fs::read_to_string(setup_machine_events_path(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
        ))?;
        assert!(events.contains("setup.machine.incompatible_state"));
        Ok(())
    }

    #[test]
    fn setup_machine_state_records_pack_and_answers_hashes() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine = valid_machine();
        let pack_path = temp.path().join("provider.gtpack");
        std::fs::write(&pack_path, b"pack-v1")?;
        let answers_path = temp
            .path()
            .join("state/config/messaging-teams/setup-answers.json");
        std::fs::create_dir_all(answers_path.parent().unwrap())?;
        std::fs::write(&answers_path, br#"{"client_id":"demo"}"#)?;

        let state = load_or_init_setup_machine_state_with_context(
            temp.path(),
            Some(&pack_path),
            "messaging-teams",
            "demo",
            "default",
            &machine,
        )?;

        assert!(state.pack_fingerprint.as_deref().is_some_and(|hash| {
            hash.starts_with("sha256:") && hash == setup_machine_hash_file(&pack_path).unwrap()
        }));
        assert!(state.answers_hash.as_deref().is_some_and(|hash| {
            hash.starts_with("sha256:") && hash == setup_machine_hash_file(&answers_path).unwrap()
        }));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_blocks_changed_provider_pack_fingerprint() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine = valid_machine();
        let pack_path = temp.path().join("provider.gtpack");
        std::fs::write(&pack_path, b"pack-v1")?;
        let mut state = load_or_init_setup_machine_state_with_context(
            temp.path(),
            Some(&pack_path),
            "messaging-teams",
            "demo",
            "default",
            &machine,
        )?;
        state.status = SetupMachineStatus::Running;
        write_setup_machine_state(temp.path(), &state)?;
        std::fs::write(&pack_path, b"pack-v2")?;

        let output = advance_setup_machine_with_pack(
            temp.path(),
            Some(&pack_path),
            "messaging-teams",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert!(
            output["next"]
                .as_str()
                .unwrap()
                .contains("pack_fingerprint_changed")
        );
        let loaded = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
        ))?;
        assert_eq!(
            loaded
                .last_error
                .as_ref()
                .and_then(|error| error.get("error"))
                .and_then(Value::as_str),
            Some("pack_fingerprint_changed")
        );
        Ok(())
    }

    #[test]
    fn advance_setup_machine_blocks_changed_setup_answers_hash() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine = valid_machine();
        let pack_path = temp.path().join("provider.gtpack");
        std::fs::write(&pack_path, b"pack-v1")?;
        let answers_path = temp
            .path()
            .join("state/config/messaging-teams/setup-answers.json");
        std::fs::create_dir_all(answers_path.parent().unwrap())?;
        std::fs::write(&answers_path, br#"{"client_id":"demo"}"#)?;
        let mut state = load_or_init_setup_machine_state_with_context(
            temp.path(),
            Some(&pack_path),
            "messaging-teams",
            "demo",
            "default",
            &machine,
        )?;
        state.status = SetupMachineStatus::Running;
        write_setup_machine_state(temp.path(), &state)?;
        std::fs::write(&answers_path, br#"{"client_id":"changed"}"#)?;

        let output = advance_setup_machine_with_pack(
            temp.path(),
            Some(&pack_path),
            "messaging-teams",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert!(
            output["next"]
                .as_str()
                .unwrap()
                .contains("setup_answers_changed")
        );
        let loaded = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-teams",
        ))?;
        assert_eq!(
            loaded
                .last_error
                .as_ref()
                .and_then(|error| error.get("error"))
                .and_then(Value::as_str),
            Some("setup_answers_changed")
        );
        Ok(())
    }

    #[test]
    fn advance_setup_machine_completes_auto_step_and_persists_state() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "collect",
            "steps": [
                {
                    "id": "collect",
                    "kind": "qa_form",
                    "auto_complete": true,
                    "on_success": "manual"
                },
                {
                    "id": "manual",
                    "kind": "manual_action",
                    "title": "Approve"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], true);
        assert_eq!(output["step"], "collect");
        assert_eq!(output["status"]["current_step"], "manual");
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Running);
        assert_eq!(state.completed_steps, vec!["collect"]);
        assert_eq!(state.current_step.as_deref(), Some("manual"));
        let events = std::fs::read_to_string(setup_machine_events_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert!(events.contains("setup.step.completed"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_resumes_current_step_and_pauses_for_manual_action()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "manual",
            "steps": [
                {
                    "id": "manual",
                    "kind": "manual_action",
                    "title": "Approve"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["step"], "manual");
        assert_eq!(output["result"]["status"], "paused");
        assert_eq!(
            output["result"]["detail"]["error"],
            "manual_action_required"
        );
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("manual"));

        let repeated = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            true,
        )?;
        assert_eq!(repeated["step"], "manual");
        assert_eq!(repeated["dry_run"], true);
        Ok(())
    }

    #[test]
    fn advance_setup_machine_follows_declared_failure_recovery_step() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "public_url",
            "steps": [
                {
                    "id": "public_url",
                    "kind": "platform_public_url",
                    "on_failure": [
                        {
                            "when": "missing_prerequisite",
                            "target": "manual_public_url"
                        }
                    ],
                    "on_success": "complete"
                },
                {
                    "id": "manual_public_url",
                    "kind": "manual_action",
                    "on_success": "public_url"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["next"], "recovery scheduled: manual_public_url");
        assert_eq!(
            output["result"]["detail"]["recovery_step"],
            "manual_public_url"
        );
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Running);
        assert_eq!(state.current_step.as_deref(), Some("manual_public_url"));
        assert_eq!(
            state
                .last_error
                .as_ref()
                .and_then(|error| error.get("recovery_step"))
                .and_then(Value::as_str),
            Some("manual_public_url")
        );
        let events = std::fs::read_to_string(setup_machine_events_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert!(events.contains("setup.step.recovery_scheduled"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_resolves_platform_public_url_from_static_routes() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        crate::platform_setup::persist_static_routes_artifact(
            temp.path(),
            &crate::platform_setup::StaticRoutesPolicy {
                public_web_enabled: true,
                public_base_url: Some("https://runtime.example.com/base/".to_string()),
                public_surface_policy: "enabled".to_string(),
                ..Default::default()
            },
        )?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "public_url",
            "steps": [
                {
                    "id": "public_url",
                    "kind": "platform_public_url",
                    "on_success": "complete"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], true);
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(
            state.outputs["public_base_url"],
            "https://runtime.example.com/base"
        );
        Ok(())
    }

    #[test]
    fn advance_setup_machine_blocks_platform_public_url_when_missing() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "public_url",
            "steps": [
                {
                    "id": "public_url",
                    "kind": "platform_public_url"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["result"]["detail"]["error"], "missing_prerequisite");
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("public_url"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_cloudflare_tunnel_resolves_existing_https_url() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "tunnel",
            "steps": [
                {
                    "id": "tunnel",
                    "kind": "cloudflare_tunnel",
                    "public_base_url": "https://setup.trycloudflare.com",
                    "on_success": "complete"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], true);
        assert_eq!(output["result"]["detail"]["ephemeral"], true);
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(
            state.outputs["public_base_url"],
            "https://setup.trycloudflare.com"
        );
        Ok(())
    }

    #[test]
    fn advance_setup_machine_cloudflare_tunnel_blocks_when_missing() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "tunnel",
            "steps": [
                {
                    "id": "tunnel",
                    "kind": "cloudflare_tunnel"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["result"]["detail"]["error"], "tunnel_unreachable");
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("tunnel"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_oauth_authorization_code_renders_signed_url() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Running,
            current_step: Some("oauth".to_string()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({
                "public_base_url": "https://runtime.example.com",
                "client_id": "client-123"
            }),
            created_resources: Vec::new(),
            last_error: None,
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "oauth",
            "steps": [
                {
                    "id": "oauth",
                    "kind": "oauth_authorization_code",
                    "authorize_url": "https://login.example.com/oauth2/v2.0/authorize",
                    "token_url": "https://login.example.com/oauth2/v2.0/token",
                    "callback_path": "/oauth/callback",
                    "scopes": ["User.Read", "offline_access"],
                    "params": {"prompt": "consent"}
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(
            output["result"]["detail"]["reason"],
            "oauth_authorization_required"
        );
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        let authorization_url = state.outputs["authorization_url"]
            .as_str()
            .expect("authorization_url");
        let parsed = url::Url::parse(authorization_url)?;
        let params: std::collections::BTreeMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(
            params.get("client_id").map(|v| v.as_ref()),
            Some("client-123")
        );
        assert_eq!(
            params.get("redirect_uri").map(|v| v.as_ref()),
            Some("https://runtime.example.com/oauth/callback")
        );
        assert_eq!(
            params.get("scope").map(|v| v.as_ref()),
            Some("User.Read offline_access")
        );
        assert_eq!(params.get("prompt").map(|v| v.as_ref()), Some("consent"));
        let signed_state = params.get("state").expect("state");
        let key = crate::setup_actions::load_or_create_signing_key(temp.path())?;
        let payload = crate::setup_actions::validate_oauth_state(
            signed_state,
            &key,
            Some("messaging-example"),
            Some("demo"),
            Some("default"),
            crate::setup_actions::current_epoch_secs(),
        )?;
        assert_eq!(payload.action_id, "oauth");
        Ok(())
    }

    #[test]
    fn advance_setup_machine_oauth_authorization_code_blocks_without_public_url()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "oauth",
            "steps": [
                {
                    "id": "oauth",
                    "kind": "oauth_authorization_code",
                    "authorize_url": "https://login.example.com/oauth2/v2.0/authorize",
                    "token_url": "https://login.example.com/oauth2/v2.0/token",
                    "callback_path": "/oauth/callback"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["result"]["detail"]["error"], "missing_prerequisite");
        assert_eq!(
            output["result"]["detail"]["missing_config_key"],
            "public_base_url"
        );
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("oauth"));
        Ok(())
    }

    #[tokio::test]
    async fn complete_setup_machine_oauth_authorization_code_persists_tokens_as_secrets()
    -> anyhow::Result<()> {
        let _store_iso_dir = tempfile::tempdir().expect("store isolation dir");
        let _store_iso = crate::secrets::test_support::StoreOverride::in_dir(_store_iso_dir.path());
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "oauth",
            "steps": [
                {
                    "id": "oauth",
                    "kind": "oauth_authorization_code",
                    "authorize_url": "https://login.example.com/oauth2/v2.0/authorize",
                    "token_url": "https://login.example.com/oauth2/v2.0/token",
                    "token_store_key": "EXAMPLE_ACCESS_TOKEN",
                    "refresh_token_store_key": "EXAMPLE_REFRESH_TOKEN",
                    "id_token_store_key": "EXAMPLE_ID_TOKEN",
                    "output_key": "oauth_result",
                    "on_success": "complete"
                }
            ]
        }))?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Paused,
            current_step: Some("oauth".to_string()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({}),
            created_resources: Vec::new(),
            last_error: Some(serde_json::json!({
                "error": "manual_action_required",
                "reason": "oauth_authorization_required",
            })),
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;
        let oauth_state = crate::setup_actions::OAuthStatePayload {
            provider_id: "messaging-example".into(),
            tenant: "demo".into(),
            team: "default".into(),
            action_id: "oauth".into(),
            nonce: "nonce".into(),
            expires_at: crate::setup_actions::current_epoch_secs() + 60,
        };

        let mut report = complete_setup_machine_oauth_authorization_code_for_machine(
            temp.path(),
            "dev",
            None,
            &machine,
            &oauth_state,
            &serde_json::json!({
                "access_token": "access-secret",
                "refresh_token": "refresh-secret",
                "id_token": "id-secret",
            }),
        )
        .await?;

        report.persisted_secret_keys.sort();
        assert_eq!(
            report.persisted_secret_keys,
            vec![
                "EXAMPLE_ACCESS_TOKEN",
                "EXAMPLE_ID_TOKEN",
                "EXAMPLE_REFRESH_TOKEN"
            ]
        );
        let state_path =
            setup_machine_state_path(temp.path(), "demo", "default", "messaging-example");
        let stored_state = std::fs::read_to_string(&state_path)?;
        assert!(!stored_state.contains("access-secret"));
        assert!(!stored_state.contains("refresh-secret"));
        assert!(!stored_state.contains("id-secret"));

        let state = load_setup_machine_state(&state_path)?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(state.outputs["oauth_result"]["ok"], true);

        let store = crate::secrets::open_dev_store_for_env(temp.path(), "dev")?;
        for (key, expected) in [
            ("EXAMPLE_ACCESS_TOKEN", "access-secret"),
            ("EXAMPLE_REFRESH_TOKEN", "refresh-secret"),
            ("EXAMPLE_ID_TOKEN", "id-secret"),
        ] {
            let uri = crate::canonical_secret_uri(
                "dev",
                "demo",
                Some("default"),
                "messaging-example",
                key,
            );
            let bytes = store.get(&uri).await?;
            assert_eq!(String::from_utf8(bytes)?, expected);
        }
        Ok(())
    }

    #[test]
    fn advance_setup_machine_oauth_device_code_starts_then_polls_complete() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let base = serve_json_sequence(vec![
            (
                200,
                r#"{"device_code":"raw-device-code","user_code":"ABCD-EFGH","verification_uri":"https://login.example.com/device","expires_in":900,"interval":1}"#,
            ),
            (
                200,
                r#"{"access_token":"access-token-secret","refresh_token":"refresh-token-secret"}"#,
            ),
        ]);
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "device",
            "steps": [
                {
                    "id": "device",
                    "kind": "oauth_device_code",
                    "device_code_url": format!("{base}/device"),
                    "token_url": format!("{base}/token"),
                    "client_id": "client-123",
                    "scopes": ["User.Read"],
                    "token_store_key": "graph_access_token",
                    "refresh_token_store_key": "graph_refresh_token",
                    "on_success": "complete"
                }
            ]
        }))?;

        let started = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(started["ok"], false);
        assert_eq!(
            started["result"]["detail"]["reason"],
            "oauth_device_code_pending"
        );
        let state_path =
            setup_machine_state_path(temp.path(), "demo", "default", "messaging-example");
        let raw_state = std::fs::read_to_string(&state_path)?;
        assert!(!raw_state.contains("raw-device-code"));
        assert!(!raw_state.contains("access-token-secret"));
        let state = load_setup_machine_state(&state_path)?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(
            state.outputs["oauth_device_login"]["user_code"],
            "ABCD-EFGH"
        );
        assert!(setup_machine_oauth_device_session_path(temp.path(), &state, "device").is_file());

        let completed = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(completed["ok"], true);
        let state = load_setup_machine_state(&state_path)?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(
            state.outputs["oauth_device_login"]["token_store_key"],
            "graph_access_token"
        );
        assert!(!setup_machine_oauth_device_session_path(temp.path(), &state, "device").exists());
        let raw_state = std::fs::read_to_string(&state_path)?;
        assert!(!raw_state.contains("access-token-secret"));
        assert!(!raw_state.contains("refresh-token-secret"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_oauth_device_code_keeps_pending_resumable() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let base = serve_json_sequence(vec![
            (
                200,
                r#"{"device_code":"raw-device-code","user_code":"ABCD-EFGH","verification_uri":"https://login.example.com/device","expires_in":900,"interval":1}"#,
            ),
            (200, r#"{"error":"authorization_pending"}"#),
        ]);
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "device",
            "steps": [
                {
                    "id": "device",
                    "kind": "oauth_device_code",
                    "device_code_url": format!("{base}/device"),
                    "token_url": format!("{base}/token"),
                    "client_id": "client-123",
                    "scope": "User.Read",
                    "token_store_key": "graph_access_token",
                    "on_success": "complete"
                }
            ]
        }))?;

        let _ = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;
        let pending = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(pending["ok"], false);
        assert_eq!(pending["result"]["detail"]["error"], "oauth_pending");
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("device"));
        assert!(setup_machine_oauth_device_session_path(temp.path(), &state, "device").is_file());
        Ok(())
    }

    #[test]
    fn advance_setup_machine_http_json_stores_response_and_advances() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let url = serve_one_json(200, r#"{"registered":true,"tenant":"demo"}"#);
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "register",
            "steps": [
                {
                    "id": "register",
                    "kind": "http_json",
                    "url_template": url,
                    "output_key": "registration",
                    "on_success": "complete"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], true);
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(state.outputs["registration"]["registered"], true);
        assert_eq!(state.outputs["registration"]["tenant"], "demo");
        Ok(())
    }

    #[test]
    fn advance_setup_machine_http_probe_blocks_on_unexpected_status() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let url = serve_one_json(503, r#"{"ok":false}"#);
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "probe",
            "steps": [
                {
                    "id": "probe",
                    "kind": "http_probe",
                    "url_template": url,
                    "expected_status": 200
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(
            output["result"]["detail"]["error"],
            "provider_contract_error"
        );
        assert_eq!(output["result"]["detail"]["status"], 503);
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("probe"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_select_from_json_auto_selects_single_option() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "select_team",
            "steps": [
                {
                    "id": "select_team",
                    "kind": "select_from_json",
                    "source_output": "teams_response",
                    "json_pointer": "/value",
                    "value_key": "id",
                    "output_key": "team_id",
                    "on_success": "complete"
                }
            ]
        }))?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Running,
            current_step: Some("select_team".to_string()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({
                "teams_response": {
                    "value": [
                        {"id": "team-123", "displayName": "Engineering"}
                    ]
                }
            }),
            created_resources: Vec::new(),
            last_error: None,
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], true);
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(state.outputs["team_id"], "team-123");
        assert_eq!(state.outputs["team_id_item"]["displayName"], "Engineering");
        Ok(())
    }

    #[test]
    fn advance_setup_machine_select_from_json_pauses_for_multiple_options() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "select_team",
            "steps": [
                {
                    "id": "select_team",
                    "kind": "select_from_json",
                    "source_output": "teams_response",
                    "json_pointer": "/value",
                    "value_key": "id",
                    "output_key": "team_id"
                }
            ]
        }))?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Running,
            current_step: Some("select_team".to_string()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({
                "teams_response": {
                    "value": [
                        {"id": "team-123", "displayName": "Engineering"},
                        {"id": "team-456", "displayName": "Support"}
                    ]
                }
            }),
            created_resources: Vec::new(),
            last_error: None,
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["result"]["detail"]["reason"], "selection_required");
        assert_eq!(output["result"]["detail"]["options"], 2);
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("select_team"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_generate_file_writes_artifact_and_records_resource()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "generate",
            "steps": [
                {
                    "id": "generate",
                    "kind": "generate_file",
                    "artifact_path": "artifacts/app/config.json",
                    "output_key": "config_artifact",
                    "content": {
                        "tenant": "{tenant}",
                        "team": "{team}",
                        "provider": "{provider_id}",
                        "public_base_url": "{public_base_url}"
                    },
                    "on_success": "complete"
                }
            ]
        }))?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Running,
            current_step: Some("generate".to_string()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({
                "public_base_url": "https://runtime.example.com"
            }),
            created_resources: Vec::new(),
            last_error: None,
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], true);
        let artifact_path =
            setup_machine_state_dir(temp.path(), "demo", "default", "messaging-example")
                .join("artifacts/app/config.json");
        let artifact = std::fs::read_to_string(artifact_path)?;
        let artifact: Value = serde_json::from_str(&artifact)?;
        assert_eq!(artifact["tenant"], "demo");
        assert_eq!(artifact["team"], "default");
        assert_eq!(artifact["provider"], "messaging-example");
        assert_eq!(artifact["public_base_url"], "https://runtime.example.com");

        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(
            state.outputs["config_artifact"],
            "artifacts/app/config.json"
        );
        assert_eq!(
            state
                .created_resources
                .first()
                .and_then(|value| value.get("path")),
            Some(&Value::String("artifacts/app/config.json".to_string()))
        );
        Ok(())
    }

    #[test]
    fn advance_setup_machine_generate_file_blocks_without_content() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "generate",
            "steps": [
                {
                    "id": "generate",
                    "kind": "generate_file",
                    "artifact_path": "artifacts/app/config.json"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["result"]["detail"]["error"], "missing_prerequisite");
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("generate"));
        assert_eq!(
            state
                .last_error
                .as_ref()
                .and_then(|value| value.get("error")),
            Some(&Value::String("missing_prerequisite".to_string()))
        );
        Ok(())
    }

    #[test]
    fn advance_setup_machine_persist_runtime_config_writes_setup_answers_and_strips_server_owned()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "persist",
            "server_owned_config_keys": ["graph_access_token"],
            "steps": [
                {
                    "id": "persist",
                    "kind": "persist_runtime_config",
                    "config": {
                        "bot_app_id": "{bot_app_id}",
                        "team_id": "{team_id}",
                        "graph_access_token": "{graph_access_token}"
                    },
                    "on_success": "complete"
                }
            ]
        }))?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Running,
            current_step: Some("persist".to_string()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({
                "bot_app_id": "app-123",
                "team_id": "team-123",
                "graph_access_token": "secret-token"
            }),
            created_resources: Vec::new(),
            last_error: None,
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], true);
        assert_eq!(output["result"]["detail"]["envelope_skipped"], true);
        let config_path = temp
            .path()
            .join("state/config/messaging-example/setup-answers.json");
        let config: Value = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;
        assert_eq!(config["bot_app_id"], "app-123");
        assert_eq!(config["team_id"], "team-123");
        assert!(config.get("graph_access_token").is_none());
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Complete);
        assert_eq!(
            state
                .created_resources
                .first()
                .and_then(|value| value.get("kind")),
            Some(&Value::String("runtime_config".to_string()))
        );
        Ok(())
    }

    #[test]
    fn advance_setup_machine_persist_runtime_config_blocks_without_config() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "persist",
            "steps": [
                {
                    "id": "persist",
                    "kind": "persist_runtime_config"
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["result"]["detail"]["error"], "missing_prerequisite");
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("persist"));
        Ok(())
    }

    #[test]
    fn advance_setup_machine_provider_component_call_blocks_without_pack_path() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "register",
            "steps": [
                {
                    "id": "register",
                    "kind": "provider_component_call",
                    "component_ref": "messaging-example",
                    "op": "register",
                    "request": {"tenant": "{tenant}"}
                }
            ]
        }))?;

        let output = advance_setup_machine(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            false,
        )?;

        assert_eq!(output["ok"], false);
        assert_eq!(output["result"]["detail"]["error"], "missing_prerequisite");
        assert_eq!(
            output["result"]["detail"]["missing_config_key"],
            "pack_path"
        );
        let state = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(state.status, SetupMachineStatus::Paused);
        assert_eq!(state.current_step.as_deref(), Some("register"));
        Ok(())
    }

    #[test]
    fn retry_setup_machine_step_clears_paused_error_without_losing_completed_steps()
    -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "collect",
            "steps": [
                {"id": "collect", "kind": "qa_form", "on_success": "manual"},
                {"id": "manual", "kind": "manual_action"}
            ]
        }))?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Paused,
            current_step: Some("manual".to_string()),
            completed_steps: vec!["collect".to_string()],
            failed_step: Some("manual".to_string()),
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({}),
            created_resources: Vec::new(),
            last_error: Some(serde_json::json!({"error": "manual_action_required"})),
            updated_at: None,
        };
        write_setup_machine_state(temp.path(), &state)?;

        let output = retry_setup_machine_step(
            temp.path(),
            "messaging-example",
            "demo",
            "default",
            &machine,
            None,
        )?;

        assert_eq!(output["changed"], true);
        assert_eq!(output["step"], "manual");
        let loaded = load_setup_machine_state(&setup_machine_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert_eq!(loaded.status, SetupMachineStatus::Running);
        assert_eq!(loaded.current_step.as_deref(), Some("manual"));
        assert_eq!(loaded.completed_steps, vec!["collect"]);
        assert!(loaded.last_error.is_none());
        assert!(loaded.failed_step.is_none());
        Ok(())
    }

    #[test]
    fn reset_setup_machine_state_archives_and_removes_machine_json() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let state = SetupMachineState {
            schema_version: 1,
            provider_id: "messaging-example".to_string(),
            tenant: "demo".to_string(),
            team: "default".to_string(),
            machine_id: "example-machine".to_string(),
            machine_version: 1,
            status: SetupMachineStatus::Paused,
            current_step: Some("manual".to_string()),
            completed_steps: Vec::new(),
            failed_step: None,
            answers_hash: None,
            pack_fingerprint: None,
            outputs: serde_json::json!({}),
            created_resources: Vec::new(),
            last_error: None,
            updated_at: None,
        };
        let state_path = write_setup_machine_state(temp.path(), &state)?;

        let archive = reset_setup_machine_state(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
            "manual reset",
        )?
        .expect("archive");

        assert!(!state_path.exists());
        assert!(archive.is_file());
        let events = std::fs::read_to_string(setup_machine_events_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        ))?;
        assert!(events.contains("setup.machine.reset"));
        Ok(())
    }

    #[test]
    fn migrate_setup_machine_state_archives_legacy_artifacts_and_initializes_machine()
    -> anyhow::Result<()> {
        let _store_iso_dir = tempfile::tempdir().expect("store isolation dir");
        let _store_iso = crate::secrets::test_support::StoreOverride::in_dir(_store_iso_dir.path());
        let temp = tempfile::tempdir()?;
        let machine: SetupMachine = serde_json::from_value(serde_json::json!({
            "version": 1,
            "id": "example-machine",
            "entry_step": "start",
            "steps": [
                {
                    "id": "start",
                    "kind": "manual_action",
                    "on_success": "complete"
                }
            ]
        }))?;
        let legacy_backend = crate::setup_backend_contract::legacy_backend_state_path(
            temp.path(),
            "dev",
            "demo",
            "default",
            "messaging-example",
        )?;
        std::fs::create_dir_all(legacy_backend.parent().unwrap())?;
        std::fs::write(&legacy_backend, r#"{"config":{"bot_app_id":"old"}}"#)?;
        let legacy_actions = crate::setup_actions::setup_actions_state_path(
            temp.path(),
            "demo",
            "default",
            "messaging-example",
        );
        std::fs::create_dir_all(legacy_actions.parent().unwrap())?;
        std::fs::write(&legacy_actions, r#"{"actions":[]}"#)?;

        let migration = migrate_setup_machine_state(
            temp.path(),
            "dev",
            "demo",
            "default",
            "messaging-example",
            &machine,
        )?;

        assert_eq!(migration.source, "legacy-archived");
        assert!(migration.initialized);
        assert!(migration.legacy_backend_removed);
        assert!(migration.setup_actions_removed);
        assert!(!legacy_backend.exists());
        assert!(!legacy_actions.exists());
        assert!(migration.backend_archive_path.unwrap().is_file());
        assert!(migration.setup_actions_archive_path.unwrap().is_file());
        let state = load_setup_machine_state(&migration.state_path)?;
        assert_eq!(state.status, SetupMachineStatus::NotStarted);
        assert_eq!(state.current_step.as_deref(), Some("start"));
        assert!(migration.event_path.is_file());

        let repeated = migrate_setup_machine_state(
            temp.path(),
            "dev",
            "demo",
            "default",
            "messaging-example",
            &machine,
        )?;
        assert_eq!(repeated.source, "machine");
        assert!(!repeated.initialized);
        assert!(!repeated.legacy_backend_removed);
        assert!(!repeated.setup_actions_removed);
        assert!(!repeated.empty);
        Ok(())
    }
}
