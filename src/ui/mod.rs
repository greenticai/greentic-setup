//! Web-based setup UI server.
//!
//! Launches an Axum HTTP server on a random port, opens the browser, and serves
//! a single-page app that drives the setup wizard through the same FormSpec
//! infrastructure as the terminal wizard.

mod assets;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};
use tokio::sync::broadcast;

use crate::cli_i18n::CliI18n;
use crate::engine::{SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::platform_setup::StaticRoutesPolicy;
use crate::qa::wizard;
use crate::{SetupEngine, SetupMode, discovery, setup_to_formspec};

use crate::qa::shared_questions::HIDDEN_FROM_PROMPTS;

// ── Types ──

struct UiState {
    bundle_path: PathBuf,
    tenant: String,
    team: Option<String>,
    env: String,
    #[allow(dead_code)]
    advanced: bool,
    locale: Option<String>,
    /// Pre-loaded answers from `--answers` file, keyed by provider_id.
    prefill_answers: Option<JsonMap<String, Value>>,
    /// When true the tenant/env came from an answers file and should not be
    /// overridden by bundle auto-detection.
    scope_from_answers: bool,
    shutdown_tx: broadcast::Sender<()>,
    #[allow(dead_code)]
    result: Mutex<Option<ExecutionResult>>,
}

#[derive(Serialize)]
#[allow(dead_code)]
struct ProvidersResponse {
    bundle_path: String,
    providers: Vec<ProviderInfo>,
    provider_forms: Vec<ProviderForm>,
    shared_questions: Vec<QuestionInfo>,
}

#[derive(Serialize)]
struct ProviderInfo {
    provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    domain: String,
    question_count: usize,
}

#[derive(Serialize)]
struct ProviderForm {
    provider_id: String,
    title: String,
    questions: Vec<QuestionInfo>,
}

#[derive(Serialize, Clone)]
struct QuestionInfo {
    id: String,
    title: String,
    kind: String,
    required: bool,
    secret: bool,
    default_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_value: Option<String>,
    help: Option<String>,
    choices: Option<Vec<String>>,
    visible_if: Option<VisibleIfInfo>,
    placeholder: Option<String>,
    group: Option<String>,
    docs_url: Option<String>,
}

#[derive(Serialize, Clone)]
struct VisibleIfInfo {
    field: String,
    eq: Option<String>,
}

/// Extra fields from setup.yaml not in FormSpec.
struct SetupQuestionExtras {
    placeholder: Option<String>,
    group: Option<String>,
    docs_url: Option<String>,
}

#[derive(Deserialize)]
struct ExecuteRequest {
    answers: JsonMap<String, Value>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    tunnel: Option<String>,
}

#[derive(Deserialize)]
struct DraftSaveRequest {
    answers: JsonMap<String, Value>,
    tenant: String,
    #[serde(default)]
    team: Option<String>,
    env: String,
}

#[derive(Serialize)]
struct ScopeResponse {
    tenant: String,
    team: Option<String>,
    env: String,
    detected_tenant: Option<String>,
}

#[derive(Serialize, Clone)]
struct ExecutionResult {
    success: bool,
    stdout: String,
    stderr: String,
    manual_steps: Vec<crate::webhook::ProviderInstruction>,
}

// ── Public API ──

/// Launch the setup UI server and open in browser.
///
/// When `prefill_answers` is provided (from `--answers` file), the values are
/// injected into the UI as pre-filled form values so the user can review and
/// edit before executing.
#[allow(clippy::too_many_arguments)]
pub async fn launch(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    advanced: bool,
    locale: Option<&str>,
    prefill_answers: Option<JsonMap<String, Value>>,
    scope_from_answers: bool,
) -> Result<()> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let state = std::sync::Arc::new(UiState {
        bundle_path: bundle_path.to_path_buf(),
        tenant: tenant.to_string(),
        team: team.map(String::from),
        env: env.to_string(),
        advanced,
        locale: locale.map(String::from),
        prefill_answers,
        scope_from_answers,
        shutdown_tx: shutdown_tx.clone(),
        result: Mutex::new(None),
    });

    let router = build_router(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");

    eprintln!("Setup UI started at: {url}");
    let _ = open::that(&url);

    let mut shutdown_rx = shutdown_tx.subscribe();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.recv().await;
        })
        .await?;

    Ok(())
}

fn build_router(state: std::sync::Arc<UiState>) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/app.js", get(serve_js))
        .route("/style.css", get(serve_css))
        .route("/api/locales", get(get_locales))
        .route("/api/scope", get(get_scope))
        .route("/api/existing-scopes", get(get_existing_scopes))
        .route("/api/providers", get(get_providers))
        .route("/api/draft", post(post_draft))
        .route("/api/execute", post(post_execute))
        .route("/api/export", post(post_export))
        .route("/api/decrypt", post(post_decrypt))
        .route("/api/shutdown", post(post_shutdown))
        .with_state(state)
}

// ── Static assets ──

async fn serve_index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        assets::INDEX_HTML,
    )
}

async fn serve_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        assets::APP_JS,
    )
}

async fn serve_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        assets::STYLE_CSS,
    )
}

// ── API handlers ──

/// Well-known locales with display labels.
const LOCALE_OPTIONS: &[(&str, &str)] = &[
    ("en", "English"),
    ("id", "Bahasa Indonesia"),
    ("ja", "日本語"),
    ("zh", "中文"),
    ("ko", "한국어"),
    ("es", "Español"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("pt", "Português"),
    ("ru", "Русский"),
    ("ar", "العربية"),
    ("th", "ไทย"),
    ("vi", "Tiếng Việt"),
    ("tr", "Türkçe"),
    ("it", "Italiano"),
    ("nl", "Nederlands"),
    ("pl", "Polski"),
    ("sv", "Svenska"),
    ("hi", "हिन्दी"),
    ("ms", "Bahasa Melayu"),
];

async fn get_locales(State(state): State<std::sync::Arc<UiState>>) -> Json<Value> {
    let current = state.locale.as_deref().unwrap_or("en");
    let locales: Vec<Value> = LOCALE_OPTIONS
        .iter()
        .map(|(code, label)| {
            serde_json::json!({
                "code": code,
                "label": label,
                "selected": *code == current,
            })
        })
        .collect();
    Json(serde_json::json!({ "locales": locales, "current": current }))
}

#[derive(Deserialize)]
struct ProviderQuery {
    locale: Option<String>,
}

async fn get_scope(State(state): State<std::sync::Arc<UiState>>) -> Json<ScopeResponse> {
    let bundle_path = &state.bundle_path;
    let cli_tenant = &state.tenant;
    let cli_env = &state.env;

    // Detect tenant from the bundle's tenants/ directory for informational display.
    let detected_tenant = detect_tenant_from_bundle(bundle_path);

    // When the scope was explicitly provided via --answers, use it as-is
    // without overriding with bundle detection.
    let effective_tenant = if state.scope_from_answers {
        cli_tenant.clone()
    } else if cli_tenant == "demo" {
        // Apply same resolution logic as resolve_setup_scope_with_bundle:
        // if CLI tenant is the default "demo" and we detect a tenant from
        // the bundle, use it.
        detected_tenant
            .clone()
            .unwrap_or_else(|| cli_tenant.clone())
    } else {
        cli_tenant.clone()
    };

    Json(ScopeResponse {
        tenant: effective_tenant,
        team: state.team.clone(),
        env: cli_env.clone(),
        detected_tenant,
    })
}

/// Detect tenant from the bundle's `tenants/` directory.
fn detect_tenant_from_bundle(bundle_dir: &Path) -> Option<String> {
    let tenants_dir = bundle_dir.join("tenants");
    let entries: Vec<String> = std::fs::read_dir(&tenants_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();

    match entries.len() {
        0 => None,
        1 => Some(entries[0].clone()),
        _ => entries
            .iter()
            .find(|t| t.as_str() != "demo")
            .cloned()
            .or_else(|| entries.first().cloned()),
    }
}

/// Scan the bundle for previously configured scopes.
///
/// Reads `state/config/*/setup-answers.json` for provider answers and
/// probes the dev secrets store with detected tenants to reconstruct
/// existing scope configurations.
async fn get_existing_scopes(State(state): State<std::sync::Arc<UiState>>) -> Json<Value> {
    let bundle_path = &state.bundle_path;

    // 1. Detect tenants from tenants/ directory
    let tenants = {
        let mut t = Vec::new();
        let tenants_dir = bundle_path.join("tenants");
        if let Ok(entries) = std::fs::read_dir(&tenants_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir()
                    && let Some(name) = entry.file_name().to_str()
                {
                    t.push(name.to_string());
                }
            }
        }
        if t.is_empty() {
            t.push(state.tenant.clone());
        }
        t.sort();
        t
    };

    // 2. Read provider answers from state/config/*/setup-answers.json
    let config_dir = bundle_path.join("state").join("config");
    let mut provider_answers: JsonMap<String, Value> = JsonMap::new();
    if let Ok(entries) = std::fs::read_dir(&config_dir) {
        for entry in entries.flatten() {
            if !entry.path().is_dir() {
                continue;
            }
            let provider_id = entry.file_name().to_string_lossy().to_string();
            let answers_file = entry.path().join("setup-answers.json");
            if let Ok(content) = std::fs::read_to_string(&answers_file)
                && let Ok(parsed) = serde_json::from_str::<Value>(&content)
            {
                provider_answers.insert(provider_id, parsed);
            }
        }
    }

    // 3. For each tenant, probe secrets store to see if secrets exist
    let discovered = discovery::discover(bundle_path).ok();
    let provider_form_specs: Vec<wizard::ProviderFormSpec> = discovered
        .iter()
        .flat_map(|d| d.setup_targets())
        .filter_map(|p| {
            setup_to_formspec::pack_to_form_spec(&p.pack_path, &p.provider_id).map(|fs| {
                wizard::ProviderFormSpec {
                    provider_id: p.provider_id.clone(),
                    form_spec: fs,
                }
            })
        })
        .collect();

    let envs_to_probe = ["dev", "local"];
    let mut scopes = Vec::new();

    for tenant in &tenants {
        for env in &envs_to_probe {
            let saved =
                load_saved_secrets(bundle_path, env, tenant, None, &provider_form_specs).await;

            if saved.is_empty() {
                continue;
            }

            // Merge saved secrets with file-based answers
            let mut merged_answers = JsonMap::new();
            for (pid, file_ans) in &provider_answers {
                merged_answers.insert(pid.clone(), file_ans.clone());
            }
            // Overlay saved secrets into answers
            for (pid, secrets) in &saved {
                let entry = merged_answers
                    .entry(pid.clone())
                    .or_insert_with(|| Value::Object(JsonMap::new()));
                if let Some(obj) = entry.as_object_mut() {
                    for (k, v) in secrets {
                        obj.insert(k.clone(), Value::String(v.clone()));
                    }
                }
            }

            scopes.push(serde_json::json!({
                "tenant": tenant,
                "env": env,
                "team": null,
                "answers": merged_answers,
                "providers_done": saved.keys().collect::<Vec<_>>(),
            }));
            break; // found secrets for this tenant, skip other envs
        }
    }

    Json(serde_json::json!({ "scopes": scopes }))
}

async fn get_providers(
    State(state): State<std::sync::Arc<UiState>>,
    axum::extract::Query(query): axum::extract::Query<ProviderQuery>,
) -> Json<Value> {
    let bundle_path = &state.bundle_path;

    // Use query locale override, fall back to CLI locale
    let locale = query.locale.as_deref().or(state.locale.as_deref());

    // Load i18n strings for the UI
    let i18n = CliI18n::from_request(locale)
        .unwrap_or_else(|_| CliI18n::from_request(Some("en")).expect("en locale must exist"));
    let ui_strings = i18n.keys_with_prefix("ui.");

    let discovered = match discovery::discover(bundle_path) {
        Ok(d) => d,
        Err(e) => {
            return Json(serde_json::json!({
                "bundle_path": bundle_path.display().to_string(),
                "providers": [],
                "provider_forms": [],
                "shared_questions": [],
                "i18n": ui_strings,
                "error": e.to_string(),
            }));
        }
    };

    let setup_targets = discovered.setup_targets();

    let provider_form_specs: Vec<wizard::ProviderFormSpec> = setup_targets
        .iter()
        .filter_map(|provider| {
            setup_to_formspec::pack_to_form_spec(&provider.pack_path, &provider.provider_id).map(
                |form_spec| wizard::ProviderFormSpec {
                    provider_id: provider.provider_id.clone(),
                    form_spec,
                },
            )
        })
        .collect();

    // Detect shared questions (saved values injected after secrets are loaded below)
    let shared_question_specs = if provider_form_specs.len() > 1 {
        wizard::collect_shared_questions(&provider_form_specs)
            .shared_questions
            .clone()
    } else {
        vec![]
    };

    let providers: Vec<ProviderInfo> = setup_targets
        .iter()
        .map(|p| {
            let form = setup_to_formspec::pack_to_form_spec(&p.pack_path, &p.provider_id);
            ProviderInfo {
                provider_id: p.provider_id.clone(),
                display_name: p.display_name.clone(),
                domain: p.domain.clone(),
                question_count: form.as_ref().map(|f| f.questions.len()).unwrap_or(0),
            }
        })
        .collect();

    // Build lookup maps for extra fields (placeholder, group, docs_url) from setup.yaml
    let mut extras_by_provider: std::collections::HashMap<
        String,
        std::collections::HashMap<String, SetupQuestionExtras>,
    > = std::collections::HashMap::new();
    for provider in &setup_targets {
        if let Ok(Some(spec)) = crate::setup_input::load_setup_spec(&provider.pack_path) {
            let mut map = std::collections::HashMap::new();
            for q in &spec.questions {
                map.insert(
                    q.name.clone(),
                    SetupQuestionExtras {
                        placeholder: q.placeholder.clone(),
                        group: q.group.clone(),
                        docs_url: q.docs_url.clone(),
                    },
                );
            }
            extras_by_provider.insert(provider.provider_id.clone(), map);
        }
    }

    // Load saved secrets from dev store for auto-fill
    let saved_secrets = load_saved_secrets(
        bundle_path,
        &state.env,
        &state.tenant,
        state.team.as_deref(),
        &provider_form_specs,
    )
    .await;

    // Build per-provider prefill map from --answers file (overrides saved secrets)
    let prefill = &state.prefill_answers;

    // Inject saved values into shared questions (pick from first provider that has the value)
    // Answers from --answers file take priority over saved secrets.
    // Filter out questions that are auto-injected by the operator (e.g. public_base_url).
    let shared_questions: Vec<QuestionInfo> = shared_question_specs
        .iter()
        .filter(|q| !HIDDEN_FROM_PROMPTS.contains(&q.id.as_str()))
        .map(|q| {
            let mut info = form_question_to_info(q, Some(&i18n));
            // First try --answers prefill (check all providers for the shared question)
            let mut found = false;
            if let Some(answers) = prefill {
                for pfs in &provider_form_specs {
                    if let Some(provider_answers) =
                        answers.get(&pfs.provider_id).and_then(|v| v.as_object())
                        && let Some(val) = provider_answers
                            .get(&q.id)
                            .and_then(value_as_nonempty_string)
                    {
                        info.saved_value = Some(val);
                        found = true;
                        break;
                    }
                }
            }
            // Fall back to saved secrets
            if !found {
                for secrets in saved_secrets.values() {
                    if let Some(val) = secrets.get(&q.id) {
                        info.saved_value = Some(val.clone());
                        break;
                    }
                }
            }
            info
        })
        .collect();

    let provider_forms: Vec<ProviderForm> = provider_form_specs
        .iter()
        .map(|pfs| {
            let extras = extras_by_provider.get(&pfs.provider_id);
            let saved = saved_secrets.get(&pfs.provider_id);
            let answers = prefill
                .as_ref()
                .and_then(|a| a.get(&pfs.provider_id))
                .and_then(|v| v.as_object());
            ProviderForm {
                provider_id: pfs.provider_id.clone(),
                title: pfs.form_spec.title.clone(),
                questions: pfs
                    .form_spec
                    .questions
                    .iter()
                    .filter(|q| !HIDDEN_FROM_PROMPTS.contains(&q.id.as_str()))
                    .map(|q| {
                        let mut info = form_question_to_info(q, Some(&i18n));
                        if let Some(ext) = extras.and_then(|m| m.get(&q.id)) {
                            if info.placeholder.is_none() {
                                info.placeholder = ext.placeholder.clone();
                            }
                            info.group = ext.group.clone();
                            info.docs_url = ext.docs_url.clone();
                        }
                        // --answers prefill takes priority over saved secrets
                        if let Some(val) = answers
                            .and_then(|m| m.get(&q.id))
                            .and_then(value_as_nonempty_string)
                        {
                            info.saved_value = Some(val);
                        } else if let Some(val) = saved.and_then(|m| m.get(&q.id)) {
                            info.saved_value = Some(val.clone());
                        }
                        info
                    })
                    .collect(),
            }
        })
        .collect();

    Json(serde_json::json!({
        "bundle_path": bundle_path.display().to_string(),
        "providers": providers,
        "provider_forms": provider_forms,
        "shared_questions": shared_questions,
        "i18n": ui_strings,
    }))
}

async fn post_execute(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<ExecuteRequest>,
) -> Json<ExecutionResult> {
    let bundle_path = state.bundle_path.clone();
    // Use scope from UI request if provided, otherwise fall back to CLI defaults
    let tenant = req.tenant.unwrap_or_else(|| state.tenant.clone());
    let team = req.team.or_else(|| state.team.clone());
    let env = req.env.unwrap_or_else(|| state.env.clone());
    let answers = req.answers;

    // Persist tunnel config from the UI selection.
    if let Some(mode) = req.tunnel.as_deref() {
        let tunnel = crate::platform_setup::TunnelAnswers {
            mode: Some(mode.to_string()),
        };
        let _ = crate::platform_setup::persist_tunnel_artifact(&state.bundle_path, &tunnel);
    }

    let result = tokio::task::spawn_blocking(move || {
        execute_setup(&bundle_path, &tenant, team.as_deref(), &env, answers)
    })
    .await
    .unwrap_or_else(|e| ExecutionResult {
        success: false,
        stdout: String::new(),
        stderr: format!("Task panicked: {e}"),
        manual_steps: vec![],
    });

    *state.result.lock().unwrap() = Some(result.clone());
    Json(result)
}

async fn post_draft(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<DraftSaveRequest>,
) -> Json<Value> {
    match persist_ui_draft(
        &state.bundle_path,
        &req.tenant,
        req.team.as_deref(),
        &req.env,
        &req.answers,
    )
    .await
    {
        Ok(persisted) => Json(serde_json::json!({
            "ok": true,
            "persisted": persisted,
        })),
        Err(err) => Json(serde_json::json!({
            "ok": false,
            "error": err.to_string(),
        })),
    }
}

#[derive(Deserialize)]
struct ExportRequest {
    scopes: Vec<ExportScope>,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
struct ExportScope {
    tenant: String,
    #[serde(default)]
    team: Option<String>,
    env: String,
    answers: JsonMap<String, Value>,
}

async fn post_export(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<ExportRequest>,
) -> Json<Value> {
    let bundle_path = state.bundle_path.clone();

    // Discover packs to identify secret fields for encryption
    let discovered = discovery::discover(&bundle_path).ok();
    let secret_fields: std::collections::HashSet<String> = discovered
        .iter()
        .flat_map(|d| d.setup_targets())
        .filter_map(|p| setup_to_formspec::pack_to_form_spec(&p.pack_path, &p.provider_id))
        .flat_map(|spec| spec.questions.into_iter())
        .filter(|q| q.secret)
        .map(|q| q.id)
        .collect();

    let mut scopes_json = Vec::new();
    for scope in &req.scopes {
        let mut setup_answers = JsonMap::new();
        for (provider_id, provider_answers) in &scope.answers {
            let mut encrypted_answers = JsonMap::new();
            if let Some(obj) = provider_answers.as_object() {
                for (field, value) in obj {
                    if secret_fields.contains(field) && req.key.is_some() {
                        let key = req.key.as_deref().unwrap();
                        match crate::answers_crypto::encrypt_value(value, key) {
                            Ok(enc) => {
                                encrypted_answers.insert(field.clone(), enc);
                            }
                            Err(_) => {
                                encrypted_answers.insert(field.clone(), value.clone());
                            }
                        }
                    } else {
                        encrypted_answers.insert(field.clone(), value.clone());
                    }
                }
            }
            setup_answers.insert(provider_id.clone(), Value::Object(encrypted_answers));
        }
        scopes_json.push(serde_json::json!({
            "tenant": scope.tenant,
            "team": scope.team,
            "env": scope.env,
            "setup_answers": setup_answers,
        }));
    }

    // Single scope → flat format (compatible with --answers)
    // Multiple scopes → array format
    let doc = if scopes_json.len() == 1 {
        let mut single = scopes_json.into_iter().next().unwrap();
        if let Some(obj) = single.as_object_mut() {
            obj.insert(
                "greentic_setup_version".to_string(),
                Value::String("1.0.0".to_string()),
            );
            obj.insert(
                "bundle_source".to_string(),
                Value::String(bundle_path.display().to_string()),
            );
        }
        single
    } else {
        serde_json::json!({
            "greentic_setup_version": "1.0.0",
            "bundle_source": bundle_path.display().to_string(),
            "scopes": scopes_json,
        })
    };

    Json(doc)
}

#[derive(Deserialize)]
struct DecryptRequest {
    doc: Value,
    key: String,
}

async fn post_decrypt(Json(req): Json<DecryptRequest>) -> Json<Value> {
    match crate::answers_crypto::decrypt_tree(&req.doc, &req.key) {
        Ok(decrypted) => Json(serde_json::json!({ "ok": true, "doc": decrypted })),
        Err(e) => Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
    }
}

async fn post_shutdown(State(state): State<std::sync::Arc<UiState>>) {
    let _ = state.shutdown_tx.send(());
}

// ── Execution ──

fn execute_setup(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    answers: JsonMap<String, Value>,
) -> ExecutionResult {
    let config = SetupConfig {
        tenant: tenant.to_string(),
        team: team.map(String::from),
        env: env.to_string(),
        offline: false,
        verbose: true,
    };

    let static_routes = match StaticRoutesPolicy::normalize(None, env) {
        Ok(sr) => sr,
        Err(e) => {
            return ExecutionResult {
                success: false,
                stdout: String::new(),
                stderr: format!("Failed to normalize static routes: {e}"),
                manual_steps: vec![],
            };
        }
    };

    // Collect manual steps before moving answers into request
    let provider_configs: Vec<(String, serde_json::Value)> = answers
        .iter()
        .map(|(id, val)| (id.clone(), val.clone()))
        .collect();
    let team_str = team.unwrap_or("default");
    let manual_steps =
        crate::webhook::collect_post_setup_instructions(&provider_configs, tenant, team_str);

    let request = SetupRequest {
        bundle: bundle_path.to_path_buf(),
        tenants: vec![TenantSelection {
            tenant: tenant.to_string(),
            team: team.map(String::from),
            allow_paths: Vec::new(),
        }],
        static_routes,
        deployment_targets: Vec::new(),
        setup_answers: answers,
        ..Default::default()
    };

    let engine = SetupEngine::new(config);

    let plan = match engine.plan(SetupMode::Create, &request, false) {
        Ok(p) => p,
        Err(e) => {
            return ExecutionResult {
                success: false,
                stdout: String::new(),
                stderr: format!("Failed to build plan: {e}"),
                manual_steps: vec![],
            };
        }
    };

    // Capture plan summary
    let mut stdout = String::new();
    for step in &plan.steps {
        stdout.push_str(&format!("  {:?}: {}\n", step.kind, step.description));
    }

    match engine.execute(&plan) {
        Ok(report) => {
            stdout.push_str(&format!(
                "\n{} provider(s) updated, {} pack(s) resolved.\n",
                report.provider_updates,
                report.resolved_packs.len()
            ));
            if !report.warnings.is_empty() {
                for w in &report.warnings {
                    stdout.push_str(&format!("  warning: {w}\n"));
                }
            }
            ExecutionResult {
                success: true,
                stdout: format!(
                    "Plan ({} steps):\n{stdout}Setup completed successfully.",
                    plan.steps.len()
                ),
                stderr: String::new(),
                manual_steps,
            }
        }
        Err(e) => ExecutionResult {
            success: false,
            stdout,
            stderr: format!("Execution failed: {e}"),
            manual_steps: vec![],
        },
    }
}

// ── Helpers ──

/// Load previously saved secret values from the dev store for all providers.
async fn load_saved_secrets(
    bundle_path: &Path,
    env: &str,
    tenant: &str,
    team: Option<&str>,
    provider_form_specs: &[wizard::ProviderFormSpec],
) -> std::collections::HashMap<String, std::collections::HashMap<String, String>> {
    use greentic_secrets_lib::SecretsStore;

    let store = match crate::secrets::open_dev_store(bundle_path) {
        Ok(s) => s,
        Err(_) => return std::collections::HashMap::new(),
    };

    let mut result = std::collections::HashMap::new();
    for pfs in provider_form_specs {
        let mut values = std::collections::HashMap::new();
        for q in &pfs.form_spec.questions {
            let uri = crate::canonical_secret_uri(env, tenant, team, &pfs.provider_id, &q.id);
            if let Ok(bytes) = store.get(&uri).await
                && let Ok(text) = String::from_utf8(bytes)
                && !text.is_empty()
            {
                values.insert(q.id.clone(), text);
            }
        }
        if !values.is_empty() {
            result.insert(pfs.provider_id.clone(), values);
        }
    }
    result
}

async fn persist_ui_draft(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    answers: &JsonMap<String, Value>,
) -> Result<JsonMap<String, Value>> {
    let discovered = discovery::discover(bundle_path).ok();
    let mut persisted = JsonMap::new();

    for (provider_id, provider_answers) in answers {
        let Some(config) = provider_answers.as_object() else {
            continue;
        };
        if config.is_empty() {
            continue;
        }

        let pack_path = discovered.as_ref().and_then(|d| {
            d.find_setup_target(provider_id)
                .map(|provider| provider.pack_path.as_path())
        });

        let keys = crate::qa::persist::persist_all_config_as_secrets(
            bundle_path,
            env,
            tenant,
            team,
            provider_id,
            provider_answers,
            pack_path,
        )
        .await?;

        if !keys.is_empty() {
            persisted.insert(provider_id.clone(), serde_json::to_value(keys)?);
        }
    }

    Ok(persisted)
}

/// Extract a non-empty string from a JSON value (handles String, Number, Bool).
fn value_as_nonempty_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn form_question_to_info(q: &qa_spec::QuestionSpec, i18n: Option<&CliI18n>) -> QuestionInfo {
    let visible_if = q.visible_if.as_ref().and_then(|v| match v {
        qa_spec::Expr::Eq { left, right } => {
            let field = match left.as_ref() {
                qa_spec::Expr::Answer { path } => path.clone(),
                _ => return None,
            };
            let eq = match right.as_ref() {
                qa_spec::Expr::Literal { value } => {
                    Some(value.as_str().unwrap_or("true").to_string())
                }
                _ => None,
            };
            Some(VisibleIfInfo { field, eq })
        }
        qa_spec::Expr::Answer { path } => Some(VisibleIfInfo {
            field: path.clone(),
            eq: None,
        }),
        _ => None,
    });

    // Resolve title and help from i18n if available
    let title_key = format!("ui.q.{}", q.id);
    let help_key = format!("ui.q.{}.help", q.id);

    let title = i18n
        .and_then(|i| {
            let t = i.t(&title_key);
            if t != title_key { Some(t) } else { None }
        })
        .unwrap_or_else(|| q.title.clone());

    let help = i18n
        .and_then(|i| {
            let t = i.t(&help_key);
            if t != help_key { Some(t) } else { None }
        })
        .or_else(|| q.description.clone());

    QuestionInfo {
        id: q.id.clone(),
        title,
        kind: format!("{:?}", q.kind),
        required: q.required,
        secret: q.secret,
        default_value: q.default_value.clone(),
        saved_value: None,
        help,
        choices: q.choices.clone(),
        visible_if,
        placeholder: None,
        group: None,
        docs_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::persist_ui_draft;
    use crate::secrets::open_dev_store;
    use greentic_secrets_lib::SecretsStore;
    use serde_json::{Map as JsonMap, Value, json};
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn write_pack_with_secret_requirements(
        path: &std::path::Path,
        pack_id: &str,
        req_json: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("manifest.json", SimpleFileOptions::default())?;
        zip.write_all(format!(r#"{{"pack_id":"{pack_id}"}}"#).as_bytes())?;
        zip.start_file(
            "assets/secret-requirements.json",
            SimpleFileOptions::default(),
        )?;
        zip.write_all(req_json.as_bytes())?;
        zip.finish()?;
        Ok(())
    }

    #[tokio::test]
    async fn persist_ui_draft_writes_provider_answers_to_dev_store() {
        let temp = tempfile::tempdir().expect("tempdir");
        let bundle_root = temp.path();
        std::fs::create_dir_all(bundle_root.join("packs")).expect("packs dir");

        let pack_path = bundle_root.join("packs").join("weatherapi-pack.gtpack");
        write_pack_with_secret_requirements(
            &pack_path,
            "weatherapi-pack",
            r#"[{"key":"auth.param.get_weather.key"}]"#,
        )
        .expect("pack");

        let answers = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "weatherapi-pack": {
                "auth_param_get_weather_key": "test-weather-key"
            }
        }))
        .expect("answers");

        let persisted = persist_ui_draft(bundle_root, "dev-tenant", None, "dev", &answers)
            .await
            .expect("persist draft");
        assert_eq!(
            persisted.get("weatherapi-pack"),
            Some(&json!(["auth_param_get_weather_key"]))
        );

        let store = open_dev_store(bundle_root).expect("open store");
        let base_uri = crate::canonical_secret_uri(
            "dev",
            "dev-tenant",
            None,
            "weatherapi-pack",
            "auth_param_get_weather_key",
        );
        let alias_uri = crate::canonical_secret_uri(
            "dev",
            "dev-tenant",
            None,
            "weatherapi-pack",
            "auth.param.get_weather.key",
        );
        let base_value =
            String::from_utf8(store.get(&base_uri).await.expect("base")).expect("base utf8");
        let alias_value =
            String::from_utf8(store.get(&alias_uri).await.expect("alias")).expect("alias utf8");
        assert_eq!(base_value, "test-weather-key");
        assert_eq!(alias_value, "test-weather-key");
    }
}
