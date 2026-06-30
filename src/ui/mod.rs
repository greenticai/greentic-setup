//! Web-based setup UI server.
//!
//! Launches an Axum HTTP server on a random port, opens the browser, and serves
//! a single-page app that drives the setup wizard through the same FormSpec
//! infrastructure as the terminal wizard.

mod assets;

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use url::Url;

use crate::cli_i18n::CliI18n;
use crate::engine::{SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::platform_setup::StaticRoutesPolicy;
use crate::qa::wizard;
use crate::setup_tunnel::{
    SetupTunnel, inject_setup_public_base_url, is_ephemeral_tunnel_url, should_start_setup_tunnel,
    start_setup_tunnel,
};
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
    /// Where the on-disk artifact should be written back after a successful
    /// setup. `Some(Archive)` means re-pack the extracted bundle dir into
    /// a `.gtbundle`; `Some(Directory)` means copy the dir; `None` means
    /// the user passed a directory and the working dir IS the artifact, so
    /// no copy/repack is needed.
    output_target: Option<crate::cli_helpers::SetupOutputTarget>,
    local_base_url: String,
    setup_session_id: String,
    setup_tunnel: Mutex<Option<SetupTunnel>>,
    setup_tunnel_start: AsyncMutex<()>,
    setup_runtime: Mutex<Option<SetupRuntime>>,
    setup_runtime_start: AsyncMutex<()>,
    shutdown_tx: broadcast::Sender<()>,
    #[allow(dead_code)]
    result: Mutex<Option<ExecutionResult>>,
}

struct SetupRuntime {
    child: Child,
    info: Arc<Mutex<SetupRuntimeInfo>>,
}

#[derive(Clone, Debug, Default)]
struct SetupRuntimeInfo {
    local_base_url: Option<String>,
    public_base_url: Option<String>,
    system_log_line_floor: Option<usize>,
    ready: bool,
}

impl Drop for SetupRuntime {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_web_component: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_backend_contract: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_machine: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_actions: Option<Value>,
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
    /// Pre-populated rows for `kind: List` questions, hydrated on wizard
    /// re-run from the bundle's existing tenant config (e.g. nav_links).
    /// Each entry is a JSON object keyed by `column.id` whose value matches
    /// the column kind (string for scalars, locale-keyed object for
    /// multilingual cells).
    #[serde(skip_serializing_if = "Option::is_none")]
    saved_rows: Option<Vec<Value>>,
    help: Option<String>,
    choices: Option<Vec<String>>,
    visible_if: Option<VisibleIfInfo>,
    placeholder: Option<String>,
    group: Option<String>,
    docs_url: Option<String>,
    /// Column schema for `kind: List` (table) questions. Each entry tells
    /// the front-end how to render one cell per row. Absent for scalar
    /// kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    list_columns: Option<Vec<ListColumnInfo>>,
    /// Minimum row count for a `kind: List` question.
    #[serde(skip_serializing_if = "Option::is_none")]
    min_rows: Option<usize>,
    /// Maximum row count for a `kind: List` question.
    #[serde(skip_serializing_if = "Option::is_none")]
    max_rows: Option<usize>,
}

/// Per-column metadata sent to the front-end so it can render one input
/// per cell when the question kind is `List` (a.k.a. table).
#[derive(Serialize, Clone)]
struct ListColumnInfo {
    id: String,
    title: String,
    kind: String,
    required: bool,
    help: Option<String>,
    placeholder: Option<String>,
    choices: Option<Vec<String>>,
    default_value: Option<String>,
    /// When true, the front-end renders a multi-locale cell — operator can
    /// add per-locale translations via "+ Add language". Persisted as a
    /// locale-keyed object instead of a plain string.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    multilingual: bool,
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
    /// Per-column metadata for `kind: table` questions. Maps column `key`
    /// → multilingual flag. Used by the UI to render i18n-aware cells.
    /// Empty for non-table questions.
    column_multilingual: std::collections::HashMap<String, bool>,
}

#[derive(Deserialize)]
struct ExecuteRequest {
    answers: JsonMap<String, Value>,
    #[serde(default)]
    provider_setup_status: JsonMap<String, Value>,
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
struct ProviderSetupEventRequest {
    provider_id: String,
    event_name: String,
    #[serde(default)]
    event_detail: Value,
    #[serde(default)]
    current_step_id: Option<Value>,
    #[serde(default)]
    current_progress: Option<Value>,
    #[serde(default)]
    action_name: Option<Value>,
    #[serde(default)]
    request_method: Option<Value>,
    #[serde(default)]
    request_path: Option<Value>,
    #[serde(default)]
    http_status: Option<Value>,
    #[serde(default)]
    response_body: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    correlation_id: Option<Value>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    setup_session_id: Option<String>,
    #[serde(default)]
    setup_ui_url: Option<String>,
}

#[derive(Deserialize)]
struct ProviderSetupEventsQuery {
    provider_id: String,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct DraftSaveRequest {
    answers: JsonMap<String, Value>,
    tenant: String,
    #[serde(default)]
    team: Option<String>,
    env: String,
    #[serde(default)]
    tunnel: Option<String>,
}

#[derive(Deserialize)]
struct SetupActionRequest {
    provider_id: String,
    action_id: String,
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
struct SetupPublicUrlRequest {
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    env: Option<String>,
    #[serde(default)]
    tunnel: Option<String>,
}

#[derive(Serialize)]
struct ScopeResponse {
    tenant: String,
    team: Option<String>,
    env: String,
    detected_tenant: Option<String>,
    cloud_deploy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tunnel: Option<String>,
}

#[derive(Serialize, Clone)]
struct ExecutionResult {
    success: bool,
    stdout: String,
    stderr: String,
    manual_steps: Vec<crate::webhook::ProviderInstruction>,
    #[serde(default, skip_serializing_if = "JsonMap::is_empty")]
    provider_setup_status: JsonMap<String, Value>,
}

#[derive(Clone, Debug)]
struct DeclaredStaticRoute {
    provider_id: String,
    pack_path: PathBuf,
    public_path: String,
    source_root: String,
}

#[derive(Clone, Debug)]
struct ProviderBackendContract {
    provider_id: String,
    inline: Value,
    load_error: Option<String>,
}

#[derive(Clone, Debug)]
struct DeclaredProviderHttpRoute {
    provider_id: String,
    pack_path: PathBuf,
    methods: Vec<String>,
    target: ProviderHttpRouteTarget,
    segments: Vec<ProviderHttpRouteSegment>,
}

#[derive(Clone, Debug)]
enum ProviderHttpRouteTarget {
    SetupComponent { component_ref: String, op: String },
    ProviderIngress { component_ref: String, op: String },
}

#[derive(Clone, Debug)]
enum ProviderHttpRouteSegment {
    Literal(String),
    Tenant,
    Team,
    Wildcard,
}

#[derive(Clone, Debug)]
struct ProviderHttpRouteMatch {
    route: DeclaredProviderHttpRoute,
    tenant: String,
    team: String,
}

struct ProviderHttpLocalExecution<'a> {
    contract: &'a ProviderBackendContract,
    provider_id: &'a str,
    tenant: &'a str,
    action: &'a Value,
    target: &'a str,
    method: &'a str,
    payload: Value,
    runtime_context: Value,
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
    _scope_from_answers: bool,
    output_target: Option<crate::cli_helpers::SetupOutputTarget>,
) -> Result<()> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");
    let setup_session_id = format!("setup-{port}-{}", unix_timestamp_millis());

    let state = std::sync::Arc::new(UiState {
        bundle_path: bundle_path.to_path_buf(),
        tenant: tenant.to_string(),
        team: team.map(String::from),
        env: env.to_string(),
        advanced,
        locale: locale.map(String::from),
        prefill_answers,
        output_target,
        local_base_url: url.clone(),
        setup_session_id,
        setup_tunnel: Mutex::new(None),
        setup_tunnel_start: AsyncMutex::new(()),
        setup_runtime: Mutex::new(None),
        setup_runtime_start: AsyncMutex::new(()),
        shutdown_tx: shutdown_tx.clone(),
        result: Mutex::new(None),
    });

    let router = build_router(state.clone());

    eprintln!("Setup UI started at: {url}");
    if std::env::var("GREENTIC_SETUP_NO_OPEN").ok().as_deref() != Some("1") {
        let _ = open::that(&url);
    }

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
        .route("/api/result", get(get_result))
        .route(
            "/api/provider-setup-events",
            get(get_provider_setup_events).post(post_provider_setup_event),
        )
        .route("/api/draft", post(post_draft))
        .route("/api/setup-public-url", post(post_setup_public_url))
        .route("/api/setup-action", post(post_setup_action))
        .route("/api/execute", post(post_execute))
        .route("/api/export", post(post_export))
        .route("/api/decrypt", post(post_decrypt))
        .route("/oauth/callback/{provider}", get(get_oauth_callback))
        .route("/v1/web/{*asset_path}", get(get_declared_static_asset))
        .route(
            "/v1/setup/{*proxy_path}",
            any(proxy_declared_provider_http_route),
        )
        .route(
            "/v1/messaging/setup/{*proxy_path}",
            any(proxy_provider_setup_api),
        )
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

    // The web UI should honor the requested CLI/answers scope. Detected bundle
    // tenants are informational only; otherwise a scaffold containing both
    // `demo` and `default` can silently shift setup into the wrong tenant.
    let effective_tenant = cli_tenant.clone();

    let cloud_deploy = prefill_has_cloud_deployment_targets(state.prefill_answers.as_ref());
    let tunnel = crate::platform_setup::load_tunnel_artifact(bundle_path)
        .ok()
        .flatten()
        .and_then(|answers| answers.mode)
        .map(|mode| mode.trim().to_string())
        .filter(|mode| !mode.is_empty());

    Json(ScopeResponse {
        tenant: effective_tenant,
        team: state.team.clone(),
        env: cli_env.clone(),
        detected_tenant,
        cloud_deploy,
        tunnel,
    })
}

fn prefill_has_cloud_deployment_targets(prefill: Option<&JsonMap<String, Value>>) -> bool {
    prefill
        .and_then(|answers| answers.get("platform_setup"))
        .and_then(|value| value.as_object())
        .and_then(|platform_setup| platform_setup.get("deployment_targets"))
        .and_then(|value| value.as_array())
        .map(|targets| {
            targets.iter().any(|target| {
                target
                    .get("target")
                    .and_then(Value::as_str)
                    .is_some_and(|target| matches!(target, "aws" | "gcp" | "azure"))
            })
        })
        .unwrap_or(false)
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
        if let Some(pos) = t.iter().position(|tenant| tenant == &state.tenant) {
            let selected = t.remove(pos);
            t.insert(0, selected);
        }
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
                let mut cloned = file_ans.clone();
                // Migrate legacy `<id>_json` string answers to their array
                // equivalent (the new `kind: table` wizard writes the array
                // form). Without this the legacy ghost dominates the prefill
                // and silently overrides the user's table edits on the next
                // sync.
                // Currently we only know one legacy `_json` string key —
                // `nav_links_json`. Open-coded rather than looping a single
                // element. If we add more table questions later, swap to a
                // const slice + for loop again.
                if let Some(map) = cloned.as_object_mut() {
                    let legacy_key = "nav_links_json";
                    let canonical_key = "nav_links";
                    if !map.contains_key(canonical_key)
                        && let Some(Value::String(raw)) = map.get(legacy_key)
                        && let Ok(parsed) = serde_json::from_str::<Value>(raw)
                        && parsed.is_array()
                    {
                        map.insert(canonical_key.to_string(), parsed);
                    }
                    map.remove(legacy_key);
                }
                merged_answers.insert(pid.clone(), cloned);
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

    let static_routes_by_provider = declared_static_routes_by_provider(&setup_targets);

    let providers: Vec<ProviderInfo> = setup_targets
        .iter()
        .map(|p| {
            let form = setup_to_formspec::pack_to_form_spec(&p.pack_path, &p.provider_id);
            let setup_web_component = load_setup_web_component_descriptor(
                p,
                static_routes_by_provider.get(&p.provider_id),
            );
            let setup_backend_contract =
                load_setup_backend_contract_descriptor(p).map(|contract| contract.inline);
            let setup_machine = load_setup_machine_descriptor(p);
            let setup_actions = load_setup_actions_descriptor(p);
            ProviderInfo {
                provider_id: p.provider_id.clone(),
                display_name: p.display_name.clone(),
                domain: p.domain.clone(),
                question_count: form.as_ref().map(|f| f.questions.len()).unwrap_or(0),
                setup_web_component,
                setup_backend_contract,
                setup_machine,
                setup_actions,
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
                let mut column_multilingual = std::collections::HashMap::new();
                for col in &q.columns {
                    if col.multilingual {
                        column_multilingual.insert(col.key.clone(), true);
                    }
                }
                map.insert(
                    q.name.clone(),
                    SetupQuestionExtras {
                        placeholder: q.placeholder.clone(),
                        group: q.group.clone(),
                        docs_url: q.docs_url.clone(),
                        column_multilingual,
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
                        if !info.secret {
                            info.saved_value = Some(val);
                        }
                        found = true;
                        break;
                    }
                }
            }
            // Fall back to saved secrets
            if !found {
                for secrets in saved_secrets.values() {
                    if let Some(val) = secrets.get(&q.id) {
                        if !info.secret {
                            info.saved_value = Some(val.clone());
                        }
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
                            // Overlay per-column multilingual flags onto the
                            // table-rendering metadata (qa-spec QuestionSpec
                            // has no slot for this hint, so we carry it
                            // out-of-band via SetupQuestionExtras).
                            if let Some(ref mut cols) = info.list_columns {
                                for col in cols.iter_mut() {
                                    if ext
                                        .column_multilingual
                                        .get(&col.id)
                                        .copied()
                                        .unwrap_or(false)
                                    {
                                        col.multilingual = true;
                                    }
                                }
                            }
                        }
                        // --answers prefill takes priority over saved secrets
                        if let Some(val) = answers
                            .and_then(|m| m.get(&q.id))
                            .and_then(value_as_nonempty_string)
                        {
                            if !info.secret {
                                info.saved_value = Some(val);
                            }
                        } else if let Some(val) = saved.and_then(|m| m.get(&q.id))
                            && !info.secret
                        {
                            info.saved_value = Some(val.clone());
                        }
                        // Hydrate kind: List rows from --answers (if it
                        // carries an array) or, for the webchat-gui
                        // nav_links table, from the bundle's persisted
                        // tenant.json so a wizard re-run pre-populates the
                        // pills the operator just configured.
                        if matches!(q.kind, qa_spec::QuestionType::List) {
                            if let Some(arr) = answers
                                .and_then(|m| m.get(&q.id))
                                .and_then(Value::as_array)
                                .filter(|a| !a.is_empty())
                            {
                                info.saved_rows = Some(arr.clone());
                                eprintln!(
                                    "[hydrate] {} {} → saved_rows from prefill: {} row(s)",
                                    pfs.provider_id,
                                    q.id,
                                    arr.len()
                                );
                            } else if q.id == "nav_links"
                                && pfs.provider_id.contains("webchat-gui")
                            {
                                match crate::tenant_config::read_existing_nav_links(
                                    &state.bundle_path,
                                    &state.tenant,
                                ) {
                                    Some(rows) => {
                                        eprintln!(
                                            "[hydrate] {} nav_links → saved_rows from tenant.json: {} row(s)",
                                            pfs.provider_id,
                                            rows.len()
                                        );
                                        info.saved_rows = Some(rows);
                                    }
                                    None => {
                                        eprintln!(
                                            "[hydrate] {} nav_links → tenant.json had no nav_links (bundle_path={}, tenant={})",
                                            pfs.provider_id,
                                            state.bundle_path.display(),
                                            state.tenant
                                        );
                                    }
                                }
                            }
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

fn declared_static_routes_by_provider(
    setup_targets: &[&discovery::DetectedProvider],
) -> std::collections::HashMap<String, Vec<DeclaredStaticRoute>> {
    let mut by_provider = std::collections::HashMap::new();
    for provider in setup_targets {
        let routes = load_declared_static_routes(provider);
        if !routes.is_empty() {
            by_provider.insert(provider.provider_id.clone(), routes);
        }
    }
    by_provider
}

fn load_declared_static_routes(provider: &discovery::DetectedProvider) -> Vec<DeclaredStaticRoute> {
    let Ok(Some(extension)) =
        discovery::read_pack_extension(&provider.pack_path, "greentic.static-routes.v1")
    else {
        return Vec::new();
    };
    let Some(inline) = extension_inline(&extension) else {
        return Vec::new();
    };
    inline
        .get("routes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|route| {
            let public_path = route.get("public_path")?.as_str()?.trim();
            let source_root = route.get("source_root")?.as_str()?.trim();
            if !is_safe_same_origin_path(public_path) || !is_safe_pack_relative_path(source_root) {
                return None;
            }
            Some(DeclaredStaticRoute {
                provider_id: provider.provider_id.clone(),
                pack_path: provider.pack_path.clone(),
                public_path: public_path.trim_end_matches('/').to_string(),
                source_root: source_root.trim_matches('/').to_string(),
            })
        })
        .collect()
}

fn load_setup_web_component_descriptor(
    provider: &discovery::DetectedProvider,
    static_routes: Option<&Vec<DeclaredStaticRoute>>,
) -> Option<Value> {
    let extension =
        discovery::read_pack_extension(&provider.pack_path, "greentic.setup.web-component.v1")
            .ok()??;
    let inline = extension_inline(&extension)?.clone();
    if inline
        .get("schema_id")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema != "greentic.setup.web-component.v1")
    {
        return None;
    }
    let module_url = inline.get("module_url")?.as_str()?;
    if !is_safe_same_origin_path(module_url) {
        return None;
    }
    let routes = static_routes?;
    if !routes
        .iter()
        .any(|route| route_template_covers_url_template(&route.public_path, module_url))
    {
        return None;
    }
    Some(inline)
}

fn load_setup_backend_contract_descriptor(
    provider: &discovery::DetectedProvider,
) -> Option<ProviderBackendContract> {
    let extension =
        discovery::read_pack_extension(&provider.pack_path, "greentic.setup.backend-contract.v1")
            .ok()??;
    let descriptor = extension_inline(&extension)?.clone();
    if descriptor
        .get("schema_id")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema != "greentic.setup.backend-contract.v1")
    {
        return None;
    }
    let provider_id = descriptor.get("provider_id")?.as_str()?.trim();
    if provider_id != provider.provider_id {
        return None;
    }
    let (inline, load_error) = match descriptor.get("asset").and_then(Value::as_str) {
        Some(asset) => match load_setup_backend_contract_asset(provider, asset, &descriptor) {
            Ok(contract) => (contract, None),
            Err(err) => (descriptor, Some(err.to_string())),
        },
        None => (descriptor, None),
    };
    Some(ProviderBackendContract {
        provider_id: provider.provider_id.clone(),
        inline,
        load_error,
    })
}

fn load_setup_machine_descriptor(provider: &discovery::DetectedProvider) -> Option<Value> {
    let machine = crate::setup_machine::load_setup_machine_from_pack(&provider.pack_path)
        .ok()
        .flatten()?;
    Some(serde_json::json!({
        "schema_id": crate::setup_machine::SETUP_MACHINE_EXTENSION,
        "provider_id": provider.provider_id,
        "id": machine.id,
        "version": machine.version,
        "display_name": machine.display_name,
        "entry_step": machine.entry_step,
        "steps": machine.steps.iter().map(|step| {
            serde_json::json!({
                "id": step.id,
                "kind": step.kind,
                "title": step.title,
            })
        }).collect::<Vec<_>>(),
    }))
}

fn load_setup_actions_descriptor(provider: &discovery::DetectedProvider) -> Option<Value> {
    let extension =
        discovery::read_pack_extension(&provider.pack_path, "greentic.setup.actions.v1").ok()?;
    if let Some(extension) = extension {
        let mut inline = extension_inline(&extension)?.clone();
        if inline
            .get("schema_id")
            .and_then(Value::as_str)
            .is_some_and(|schema| schema != "greentic.setup.actions.v1")
        {
            return None;
        }
        let provider_id = inline.get("provider_id")?.as_str()?.trim();
        if provider_id != provider.provider_id {
            return None;
        }
        if !inline.get("actions").is_some_and(Value::is_array) {
            return None;
        }
        merge_legacy_setup_actions(provider, &mut inline);
        return Some(inline);
    }
    load_legacy_setup_actions_descriptor(provider)
}

fn merge_legacy_setup_actions(provider: &discovery::DetectedProvider, descriptor: &mut Value) {
    let Some(legacy) = load_legacy_setup_actions_descriptor(provider) else {
        return;
    };
    let Some(legacy_actions) = legacy.get("actions").and_then(Value::as_array) else {
        return;
    };
    let Some(actions) = descriptor.get_mut("actions").and_then(Value::as_array_mut) else {
        return;
    };
    let mut seen_ids = actions
        .iter()
        .filter_map(|action| action.get("id").and_then(Value::as_str))
        .map(ToString::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    for action in legacy_actions {
        let id = action.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || seen_ids.insert(id.to_string()) {
            actions.push(action.clone());
        }
    }
}

fn load_legacy_setup_actions_descriptor(provider: &discovery::DetectedProvider) -> Option<Value> {
    let spec = crate::setup_input::load_setup_spec(&provider.pack_path)
        .ok()
        .flatten()?;
    if spec.setup_actions.is_empty() {
        return None;
    }
    let mut actions = spec.setup_actions;
    normalize_setup_action_provider_ids(&mut actions, &provider.provider_id);
    Some(serde_json::json!({
        "schema_id": "greentic.setup.actions.v1",
        "provider_id": provider.provider_id,
        "source": "legacy_setup_yaml",
        "actions": actions,
    }))
}

fn normalize_setup_action_provider_ids(actions: &mut [Value], provider_id: &str) {
    for action in actions {
        let Some(map) = action.as_object_mut() else {
            continue;
        };
        map.insert(
            "provider_id".to_string(),
            Value::String(provider_id.to_string()),
        );
    }
}

fn load_setup_backend_contract_asset(
    provider: &discovery::DetectedProvider,
    asset: &str,
    descriptor: &Value,
) -> Result<Value> {
    if !is_safe_pack_relative_path(asset) {
        anyhow::bail!("setup backend contract asset path is not safe: {asset}");
    }
    let mut contract = read_pack_json_asset(&provider.pack_path, asset)
        .with_context(|| format!("failed to load setup backend contract asset {asset}"))?;
    if contract
        .get("schema_id")
        .and_then(Value::as_str)
        .is_some_and(|schema| schema != "greentic.setup.backend-contract.v1")
    {
        anyhow::bail!("setup backend contract asset has wrong schema_id");
    }
    let contract_provider_id = contract
        .get("provider_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if contract_provider_id != provider.provider_id {
        anyhow::bail!("setup backend contract asset provider_id does not match pack provider");
    }
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
    Ok(contract)
}

fn read_pack_json_asset(pack_path: &Path, entry_name: &str) -> Result<Value> {
    let file = std::fs::File::open(pack_path)
        .with_context(|| format!("open provider pack {}", pack_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("provider pack is not a zip archive")?;
    let mut entry = archive
        .by_name(entry_name)
        .with_context(|| format!("provider pack missing {entry_name}"))?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut entry, &mut text)?;
    serde_json::from_str(&text).with_context(|| format!("parse provider pack asset {entry_name}"))
}

fn find_setup_backend_contract(
    bundle_path: &Path,
    provider_id: &str,
) -> Result<Option<ProviderBackendContract>> {
    let discovered = discovery::discover(bundle_path)?;
    let Some(provider) = discovered.find_setup_target(provider_id) else {
        return Ok(None);
    };
    Ok(load_setup_backend_contract_descriptor(provider))
}

fn find_setup_actions_descriptor(bundle_path: &Path, provider_id: &str) -> Result<Option<Value>> {
    let discovered = discovery::discover(bundle_path)?;
    let Some(provider) = discovered.find_setup_target(provider_id) else {
        return Ok(None);
    };
    Ok(load_setup_actions_descriptor(provider))
}

fn extension_inline(extension: &Value) -> Option<&Value> {
    extension.get("inline").or(Some(extension))
}

fn is_safe_same_origin_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && Url::parse(path).is_err()
}

fn is_safe_pack_relative_path(path: &str) -> bool {
    let path = path.trim_matches('/');
    !path.is_empty()
        && !path.contains('\\')
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

fn route_template_covers_url_template(public_path: &str, module_url: &str) -> bool {
    let public_path = public_path.trim_end_matches('/');
    module_url == public_path || module_url.starts_with(&format!("{public_path}/"))
}

async fn get_declared_static_asset(
    State(state): State<std::sync::Arc<UiState>>,
    AxumPath(asset_path): AxumPath<String>,
) -> Response {
    let request_path = format!("/v1/web/{asset_path}");
    let Ok(discovered) = discovery::discover(&state.bundle_path) else {
        return status_text(StatusCode::NOT_FOUND, "no bundle packs discovered");
    };
    let targets = discovered.setup_targets();
    for route in declared_static_routes_by_provider(&targets)
        .into_values()
        .flatten()
    {
        if let Some(relative_asset) = match_declared_static_route(&route.public_path, &request_path)
        {
            return serve_pack_asset(&route, &relative_asset);
        }
    }
    status_text(
        StatusCode::NOT_FOUND,
        "asset is not covered by a declared static route",
    )
}

fn match_declared_static_route(public_path_template: &str, request_path: &str) -> Option<String> {
    let route_segments: Vec<&str> = public_path_template
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    let request_segments: Vec<&str> = request_path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if request_segments.len() < route_segments.len() {
        return None;
    }
    for (route_segment, request_segment) in route_segments.iter().zip(request_segments.iter()) {
        let is_placeholder = route_segment.starts_with('{') && route_segment.ends_with('}');
        if !is_placeholder && route_segment != request_segment {
            return None;
        }
        if is_placeholder && request_segment.is_empty() {
            return None;
        }
    }
    let relative = request_segments[route_segments.len()..].join("/");
    if relative.is_empty() || !is_safe_pack_relative_path(&relative) {
        return None;
    }
    Some(relative)
}

fn serve_pack_asset(route: &DeclaredStaticRoute, relative_asset: &str) -> Response {
    let entry_name = format!(
        "{}/{}",
        route.source_root.trim_matches('/'),
        relative_asset.trim_matches('/')
    );
    let file = match std::fs::File::open(&route.pack_path) {
        Ok(file) => file,
        Err(err) => {
            return status_text(
                StatusCode::NOT_FOUND,
                &format!("provider pack not readable: {err}"),
            );
        }
    };
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(archive) => archive,
        Err(err) => {
            return status_text(
                StatusCode::NOT_FOUND,
                &format!("provider pack is not a zip archive: {err}"),
            );
        }
    };
    let mut entry = match archive.by_name(&entry_name) {
        Ok(entry) => entry,
        Err(_) => return status_text(StatusCode::NOT_FOUND, "declared asset not found in pack"),
    };
    let mut bytes = Vec::new();
    if let Err(err) = std::io::Read::read_to_end(&mut entry, &mut bytes) {
        return status_text(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string());
    }
    let bytes = maybe_patch_setup_web_component_asset(&entry_name, bytes);
    let mut response = Body::from(bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type_for_path(&entry_name)),
    );
    response.headers_mut().insert(
        "x-greentic-provider",
        HeaderValue::from_str(&route.provider_id)
            .unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );
    response
}

fn maybe_patch_setup_web_component_asset(entry_name: &str, bytes: Vec<u8>) -> Vec<u8> {
    if !entry_name.ends_with(".js") || !entry_name.contains("/setup/") {
        return bytes;
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => return err.into_bytes(),
    };
    let patched = patch_setup_web_component_reset_guard(text);
    patched.into_bytes()
}

fn patch_setup_web_component_reset_guard(text: String) -> String {
    let mut text = text;
    let marker = "if (!result || typeof result !== \"object\") return \"\";";
    if !text.contains("pending_device_login") && text.contains(marker) {
        text = text.replacen(
            marker,
            "if (!result || typeof result !== \"object\") return \"\";\n    if ((result.result && result.result.pending_device_login) || result.pending_device_login) return \"\";",
            1,
        );
    }
    let marker = "if (status.blocked) {\n      return {\n        kind: \"blocked-refresh\",\n        label: this._t(\"refreshAfterManualAction\")\n      };\n    }";
    if !text.contains("status.blocked.retryable") && text.contains(marker) {
        text = text.replacen(
            marker,
            "if (status.blocked && !status.blocked.retryable) {\n      return {\n        kind: \"blocked-refresh\",\n        label: this._t(\"refreshAfterManualAction\")\n      };\n    }",
            1,
        );
    }
    let marker = r#"if (publish.ok && !install.ok && addToTeamsUrl) {
      if (this._manualActions.addToTeamsOpened) {
        return {
          kind: "continue",
          label: this._t("verifyTeamsInstall")
        };
      }
      return {
        kind: "add-to-teams",
        label: this._t("addToTeams"),
        url: addToTeamsUrl
      };
    }"#;
    if !text.contains("greentic-setup always verifies Teams install after publish")
        && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"if (publish.ok && !install.ok && addToTeamsUrl) {
      // greentic-setup always offers install verification after publish; relying
      // on sessionStorage to switch from Add to Teams can loop after browser remounts.
      return {
        kind: "continue",
        label: this._t("verifyTeamsInstall"),
        stepId: "teams_app_user_install",
        addToTeamsUrl
      };
    }"#,
            1,
        );
    }
    let marker = r#"const result = await this._request("POST", this._endpoint("next"), this._collectConfig());"#;
    if !text.contains("greentic-setup can run a targeted setup action") && text.contains(marker) {
        text = text.replacen(
            marker,
            r#"// greentic-setup can run a targeted setup action for manual verification
        // buttons, instead of republishing whatever the scheduler considers next.
        const path = waitAction.stepId
          ? this._endpoint("next").replace(/\/next$/, `/action/${encodeURIComponent(waitAction.stepId)}`)
          : this._endpoint("next");
        const result = await this._request("POST", path, this._collectConfig());"#,
            1,
        );
    }
    let marker = r#"  _actionHtml(action) {
    return `<button type="button" class="primary" data-action="run-current">${this._escape(action.label || this._t("continue"))}</button>`;
  }"#;
    if !text.contains("greentic-setup exposes Add to Teams next to Verify") && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"  _actionHtml(action) {
    const primary = `<button type="button" class="primary" data-action="run-current">${this._escape(action.label || this._t("continue"))}</button>`;
    if (action && action.addToTeamsUrl) {
      // greentic-setup exposes Add to Teams next to Verify without making
      // verification depend on sessionStorage surviving the Teams deep link.
      return `${primary}<a class="button" target="_blank" rel="noopener noreferrer" href="${this._escape(action.addToTeamsUrl)}">${this._escape(this._t("addToTeams"))}</a>`;
    }
    return primary;
  }"#,
            1,
        );
    }
    let marker = r#"  _oauthComplete(kind) {
    const values = this._state && this._state.values || {};
    const oauth = values.oauth || {};
    return Boolean(oauth[kind || "default"] && oauth[kind || "default"].ok);
  }"#;
    if !text.contains("greentic-setup oauth_resume keeps refreshed OAuth incomplete")
        && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"  _oauthComplete(kind) {
    const values = this._state && this._state.values || {};
    const resume = values.oauth_resume || {};
    const normalized = kind || "default";
    const tokenKey = resume.token_store_key || "";
    if (
      // greentic-setup oauth_resume keeps refreshed OAuth incomplete until the
      // new device-code token exchange succeeds.
      (normalized === "management" && tokenKey === "azure_management_access_token") ||
      (normalized === "graph" && tokenKey === "graph_access_token")
    ) {
      return false;
    }
    const oauth = values.oauth || {};
    return Boolean(oauth[normalized] && oauth[normalized].ok);
  }"#,
            1,
        );
    }
    let marker = r#"    const response = values.last_oauth && values.last_oauth.response || {};
    const oauthKind = this._oauthKind();
    const codeKey = oauthKind === "management" ? "azure_management_user_code" : "oauth_user_code";
    const userCode = cfg[codeKey] || response.user_code || response.userCode;"#;
    if !text.contains("greentic-setup prefers newest OAuth response code") && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"    const response = values.last_oauth && values.last_oauth.response || {};
    const oauthKind = this._oauthKind();
    const codeKey = oauthKind === "management" ? "azure_management_user_code" : "oauth_user_code";
    // greentic-setup prefers newest OAuth response code over stale persisted config.
    const userCode = response.user_code || response.userCode || cfg[codeKey];"#,
            1,
        );
    }
    let marker =
        r#"const firstMessage = values.last_activity || values.last_webchat_conversation;"#;
    if !text.contains("greentic-setup ignores stale runtime observations") && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"// greentic-setup ignores stale runtime observations when deciding
    // whether the first Teams message has arrived for the current tunnel.
    const firstMessage = [values.last_activity, values.last_webchat_conversation].find((value) => value && !value.stale);"#,
            1,
        );
    }
    let marker = r#"if (install.ok && !firstMessage && openBotChatUrl) {"#;
    if !text.contains("greentic-setup waits for endpoint registration before bot chat")
        && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"const pendingStep = this._currentPendingStepId && this._currentPendingStepId();
    if (install.ok && openBotChatUrl && pendingStep && pendingStep !== "first_bot_framework_post") {
      // greentic-setup waits for the Bot Framework endpoint registration to be
      // current before asking Teams to send the first message.
      return {
        kind: "continue",
        label: this._t("continue") || "Continue setup"
      };
    }

    if (install.ok && !firstMessage && openBotChatUrl) {"#,
            1,
        );
    }
    let marker = r#"        kind: "open-chat","#;
    if !text.contains("greentic-setup waits for endpoint registration before bot chat")
        && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"        ...(() => {
          const pendingStep = this._currentPendingStepId && this._currentPendingStepId();
          if (pendingStep && pendingStep !== "first_bot_framework_post") {
            // greentic-setup waits for the Bot Framework endpoint registration to be
            // current before asking Teams to send the first message.
            return {
              kind: "continue",
              label: this._t("continue") || "Continue setup"
            };
          }
          return { kind: "open-chat" };
        })(),"#,
            1,
        );
    }
    let marker = r#"&& !(values.last_activity || values.last_webchat_conversation)"#;
    if text.contains("greentic-setup ignores stale runtime observations") && text.contains(marker) {
        text = text.replacen(
            marker,
            r#"&& ![values.last_activity, values.last_webchat_conversation].some((value) => value && !value.stale)"#,
            1,
        );
    }
    let marker =
        r#"firstMessage: Boolean(values.last_activity || values.last_webchat_conversation)"#;
    if text.contains("greentic-setup ignores stale runtime observations") && text.contains(marker) {
        text = text.replacen(
            marker,
            r#"firstMessage: [values.last_activity, values.last_webchat_conversation].some((value) => value && !value.stale)"#,
            1,
        );
    }
    let marker = r#"    if (!complete) {
      return {
        kind: "continue",
        label: this._t("continue") || "Continue setup"
      };
    }"#;
    if !text.contains("greentic-setup has no next action after observed completion")
        && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"    if (complete && firstMessage) {
      // greentic-setup has no next action after observed completion.
      return null;
    }

    if (!complete) {
      return {
        kind: "continue",
        label: this._t("continue") || "Continue setup"
      };
    }"#,
            1,
        );
    }
    let marker = r#"    if (pending === "first_bot_framework_post") {
      return await this._runtimeIngressPreflight();
    }"#;
    if !text.contains("greentic-setup lets backend verify runtime observation")
        && text.contains(marker)
    {
        text = text.replacen(
            marker,
            r#"    if (pending === "first_bot_framework_post") {
      // greentic-setup lets the backend/runtime observation path verify Teams
      // activity; browser GET probes against Bot Framework ingress can fail.
      return "";
    }"#,
            1,
        );
    }
    text
}

async fn proxy_provider_setup_api(
    State(state): State<std::sync::Arc<UiState>>,
    AxumPath(proxy_path): AxumPath<String>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let method = request.method().clone();
    let query = request
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let body = match to_bytes(request.into_body(), 10 * 1024 * 1024).await {
        Ok(body) => body,
        Err(err) => return status_text(StatusCode::BAD_REQUEST, &err.to_string()),
    };

    match handle_provider_setup_backend_contract(
        &state,
        method.clone(),
        &proxy_path,
        &query,
        body.clone(),
    )
    .await
    {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "error": err.to_string(),
                })),
            )
                .into_response();
        }
    }

    let declared_path = format!("/v1/messaging/setup/{}", proxy_path.trim_start_matches('/'));
    match dispatch_declared_provider_http_route(
        &state,
        method.as_str(),
        &declared_path,
        &query,
        &headers,
        body.clone(),
    )
    .await
    {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": err.to_string(),
                })),
            )
                .into_response();
        }
    }

    let Some(runtime_base) = configured_runtime_proxy_base_url() else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
            "ok": false,
            "blocked": true,
            "error": "provider setup route is not declared by pack",
            "expected": format!("/v1/messaging/setup/{proxy_path}"),
            "configure": "Declare the route in greentic.http-routes.v1 or use a backend-contract route."
            })),
        )
            .into_response();
    };
    let target = format!(
        "{}/v1/messaging/setup/{}{}",
        runtime_base.trim_end_matches('/'),
        proxy_path,
        query
    );
    match forward_runtime_request(method.clone(), &target, headers.clone(), body.clone()).await {
        Ok(response) if response.status() == StatusCode::NOT_FOUND => {
            let Some(fallback_target) =
                setup_runtime_fallback_target(&runtime_base, &proxy_path, &query, &method)
            else {
                return response;
            };
            match forward_runtime_request(method, &fallback_target, headers, body).await {
                Ok(fallback_response) => fallback_response,
                Err(err) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({
                        "ok": false,
                        "blocked": true,
                        "error": "Provider setup service is not running",
                        "target": fallback_target,
                        "detail": err.to_string(),
                        "configure": "Start the provider setup runtime, then set GREENTIC_SETUP_RUNTIME_URL to its local base URL."
                    })),
                )
                    .into_response(),
            }
        }
        Ok(response) => response,
        Err(err) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "Provider setup service is not running",
                "target": target,
                "detail": err.to_string(),
                "configure": "Start the provider setup runtime, then set GREENTIC_SETUP_RUNTIME_URL to its local base URL."
            })),
        )
            .into_response(),
    }
}

async fn proxy_declared_provider_http_route(
    State(state): State<std::sync::Arc<UiState>>,
    AxumPath(proxy_path): AxumPath<String>,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let method = request.method().clone();
    let query = request
        .uri()
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let path = format!("/v1/setup/{}", proxy_path.trim_start_matches('/'));
    let body = match to_bytes(request.into_body(), 10 * 1024 * 1024).await {
        Ok(body) => body,
        Err(err) => return status_text(StatusCode::BAD_REQUEST, &err.to_string()),
    };
    match dispatch_declared_provider_http_route(
        &state,
        method.as_str(),
        &path,
        &query,
        &headers,
        body,
    )
    .await
    {
        Ok(Some(response)) => response,
        Ok(None) => status_text(StatusCode::NOT_FOUND, "provider route not declared by pack"),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": err.to_string(),
            })),
        )
            .into_response(),
    }
}

fn setup_runtime_fallback_target(
    runtime_base: &str,
    proxy_path: &str,
    query: &str,
    method: &axum::http::Method,
) -> Option<String> {
    let segments: Vec<&str> = proxy_path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return None;
    }
    let suffix = &segments[2..];
    let fallback_path = match (method.as_str(), suffix) {
        ("GET", []) => "/api/state".to_string(),
        ("POST", ["next"]) => "/api/setup/next".to_string(),
        ("POST", ["config"]) => "/api/config".to_string(),
        ("POST", ["oauth", kind, "start"]) if is_safe_runtime_path_segment(kind) => {
            format!("/api/oauth/{kind}/start")
        }
        ("POST", ["oauth", kind, "complete"]) if is_safe_runtime_path_segment(kind) => {
            format!("/api/oauth/{kind}/complete")
        }
        _ => return None,
    };
    Some(format!(
        "{}{}{}",
        runtime_base.trim_end_matches('/'),
        fallback_path,
        query
    ))
}

async fn handle_provider_setup_backend_contract(
    state: &UiState,
    method: axum::http::Method,
    proxy_path: &str,
    _query: &str,
    body: Bytes,
) -> Result<Option<Response>> {
    let segments: Vec<&str> = proxy_path
        .trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() < 2 {
        return Ok(None);
    }
    let provider_id = segments[0];
    let tenant = segments[1];
    let suffix = &segments[2..];
    if let Some(response) = handle_provider_setup_machine(
        state,
        method.as_str(),
        provider_id,
        tenant,
        suffix,
        body.clone(),
    )? {
        return Ok(Some(response));
    }
    let Some(contract) = find_setup_backend_contract(&state.bundle_path, provider_id)? else {
        return Ok(None);
    };

    let response = match (method.as_str(), suffix) {
        ("GET", []) => {
            Json(setup_backend_contract_state(state, &contract, tenant)?).into_response()
        }
        ("POST", ["config"]) => {
            let body = parse_json_body(body)?;
            Json(setup_backend_contract_save_config(
                state, &contract, tenant, &body,
            )?)
            .into_response()
        }
        ("POST", ["next"]) => {
            let body = parse_json_body(body)?;
            Json(
                setup_backend_contract_next(
                    state,
                    &contract,
                    tenant,
                    &format!("/v1/messaging/setup/{provider_id}/{tenant}/next"),
                    &body,
                )
                .await?,
            )
            .into_response()
        }
        ("POST", ["action", step]) if is_safe_runtime_path_segment(step) => {
            let body = parse_json_body(body)?;
            Json(
                setup_backend_contract_action(
                    state,
                    &contract,
                    tenant,
                    step,
                    &format!("/v1/messaging/setup/{provider_id}/{tenant}/action/{step}"),
                    &body,
                )
                .await?,
            )
            .into_response()
        }
        ("POST", ["oauth", kind, "start"]) => {
            let body = parse_json_body(body)?;
            Json(setup_backend_contract_oauth_start(state, &contract, tenant, kind, &body).await?)
                .into_response()
        }
        ("POST", ["oauth", kind, "complete"]) => {
            Json(setup_backend_contract_oauth_complete(state, &contract, tenant, kind).await?)
                .into_response()
        }
        ("GET", _) => contract_unsupported_response(
            provider_id,
            "This setup backend contract declares an asset route, but the contract does not provide a generic asset mapping for greentic-setup to serve.",
        ),
        _ => status_text(
            StatusCode::NOT_FOUND,
            "setup backend route not declared by contract",
        ),
    };
    Ok(Some(response))
}

fn parse_json_body(body: Bytes) -> Result<Value> {
    if body.is_empty() {
        return Ok(Value::Object(JsonMap::new()));
    }
    serde_json::from_slice(&body).context("invalid JSON request body")
}

fn handle_provider_setup_machine(
    state: &UiState,
    method: &str,
    provider_id: &str,
    tenant: &str,
    suffix: &[&str],
    body: Bytes,
) -> Result<Option<Response>> {
    let discovered = discovery::discover(&state.bundle_path)?;
    let Some(provider) = discovered.find_setup_target(provider_id) else {
        return Ok(None);
    };
    let Some(machine) = crate::setup_machine::load_setup_machine_from_pack(&provider.pack_path)?
    else {
        return Ok(None);
    };
    let team = state.team.as_deref().unwrap_or("default");
    let response = match (method, suffix) {
        ("GET", []) => Json(setup_machine_ui_state(
            state,
            &provider.provider_id,
            tenant,
            team,
            &machine,
        )?)
        .into_response(),
        ("POST", ["next"]) => {
            let _ = parse_json_body(body)?;
            let output = crate::setup_machine::advance_setup_machine_with_pack(
                &state.bundle_path,
                Some(&provider.pack_path),
                &provider.provider_id,
                tenant,
                team,
                &machine,
                false,
            )?;
            Json(setup_machine_ui_report(
                state,
                &provider.provider_id,
                tenant,
                team,
                &machine,
                Some(output),
            )?)
            .into_response()
        }
        ("POST", ["retry"]) => {
            let body = parse_json_body(body)?;
            let step = body
                .get("step")
                .or_else(|| body.get("step_id"))
                .and_then(Value::as_str);
            let output = crate::setup_machine::retry_setup_machine_step(
                &state.bundle_path,
                &provider.provider_id,
                tenant,
                team,
                &machine,
                step,
            )?;
            Json(setup_machine_ui_report(
                state,
                &provider.provider_id,
                tenant,
                team,
                &machine,
                Some(output),
            )?)
            .into_response()
        }
        ("POST", ["reset"]) => {
            let _ = parse_json_body(body)?;
            let archive_path = crate::setup_machine::reset_setup_machine_state(
                &state.bundle_path,
                tenant,
                team,
                &provider.provider_id,
                "ui reset",
            )?;
            let mut report =
                setup_machine_ui_state(state, &provider.provider_id, tenant, team, &machine)?;
            if let Some(obj) = report.as_object_mut() {
                obj.insert(
                    "reset".to_string(),
                    serde_json::json!({
                        "archive_path": archive_path.map(|path| path.display().to_string()),
                    }),
                );
            }
            Json(report).into_response()
        }
        ("POST", ["config"]) => Json(setup_machine_ui_state(
            state,
            &provider.provider_id,
            tenant,
            team,
            &machine,
        )?)
        .into_response(),
        ("GET", _) => status_text(StatusCode::NOT_FOUND, "setup-machine route not declared"),
        _ => status_text(StatusCode::NOT_FOUND, "setup-machine route not declared"),
    };
    Ok(Some(response))
}

fn contract_unsupported_response(provider_id: &str, message: &str) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "ok": false,
            "blocked": true,
            "provider_id": provider_id,
            "error": "setup backend contract unsupported",
            "detail": message,
            "next": "Use a provider setup runtime via GREENTIC_SETUP_RUNTIME_URL only for development fallback, or update greentic-setup with a backend implementation for this contract."
        })),
    )
        .into_response()
}

async fn dispatch_declared_provider_http_route(
    state: &UiState,
    method: &str,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Option<Response>> {
    let Some(route_match) = find_declared_provider_http_route(
        &state.bundle_path,
        method,
        path,
        state.tenant.as_str(),
        state.team.as_deref().unwrap_or("default"),
    )?
    else {
        return Ok(None);
    };
    let output = invoke_declared_provider_http_route(
        state,
        &route_match,
        method,
        path,
        query,
        headers,
        body,
    )
    .await?;
    Ok(Some(provider_http_output_to_response(output)))
}

fn provider_http_output_to_response(output: Value) -> Response {
    let status = output
        .get("response")
        .and_then(|response| response.get("status"))
        .or_else(|| output.get("status"))
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::OK);
    let body = output
        .get("response")
        .and_then(|response| response.get("body_json"))
        .or_else(|| output.get("body_json"))
        .or_else(|| output.get("body"))
        .cloned()
        .unwrap_or(output);
    (status, Json(body)).into_response()
}

async fn invoke_declared_provider_http_route(
    state: &UiState,
    route_match: &ProviderHttpRouteMatch,
    method: &str,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Value> {
    let headers_json = provider_ingress_headers_json(method, path, query, headers)?;
    let body_json = if body.is_empty() {
        "{}".to_string()
    } else {
        String::from_utf8(body.to_vec()).context("provider route request body is not utf-8")?
    };
    let request = serde_json::json!({
        "v": 1,
        "domain": "messaging",
        "provider": route_match.route.provider_id,
        "tenant": route_match.tenant,
        "team": route_match.team,
        "method": method,
        "path": path,
        "query": parse_query_pairs(query),
        "headers": headers_json,
        "body_json": body_json,
    });
    let setup_config = SetupConfig {
        tenant: route_match.tenant.clone(),
        team: Some(route_match.team.clone()),
        env: state.env.clone(),
        offline: false,
        verbose: state.advanced,
    };
    let output = match &route_match.route.target {
        ProviderHttpRouteTarget::SetupComponent { component_ref, op } => {
            invoke_setup_component_operation_blocking(
                state.bundle_path.clone(),
                route_match.route.pack_path.clone(),
                component_ref.clone(),
                op.clone(),
                request,
                setup_config,
            )
            .await?
        }
        ProviderHttpRouteTarget::ProviderIngress { component_ref, op } => {
            anyhow::bail!(
                "provider route '{}' for {} declares provider ingress component '{}' op '{}', but greentic-setup requires setup_component_ref/setup_op for setup-time pack routes",
                path,
                route_match.route.provider_id,
                component_ref,
                op
            );
        }
    };
    Ok(output)
}

async fn invoke_setup_component_operation_blocking(
    bundle_path: PathBuf,
    pack_path: PathBuf,
    component_ref: String,
    op: String,
    request: Value,
    setup_config: SetupConfig,
) -> Result<Value> {
    tokio::task::spawn_blocking(move || {
        crate::engine::invoke_setup_component_operation(
            &bundle_path,
            &pack_path,
            &component_ref,
            &op,
            &request,
            &setup_config,
        )
    })
    .await
    .context("provider setup route task panicked")?
}

fn provider_ingress_headers_json(
    method: &str,
    path: &str,
    query: &str,
    headers: &HeaderMap,
) -> Result<String> {
    let mut object = JsonMap::new();
    for (name, value) in headers {
        if let Ok(value) = value.to_str() {
            object.insert(name.as_str().to_string(), Value::String(value.to_string()));
        }
    }
    object.insert("method".to_string(), Value::String(method.to_string()));
    object.insert("path".to_string(), Value::String(path.to_string()));
    object.insert(
        "query".to_string(),
        Value::String(query.trim_start_matches('?').to_string()),
    );
    Ok(serde_json::to_string(&Value::Object(object))?)
}

fn parse_query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .trim_start_matches('?')
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            (key.to_string(), value.to_string())
        })
        .collect()
}

fn find_declared_provider_http_route(
    bundle_path: &Path,
    method: &str,
    path: &str,
    default_tenant: &str,
    default_team: &str,
) -> Result<Option<ProviderHttpRouteMatch>> {
    let mut routes = load_declared_provider_http_routes(bundle_path)?;
    routes.sort_by(|a, b| {
        let a_wild = a
            .segments
            .iter()
            .any(|segment| matches!(segment, ProviderHttpRouteSegment::Wildcard));
        let b_wild = b
            .segments
            .iter()
            .any(|segment| matches!(segment, ProviderHttpRouteSegment::Wildcard));
        b.segments
            .len()
            .cmp(&a.segments.len())
            .then(a_wild.cmp(&b_wild))
    });
    let request_segments: Vec<&str> = path
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    for route in routes {
        if !route.methods.is_empty()
            && !route
                .methods
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(method))
        {
            continue;
        }
        if let Some((tenant, team)) =
            match_provider_http_route(&route, &request_segments, default_tenant, default_team)
        {
            return Ok(Some(ProviderHttpRouteMatch {
                route,
                tenant,
                team,
            }));
        }
    }
    Ok(None)
}

fn load_declared_provider_http_routes(
    bundle_path: &Path,
) -> Result<Vec<DeclaredProviderHttpRoute>> {
    let discovered = discovery::discover(bundle_path)?;
    let mut routes = Vec::new();
    for provider in discovered.providers {
        let Some(http_routes_extension) =
            discovery::read_pack_extension(&provider.pack_path, "greentic.http-routes.v1")?
        else {
            continue;
        };
        let http_routes =
            extension_inline(&http_routes_extension).unwrap_or(&http_routes_extension);
        let ingress_extension =
            discovery::read_pack_extension(&provider.pack_path, "messaging.provider_ingress.v1")?;
        let ingress = ingress_extension
            .as_ref()
            .and_then(|extension| extension_inline(extension).or(Some(extension)));
        let Some(records) = http_routes.get("routes").and_then(Value::as_array) else {
            continue;
        };
        for record in records {
            let Some(pattern) = record
                .get("pattern")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let methods = record
                .get("methods")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect();
            let target = match declared_provider_http_route_target(record, ingress) {
                Some(target) => target,
                None => continue,
            };
            routes.push(DeclaredProviderHttpRoute {
                provider_id: provider.provider_id.clone(),
                pack_path: provider.pack_path.clone(),
                methods,
                target,
                segments: parse_provider_http_route_pattern(pattern),
            });
        }
    }
    Ok(routes)
}

fn declared_provider_http_route_target(
    record: &Value,
    ingress: Option<&Value>,
) -> Option<ProviderHttpRouteTarget> {
    let setup_component_ref = record
        .get("setup_component_ref")
        .or_else(|| record.get("component_ref"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(component_ref) = setup_component_ref {
        let op = record
            .get("setup_op")
            .or_else(|| record.get("op"))
            .or_else(|| record.get("provider_op"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("handle_http")
            .to_string();
        return Some(ProviderHttpRouteTarget::SetupComponent {
            component_ref: component_ref.to_string(),
            op,
        });
    }

    let ingress = ingress?;
    let component_ref = ingress
        .get("component_ref")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let op = record
        .get("provider_op")
        .or_else(|| record.get("op"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ingest_http")
        .to_string();
    Some(ProviderHttpRouteTarget::ProviderIngress {
        component_ref: component_ref.to_string(),
        op,
    })
}

fn parse_provider_http_route_pattern(pattern: &str) -> Vec<ProviderHttpRouteSegment> {
    pattern
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            if segment == "{tenant}" {
                ProviderHttpRouteSegment::Tenant
            } else if segment == "{team}" {
                ProviderHttpRouteSegment::Team
            } else if segment.ends_with("*}") || segment == "*" {
                ProviderHttpRouteSegment::Wildcard
            } else {
                ProviderHttpRouteSegment::Literal(segment.to_string())
            }
        })
        .collect()
}

fn match_provider_http_route(
    route: &DeclaredProviderHttpRoute,
    request_segments: &[&str],
    default_tenant: &str,
    default_team: &str,
) -> Option<(String, String)> {
    let mut tenant = default_tenant.to_string();
    let mut team = default_team.to_string();
    let mut request_index = 0;
    for segment in &route.segments {
        match segment {
            ProviderHttpRouteSegment::Literal(expected) => {
                if request_segments.get(request_index)? != expected {
                    return None;
                }
                request_index += 1;
            }
            ProviderHttpRouteSegment::Tenant => {
                tenant = request_segments.get(request_index)?.to_string();
                if tenant.is_empty() {
                    return None;
                }
                request_index += 1;
            }
            ProviderHttpRouteSegment::Team => {
                team = request_segments.get(request_index)?.to_string();
                if team.is_empty() {
                    return None;
                }
                request_index += 1;
            }
            ProviderHttpRouteSegment::Wildcard => {
                return Some((tenant, team));
            }
        }
    }
    if request_index == request_segments.len() {
        Some((tenant, team))
    } else {
        None
    }
}

fn setup_backend_contract_state(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
) -> Result<Value> {
    let mut stored = load_setup_backend_contract_state(state, &contract.provider_id, tenant)?;
    setup_backend_refresh_renderable_runtime_observation(state, contract, tenant, &mut stored)?;
    save_setup_backend_contract_state(state, &contract.provider_id, tenant, &stored)?;
    Ok(render_setup_backend_contract_state(
        state, contract, tenant, stored,
    ))
}

fn setup_backend_refresh_renderable_runtime_observation(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
) -> Result<()> {
    let Some(action) = setup_backend_action_by_id(contract, "first_bot_framework_post") else {
        return Ok(());
    };
    let Some(executor) = action.get("executor") else {
        return Ok(());
    };
    if executor.get("kind").and_then(Value::as_str) != Some("runtime_observation") {
        return Ok(());
    }
    let config = stored
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| default_setup_backend_config(state, tenant));
    let runtime_context = setup_backend_runtime_context(state, tenant, &config);
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_activity");
    setup_backend_refresh_runtime_observation_from_runtime_logs(
        state,
        tenant,
        executor,
        stored,
        state_key,
        &runtime_context,
    )
}

fn setup_machine_ui_state(
    state: &UiState,
    provider_id: &str,
    tenant: &str,
    team: &str,
    machine: &crate::setup_machine::SetupMachine,
) -> Result<Value> {
    setup_machine_ui_report(state, provider_id, tenant, team, machine, None)
}

fn setup_machine_ui_report(
    state: &UiState,
    provider_id: &str,
    tenant: &str,
    team: &str,
    machine: &crate::setup_machine::SetupMachine,
    result: Option<Value>,
) -> Result<Value> {
    let machine_state = crate::setup_machine::load_or_init_setup_machine_state(
        &state.bundle_path,
        provider_id,
        tenant,
        team,
        machine,
    )?;
    let status = crate::setup_machine::render_setup_machine_status(machine, &machine_state);
    let items = status
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ok = machine_state.status == crate::setup_machine::SetupMachineStatus::Complete;
    let blocked = setup_machine_blocked(&machine_state.last_error);
    let next = if ok {
        "Setup complete.".to_string()
    } else if let Some(blocked) = blocked.as_ref() {
        blocked
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("Setup requires attention.")
            .to_string()
    } else if let Some(result_next) = result
        .as_ref()
        .and_then(|result| result.get("result"))
        .and_then(|result| result.get("next"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        result_next.to_string()
    } else {
        "Run the next setup step.".to_string()
    };
    let mut report = serde_json::json!({
        "ok": true,
        "values": status.get("outputs").cloned().unwrap_or_else(|| machine_state.outputs.clone()),
        "setup_status": {
            "ok": ok,
            "items": items,
            "selected": {
                "provider_id": provider_id,
                "tenant": tenant,
                "team": team,
                "env": state.env,
            },
            "blocked": blocked,
            "last_step": machine_state.current_step.clone().unwrap_or_else(|| "complete".to_string()),
            "next": next,
            "reset": false,
            "machine": status,
            "last_result": result,
        },
    });
    attach_final_setup_actions(state, provider_id, &mut report);
    Ok(report)
}

fn setup_machine_blocked(last_error: &Option<Value>) -> Option<Value> {
    let error = last_error.as_ref()?;
    Some(serde_json::json!({
        "title": "Setup step blocked",
        "summary": error
            .get("detail")
            .and_then(Value::as_str)
            .or_else(|| error.get("error").and_then(Value::as_str))
            .unwrap_or("Setup requires attention before it can continue."),
        "retryable": error
            .get("recoverable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        "detail": error,
    }))
}

fn setup_backend_contract_save_config(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    body: &Value,
) -> Result<Value> {
    let mut stored = load_setup_backend_contract_state(state, &contract.provider_id, tenant)?;
    let incoming = body
        .get("config")
        .or_else(|| body.get("values").and_then(|values| values.get("config")))
        .unwrap_or(body);
    crate::setup_backend_contract::merge_browser_config_update(
        &mut stored,
        incoming,
        &contract.inline,
        default_setup_backend_config(state, tenant),
    )?;
    save_setup_backend_contract_state(state, &contract.provider_id, tenant, &stored)?;
    Ok(render_setup_backend_contract_state(
        state, contract, tenant, stored,
    ))
}

async fn setup_backend_contract_next(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    request_path: &str,
    body: &Value,
) -> Result<Value> {
    if contract.load_error.is_some() || setup_backend_required_steps(contract).is_empty() {
        return setup_backend_contract_state(state, contract, tenant);
    }
    let _ = setup_backend_contract_save_config(state, contract, tenant, body)?;
    let mut stored = load_setup_backend_contract_state(state, &contract.provider_id, tenant)?;
    ensure_setup_backend_config_defaults(state, tenant, &mut stored)?;
    let state_before = render_setup_backend_contract_state(state, contract, tenant, stored.clone());
    let next_step = setup_backend_first_pending_step(state, contract, tenant, &stored);
    let action = setup_backend_action_by_id(contract, &next_step).cloned();
    let executor = action
        .as_ref()
        .and_then(|action| action.get("executor"))
        .cloned()
        .unwrap_or(Value::Null);
    let result = if next_step == "complete" {
        serde_json::json!({
            "ok": true,
            "step": "complete",
            "next": "Setup complete.",
            "result": { "ok": true }
        })
    } else {
        setup_backend_execute_action(state, contract, tenant, &mut stored, &next_step).await?
    };
    crate::setup_backend_contract::record_action_result(
        &state.bundle_path,
        tenant,
        state.team.as_deref().unwrap_or("default"),
        &contract.provider_id,
        &mut stored,
        result.clone(),
    )?;
    let state_after = render_setup_backend_contract_state(state, contract, tenant, stored.clone());
    if state_after
        .get("setup_status")
        .and_then(|status| status.get("ok"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        crate::setup_backend_contract::apply_runtime_outputs(
            &state.bundle_path,
            tenant,
            state.team.as_deref().unwrap_or("default"),
            &contract.provider_id,
            &contract.inline,
            &stored,
        )?;
    }
    let _ = persist_setup_backend_next_diagnostic(
        state,
        contract,
        tenant,
        request_path,
        body,
        &next_step,
        action.as_ref(),
        &executor,
        &state_before,
        &state_after,
        &result,
    );
    Ok(render_setup_backend_contract_state(
        state, contract, tenant, stored,
    ))
}

async fn setup_backend_contract_action(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    step: &str,
    request_path: &str,
    body: &Value,
) -> Result<Value> {
    if contract.load_error.is_some() || setup_backend_required_steps(contract).is_empty() {
        return setup_backend_contract_state(state, contract, tenant);
    }
    let _ = setup_backend_contract_save_config(state, contract, tenant, body)?;
    let mut stored = load_setup_backend_contract_state(state, &contract.provider_id, tenant)?;
    ensure_setup_backend_config_defaults(state, tenant, &mut stored)?;
    let state_before = render_setup_backend_contract_state(state, contract, tenant, stored.clone());
    let action = setup_backend_action_by_id(contract, step).cloned();
    let executor = action
        .as_ref()
        .and_then(|action| action.get("executor"))
        .cloned()
        .unwrap_or(Value::Null);
    let result = if action.is_some() {
        setup_backend_execute_action(state, contract, tenant, &mut stored, step).await?
    } else {
        setup_backend_action_error(
            "missing_action",
            &format!("backend contract has no action for requested step {step}"),
        )
    };
    crate::setup_backend_contract::record_action_result(
        &state.bundle_path,
        tenant,
        state.team.as_deref().unwrap_or("default"),
        &contract.provider_id,
        &mut stored,
        result.clone(),
    )?;
    let state_after = render_setup_backend_contract_state(state, contract, tenant, stored.clone());
    let _ = persist_setup_backend_next_diagnostic(
        state,
        contract,
        tenant,
        request_path,
        body,
        step,
        action.as_ref(),
        &executor,
        &state_before,
        &state_after,
        &result,
    );
    Ok(render_setup_backend_contract_state(
        state, contract, tenant, stored,
    ))
}

#[allow(clippy::too_many_arguments)]
fn persist_setup_backend_next_diagnostic(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    request_path: &str,
    body: &Value,
    selected_step: &str,
    action: Option<&Value>,
    executor: &Value,
    state_before: &Value,
    state_after: &Value,
    result: &Value,
) -> Result<Value> {
    let executor_kind = executor
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let config = state_after
        .get("values")
        .and_then(|values| values.get("config"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let resolved_templates =
        setup_backend_resolved_executor_templates(state, tenant, &config, executor);
    let resolved_capability_route = setup_backend_resolved_capability_route(&resolved_templates);
    let result_body = result.get("result").cloned().unwrap_or(Value::Null);
    let upstream = setup_backend_upstream_diagnostic(&result_body);
    let event_detail = serde_json::json!({
        "providerId": contract.provider_id,
        "tenant": tenant,
        "team": state.team.clone().unwrap_or_else(|| "default".to_string()),
        "env": state.env,
        "request": {
            "method": "POST",
            "path": request_path,
            "body": body,
        },
        "selected_contract_step": selected_step,
        "selected_executor": {
            "kind": executor_kind,
            "action_id": action.and_then(|action| action.get("id")).cloned().unwrap_or(Value::Null),
            "action_label": action.and_then(|action| action.get("label")).cloned().unwrap_or(Value::Null),
        },
        "resolved_capability_route": resolved_capability_route,
        "resolved_templates": resolved_templates,
        "upstream_runtime_url": upstream.get("url").cloned().unwrap_or(Value::Null),
        "upstream_status": upstream.get("status").cloned().unwrap_or(Value::Null),
        "upstream_body": upstream.get("body").cloned().unwrap_or(Value::Null),
        "result": result,
        "setup_state_before": state_before,
        "setup_state_after": state_after,
    });
    persist_provider_setup_event(
        state,
        ProviderSetupEventRequest {
            provider_id: contract.provider_id.clone(),
            event_name: "greentic-provider-setup-backend-next".to_string(),
            event_detail,
            current_step_id: Some(Value::String(selected_step.to_string())),
            current_progress: state_after
                .get("setup_status")
                .and_then(|status| status.get("items"))
                .cloned(),
            action_name: action
                .and_then(|action| action.get("label").or_else(|| action.get("id")))
                .cloned(),
            request_method: Some(Value::String("POST".to_string())),
            request_path: Some(Value::String(request_path.to_string())),
            http_status: upstream
                .get("status")
                .cloned()
                .or_else(|| Some(Value::Number(serde_json::Number::from(200)))),
            response_body: Some(result_body),
            error: result
                .get("error")
                .or_else(|| result.get("next"))
                .filter(|_| result.get("ok").and_then(Value::as_bool) != Some(true))
                .cloned(),
            correlation_id: Some(provider_setup_event_detail_field(
                result,
                &[
                    "correlationId",
                    "correlation_id",
                    "trace_id",
                    "traceId",
                    "request-id",
                    "client-request-id",
                ],
            )),
            tenant: Some(tenant.to_string()),
            team: state.team.clone(),
            env: Some(state.env.clone()),
            setup_session_id: None,
            setup_ui_url: None,
        },
    )
}

fn setup_backend_resolved_executor_templates(
    state: &UiState,
    tenant: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> Value {
    let mut resolved = JsonMap::new();
    if let Some(object) = executor.as_object() {
        for (key, value) in object {
            if (key.ends_with("_template") || key == "url_template")
                && let Some(template) = value.as_str()
            {
                resolved.insert(
                    key.clone(),
                    Value::String(setup_backend_expand_template(
                        state, tenant, config, template,
                    )),
                );
            }
        }
    }
    Value::Object(resolved)
}

fn setup_backend_resolved_capability_route(resolved_templates: &Value) -> Value {
    resolved_templates
        .get("registration_url_template")
        .or_else(|| resolved_templates.get("url_template"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn setup_backend_upstream_diagnostic(result_body: &Value) -> Value {
    let registration = result_body.get("registration").unwrap_or(&Value::Null);
    serde_json::json!({
        "url": result_body
            .get("target")
            .or_else(|| result_body.get("url"))
            .or_else(|| result_body.get("registration_url"))
            .cloned()
            .unwrap_or(Value::Null),
        "status": registration
            .get("status")
            .cloned()
            .unwrap_or(Value::Null),
        "body": registration
            .get("body")
            .or_else(|| registration.get("response"))
            .cloned()
            .unwrap_or_else(|| registration.clone()),
    })
}

async fn setup_backend_contract_oauth_start(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    kind: &str,
    body: &Value,
) -> Result<Value> {
    let _ = setup_backend_contract_save_config(state, contract, tenant, body)?;
    let mut stored = load_setup_backend_contract_state(state, &contract.provider_id, tenant)?;
    ensure_setup_backend_config_defaults(state, tenant, &mut stored)?;
    let Some(action) = setup_backend_oauth_action(contract, kind) else {
        return Ok(setup_backend_action_error(
            "oauth_device_code",
            &format!("no oauth_device_code action declares oauth_kind {kind}"),
        ));
    };
    let result =
        setup_backend_execute_oauth_device_code_start(state, contract, tenant, &mut stored, action)
            .await?;
    crate::setup_backend_contract::record_action_result(
        &state.bundle_path,
        tenant,
        state.team.as_deref().unwrap_or("default"),
        &contract.provider_id,
        &mut stored,
        result.clone(),
    )?;
    Ok(result)
}

async fn setup_backend_contract_oauth_complete(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    kind: &str,
) -> Result<Value> {
    let mut stored = load_setup_backend_contract_state(state, &contract.provider_id, tenant)?;
    ensure_setup_backend_config_defaults(state, tenant, &mut stored)?;
    let Some(action) = setup_backend_oauth_action(contract, kind) else {
        return Ok(setup_backend_action_error(
            "oauth_device_code",
            &format!("no oauth_device_code action declares oauth_kind {kind}"),
        ));
    };
    let result = setup_backend_execute_oauth_device_code_complete(
        state,
        contract,
        tenant,
        &mut stored,
        action,
    )
    .await?;
    crate::setup_backend_contract::record_action_result(
        &state.bundle_path,
        tenant,
        state.team.as_deref().unwrap_or("default"),
        &contract.provider_id,
        &mut stored,
        result.clone(),
    )?;
    Ok(result)
}

async fn setup_backend_execute_action(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    step: &str,
) -> Result<Value> {
    let Some(action) = setup_backend_action_by_id(contract, step) else {
        return Ok(setup_backend_action_error(
            "missing_action",
            &format!("backend contract has no action for required step {step}"),
        ));
    };
    let kind = action
        .get("executor")
        .and_then(|executor| executor.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "oauth_device_code" => {
            setup_backend_execute_oauth_device_code_start(state, contract, tenant, stored, action)
                .await
        }
        "microsoft_graph_application" => {
            setup_backend_execute_graph_application(contract, tenant, stored, action).await
        }
        "provider_http" => {
            setup_backend_execute_provider_http(state, contract, tenant, stored, action).await
        }
        "microsoft_graph_teams_app_catalog_publish" => {
            setup_backend_execute_teams_app_publish(state, contract, tenant, stored, action).await
        }
        "microsoft_graph_teams_app_user_install" => {
            setup_backend_execute_teams_app_user_install(state, contract, tenant, stored, action)
                .await
        }
        "runtime_observation" => {
            setup_backend_execute_runtime_observation(state, contract, tenant, stored, action).await
        }
        "" => Ok(setup_backend_action_error(
            "missing_executor_kind",
            &format!("backend contract action {step} has no executor.kind"),
        )),
        other => Ok(setup_backend_action_error(
            other,
            &format!("setup backend executor kind is not implemented: {other}"),
        )),
    }
}

async fn setup_backend_execute_oauth_device_code_start(
    _state: &UiState,
    _contract: &ProviderBackendContract,
    _tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    let client_id = setup_backend_oauth_client_id(executor, config)?;
    if client_id.is_empty() {
        let client_id_key = required_executor_str(executor, "client_id_config_key")?;
        return Ok(setup_backend_step_result(
            action,
            false,
            &format!("set {client_id_key}, then retry"),
            serde_json::json!({
                "ok": false,
                "missing_config_key": client_id_key,
            }),
        ));
    }
    let authority_tenant = executor
        .get("authority_tenant_config_key")
        .and_then(Value::as_str)
        .map(|key| setup_backend_config_str(config, key))
        .filter(|value| !value.is_empty())
        .or_else(|| {
            executor
                .get("authority_tenant_default")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "organizations".to_string());
    let authority_template = required_executor_str(executor, "authority_url_template")?;
    let authority = authority_template.replace("{authority_tenant}", &authority_tenant);
    let scopes = executor
        .get("scopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    if scopes.trim().is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            "OAuth device-code action has no scopes.",
            serde_json::json!({ "ok": false, "error": "oauth_device_code executor missing scopes" }),
        ));
    }
    let device_url = format!("{}/oauth2/v2.0/devicecode", authority.trim_end_matches('/'));
    let token_url = format!("{}/oauth2/v2.0/token", authority.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let response = client
        .post(&device_url)
        .form(&[
            ("client_id", client_id.as_str()),
            ("scope", scopes.as_str()),
        ])
        .send()
        .await
        .context("OAuth device-code request failed")?;
    let status = response.status().as_u16();
    let body = response
        .json::<Value>()
        .await
        .context("failed to parse OAuth device-code response")?;
    if status >= 400 {
        return Ok(setup_backend_step_result(
            action,
            false,
            "OAuth device-code request failed.",
            serde_json::json!({ "ok": false, "http_status": status, "body": body }),
        ));
    }
    let device_code = body
        .get("device_code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if device_code.is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            "OAuth device-code response did not include a device code.",
            serde_json::json!({ "ok": false, "body": body }),
        ));
    }
    let oauth_kind = executor
        .get("oauth_kind")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let device_code_key = executor
        .get("device_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_device_code");
    let user_code_key = executor
        .get("user_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_user_code");
    config.insert(
        "oauth_kind".to_string(),
        Value::String(oauth_kind.to_string()),
    );
    config.insert(device_code_key.to_string(), Value::String(device_code));
    if let Some(user_code) = body.get("user_code").and_then(Value::as_str) {
        config.insert(
            user_code_key.to_string(),
            Value::String(user_code.to_string()),
        );
    }
    if let Some(verification_uri) = body
        .get("verification_uri")
        .or_else(|| body.get("verification_url"))
        .and_then(Value::as_str)
    {
        config.insert(
            "oauth_verification_uri".to_string(),
            Value::String(verification_uri.to_string()),
        );
    }
    config.insert("oauth_token_url".to_string(), Value::String(token_url));
    config.insert("oauth_client_id".to_string(), Value::String(client_id));
    let login = setup_backend_device_login_payload(config, user_code_key, &body);
    stored.insert(
        "last_oauth".to_string(),
        serde_json::json!({
            "kind": oauth_kind,
            "response": setup_backend_public_oauth_response(&body),
        }),
    );
    Ok(setup_backend_step_result(
        action,
        false,
        setup_backend_device_login_next_message(),
        serde_json::json!({
            "ok": false,
            "pending_device_login": true,
            "login": login,
            "body": setup_backend_public_oauth_response(&body),
        }),
    ))
}

async fn setup_backend_execute_oauth_device_code_complete(
    _state: &UiState,
    _contract: &ProviderBackendContract,
    _tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    let oauth_kind = executor
        .get("oauth_kind")
        .and_then(Value::as_str)
        .unwrap_or("default");
    let device_code_key = executor
        .get("device_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_device_code");
    let token_store_key = required_executor_str(executor, "token_store_key")?;
    let device_code = setup_backend_config_str(config, device_code_key);
    let client_id = setup_backend_config_str(config, "oauth_client_id");
    let token_url = setup_backend_config_str(config, "oauth_token_url");
    if device_code.is_empty() || client_id.is_empty() || token_url.is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            "start device login first",
            serde_json::json!({ "ok": false, "error": "device_login_not_started" }),
        ));
    }
    let client = reqwest::Client::new();
    let response = client
        .post(&token_url)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", client_id.as_str()),
            ("device_code", device_code.as_str()),
        ])
        .send()
        .await
        .context("OAuth device-code token polling failed")?;
    let status = response.status().as_u16();
    let body = response
        .json::<Value>()
        .await
        .context("failed to parse OAuth device-code token response")?;
    if let Some(error) = body.get("error").and_then(Value::as_str)
        && matches!(error, "authorization_pending" | "slow_down")
    {
        return Ok(setup_backend_step_result(
            action,
            false,
            "authorization is still pending",
            serde_json::json!({ "ok": false, "body": body }),
        ));
    }
    if status >= 400 || body.get("access_token").and_then(Value::as_str).is_none() {
        return Ok(setup_backend_step_result(
            action,
            false,
            "OAuth token polling failed.",
            serde_json::json!({ "ok": false, "http_status": status, "body": body }),
        ));
    }
    if let Some(token) = body.get("access_token").and_then(Value::as_str) {
        config.insert(
            token_store_key.to_string(),
            Value::String(token.to_string()),
        );
    }
    config.remove(device_code_key);
    let user_code_key = executor
        .get("user_code_store_key")
        .and_then(Value::as_str)
        .unwrap_or("oauth_user_code");
    config.remove(user_code_key);
    config.remove("oauth_kind");
    config.remove("oauth_client_id");
    config.remove("oauth_token_url");
    setup_backend_clear_oauth_resume_for_token(stored, token_store_key);
    let oauth = stored
        .entry("oauth".to_string())
        .or_insert_with(|| Value::Object(JsonMap::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored oauth state is not an object"))?;
    oauth.insert(
        oauth_kind.to_string(),
        serde_json::json!({
            "ok": true,
            "completed_at": setup_backend_timestamp_ms(),
            "token_store_key": token_store_key,
        }),
    );
    setup_backend_fund_linked_resource(stored, executor, &client_id, &token_url, &body).await?;
    Ok(setup_backend_step_result(
        action,
        true,
        "click again to continue setup",
        serde_json::json!({
            "ok": true,
            "persisted_keys": [token_store_key],
        }),
    ))
}

/// One sign-in funds both Microsoft tokens: if the just-completed device-code
/// executor declares `also_fund`, exchange its refresh token for the linked
/// resource token (e.g. Azure management) and mark that step's oauth state done,
/// so a second device login is unnecessary. Silent no-op on any failure — the
/// linked step then falls back to its own device-code login.
async fn setup_backend_fund_linked_resource(
    stored: &mut JsonMap<String, Value>,
    executor: &Value,
    client_id: &str,
    token_url: &str,
    token_response: &Value,
) -> Result<()> {
    let Some(also) = executor.get("also_fund").and_then(Value::as_object) else {
        return Ok(());
    };
    let Some(refresh_token) = token_response.get("refresh_token").and_then(Value::as_str) else {
        return Ok(());
    };
    let scopes = also
        .get("scopes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let token_store_key = also
        .get("token_store_key")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let oauth_kind = also
        .get("oauth_kind")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    if scopes.trim().is_empty()
        || token_store_key.is_empty()
        || client_id.is_empty()
        || token_url.is_empty()
    {
        return Ok(());
    }
    let client = reqwest::Client::new();
    let Ok(response) = client
        .post(token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
            ("scope", scopes.as_str()),
        ])
        .send()
        .await
    else {
        return Ok(());
    };
    let Ok(body) = response.json::<Value>().await else {
        return Ok(());
    };
    let Some(access_token) = body.get("access_token").and_then(Value::as_str) else {
        return Ok(());
    };
    let config = setup_backend_config_mut(stored)?;
    config.insert(
        token_store_key.clone(),
        Value::String(access_token.to_string()),
    );
    let oauth = stored
        .entry("oauth".to_string())
        .or_insert_with(|| Value::Object(JsonMap::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored oauth state is not an object"))?;
    oauth.insert(
        oauth_kind,
        serde_json::json!({
            "ok": true,
            "completed_at": setup_backend_timestamp_ms(),
            "token_store_key": token_store_key,
            "funded_by_refresh": true,
        }),
    );
    Ok(())
}

async fn setup_backend_execute_graph_application(
    contract: &ProviderBackendContract,
    _tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    crate::setup_backend_contract::execute_microsoft_graph_application(
        stored,
        &contract.provider_id,
        action,
    )
}

async fn setup_backend_execute_provider_http(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    setup_backend_apply_host_defaults(state, tenant, config);
    setup_backend_adopt_runtime_public_base_url(state, tenant, config)?;
    if setup_backend_public_base_url_needs_runtime(state, tenant, config)
        && let Err(err) = ensure_setup_runtime(state, tenant).await
    {
        let detail = setup_backend_error_chain(&err);
        return Ok(setup_backend_step_result(
            action,
            false,
            "runtime/tunnel not running",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "runtime/tunnel not running",
                "detail": detail,
            }),
        ));
    }
    if let Err(err) = setup_backend_refresh_public_tunnel_if_needed(state, tenant, config).await {
        let detail = setup_backend_error_chain(&err);
        return Ok(setup_backend_step_result(
            action,
            false,
            "runtime/tunnel not running",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "runtime/tunnel not running",
                "detail": detail,
            }),
        ));
    }

    let target = match setup_backend_provider_http_url(state, tenant, config, executor) {
        Ok(url) => url,
        Err(err) => {
            return Ok(setup_backend_step_result(
                action,
                false,
                &err.to_string(),
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": err.to_string(),
                }),
            ));
        }
    };
    if setup_backend_template_unresolved(&target) || target.trim().is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            "provider_http executor target could not be resolved",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "provider_http executor target could not be resolved",
                "target": target,
            }),
        ));
    }

    let method = executor
        .get("method")
        .and_then(Value::as_str)
        .and_then(|method| reqwest::Method::from_bytes(method.as_bytes()).ok())
        .unwrap_or(reqwest::Method::POST);
    let runtime_context = setup_backend_runtime_context(state, tenant, config);
    let payload = setup_backend_provider_http_payload(state, contract, tenant, config, action);
    if is_safe_same_origin_path(&target) {
        return setup_backend_execute_provider_http_local(
            state,
            stored,
            ProviderHttpLocalExecution {
                contract,
                provider_id: &contract.provider_id,
                tenant,
                action,
                target: &target,
                method: method.as_str(),
                payload,
                runtime_context,
            },
        )
        .await;
    }
    let client = reqwest::Client::new();
    let response =
        match setup_backend_json_request(&client, method, &target, None, Some(payload)).await {
            Ok(response) => response,
            Err(err) => {
                return Ok(setup_backend_step_result(
                    action,
                    false,
                    "Provider setup service is not running",
                    serde_json::json!({
                        "ok": false,
                        "blocked": true,
                        "error": "Provider setup service is not running",
                        "target": target,
                        "detail": err.to_string(),
                    }),
                ));
            }
        };
    let ok = response
        .get("body")
        .and_then(|body| body.get("ok"))
        .and_then(Value::as_bool)
        .unwrap_or_else(|| response.get("ok").and_then(Value::as_bool).unwrap_or(false));
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            action
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("last_provider_http")
        });
    let result = serde_json::json!({
        "ok": ok,
        "target": target,
        "response": response,
        "runtime_context": runtime_context,
    });
    if !ok
        && let Some(oauth_result) =
            setup_backend_provider_http_oauth_required_result(contract, action, &result)
    {
        return Ok(oauth_result);
    }
    stored.insert(state_key.to_string(), result.clone());
    Ok(setup_backend_step_result(
        action,
        ok,
        if ok {
            "click again to continue setup"
        } else {
            "fix provider setup endpoint and retry"
        },
        result,
    ))
}

async fn setup_backend_execute_provider_http_local(
    state: &UiState,
    stored: &mut JsonMap<String, Value>,
    execution: ProviderHttpLocalExecution<'_>,
) -> Result<Value> {
    let Some(route_match) = find_declared_provider_http_route(
        &state.bundle_path,
        execution.method,
        execution.target,
        execution.tenant,
        state.team.as_deref().unwrap_or("default"),
    )?
    else {
        let message = format!(
            "provider_http target {} is not declared by pack greentic.http-routes.v1",
            execution.target
        );
        return Ok(setup_backend_step_result(
            execution.action,
            false,
            &message,
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": message,
                "target": execution.target,
                "provider_id": execution.provider_id,
            }),
        ));
    };
    let body = Bytes::from(serde_json::to_vec(&execution.payload)?);
    let headers = HeaderMap::new();
    let response = invoke_declared_provider_http_route(
        state,
        &route_match,
        execution.method,
        execution.target,
        "",
        &headers,
        body,
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(err) => {
            return Ok(setup_backend_step_result(
                execution.action,
                false,
                "Provider pack route failed",
                serde_json::json!({
                    "ok": false,
                    "blocked": true,
                    "error": err.to_string(),
                    "target": execution.target,
                    "provider_id": execution.provider_id,
                }),
            ));
        }
    };
    let ok = response
        .get("ok")
        .and_then(Value::as_bool)
        .or_else(|| {
            response
                .get("response")
                .and_then(|response| response.get("body_json"))
                .and_then(|body| body.get("ok"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(true);
    let state_key = execution
        .action
        .get("executor")
        .and_then(|executor| executor.get("state_store_key"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            execution
                .action
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("last_provider_http")
        });
    let result = serde_json::json!({
        "ok": ok,
        "target": execution.target,
        "response": response,
        "runtime_context": execution.runtime_context,
    });
    if !ok
        && let Some(oauth_result) = setup_backend_provider_http_oauth_required_result(
            execution.contract,
            execution.action,
            &result,
        )
    {
        return Ok(oauth_result);
    }
    stored.insert(state_key.to_string(), result.clone());
    Ok(setup_backend_step_result(
        execution.action,
        ok,
        if ok {
            "click again to continue setup"
        } else {
            "fix provider setup route and retry"
        },
        result,
    ))
}

async fn setup_backend_execute_teams_app_publish(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    {
        let config = setup_backend_config_mut(stored)?;
        setup_backend_apply_host_defaults(state, tenant, config);
        setup_backend_adopt_runtime_public_base_url(state, tenant, config)?;
    }
    let provider_pack = setup_backend_provider_pack_path(state, &contract.provider_id)?;
    crate::setup_backend_contract::execute_microsoft_graph_teams_app_catalog_publish(
        &provider_pack,
        stored,
        tenant,
        state.team.as_deref().unwrap_or("default"),
        &state.env,
        action,
    )
}

async fn setup_backend_execute_teams_app_user_install(
    state: &UiState,
    _contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    {
        let config = setup_backend_config_mut(stored)?;
        setup_backend_apply_host_defaults(state, tenant, config);
        setup_backend_adopt_runtime_public_base_url(state, tenant, config)?;
    }
    crate::setup_backend_contract::execute_microsoft_graph_teams_app_user_install(
        stored,
        tenant,
        state.team.as_deref().unwrap_or("default"),
        &state.env,
        action,
    )
}

async fn setup_backend_execute_runtime_observation(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    if let Err(err) =
        setup_backend_prepare_runtime_public_base_for_observation(state, tenant, config).await
    {
        return Ok(setup_backend_runtime_blocked_result(
            action,
            setup_backend_error_chain(&err),
        ));
    }
    if let Err(err) = setup_backend_refresh_public_tunnel_if_needed(state, tenant, config).await {
        let detail = setup_backend_error_chain(&err);
        return Ok(setup_backend_step_result(
            action,
            false,
            "runtime/tunnel not running",
            serde_json::json!({
                "ok": false,
                "blocked": true,
                "error": "runtime/tunnel not running",
                "detail": detail,
            }),
        ));
    }
    let runtime_context = setup_backend_runtime_context(state, tenant, config);
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_activity");
    if let Some(blocked) = setup_backend_runtime_context_blocked(state, &runtime_context).await {
        return Ok(setup_backend_step_result(
            action,
            false,
            "runtime/tunnel not running",
            blocked,
        ));
    }
    setup_backend_refresh_runtime_observation_from_runtime_logs(
        state,
        tenant,
        executor,
        stored,
        state_key,
        &runtime_context,
    )?;
    setup_backend_refresh_runtime_observation_from_provider_state(
        state,
        contract,
        tenant,
        stored,
        state_key,
        &runtime_context,
    )
    .await?;
    if stored
        .get(state_key)
        .is_some_and(|value| setup_backend_runtime_context_current(value, &runtime_context))
    {
        return Ok(setup_backend_step_result(
            action,
            true,
            "runtime observation is present",
            serde_json::json!({
                "ok": true,
                "state_store_key": state_key,
                "runtime_context": runtime_context,
            }),
        ));
    }
    Ok(setup_backend_step_result(
        action,
        false,
        "waiting for runtime observation",
        serde_json::json!({
            "ok": false,
            "waiting": true,
            "blocked": true,
            "retryable": true,
            "error": "runtime observation not present yet",
            "detail": "runtime and tunnel are running, but no current observation has been recorded yet",
            "provider_id": executor
                .get("provider_id")
                .and_then(Value::as_str)
                .unwrap_or(&contract.provider_id),
            "source": executor.get("source").cloned().unwrap_or(Value::Null),
            "event": executor.get("event").cloned().unwrap_or(Value::Null),
            "state_store_key": state_key,
            "runtime_context": runtime_context,
        }),
    ))
}

fn setup_backend_refresh_runtime_observation_from_runtime_logs(
    state: &UiState,
    tenant: &str,
    executor: &Value,
    stored: &mut JsonMap<String, Value>,
    state_key: &str,
    runtime_context: &Value,
) -> Result<()> {
    if executor.get("source").and_then(Value::as_str) != Some("greentic-start")
        || executor.get("event").and_then(Value::as_str) != Some("bot_framework_activity_received")
    {
        return Ok(());
    }
    let Some(observed) = setup_backend_latest_runtime_activity_from_logs(
        state,
        tenant,
        state.team.as_deref().unwrap_or("default"),
    )?
    else {
        return Ok(());
    };
    let observed = setup_backend_observation_with_runtime_context(observed, runtime_context);
    setup_backend_store_runtime_observation(stored, state_key, observed);
    Ok(())
}

fn setup_backend_latest_runtime_activity_from_logs(
    state: &UiState,
    tenant: &str,
    team: &str,
) -> Result<Option<Value>> {
    let log_path = state.bundle_path.join("logs").join("system.log");
    let file = match std::fs::File::open(&log_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("read {}", log_path.display())),
    };
    let line_floor = setup_backend_setup_runtime_info(state)
        .and_then(|info| info.system_log_line_floor)
        .unwrap_or(0);
    let team_some = format!("team=Some(\"{team}\")");
    let team_plain = format!("team={team}");
    let tenant_match = format!("tenant={tenant}");
    let mut observed = None;
    for (index, line) in std::io::BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .enumerate()
    {
        if index < line_floor {
            continue;
        }
        if line.contains("[fast2flow:gate] enter")
            && line.contains(&tenant_match)
            && (line.contains(&team_some) || line.contains(&team_plain))
        {
            observed = Some(serde_json::json!({
                "source": "greentic-start",
                "event": "bot_framework_activity_received",
                "tenant": tenant,
                "team": team,
                "log_path": log_path,
                "log_line": index + 1,
            }));
        }
    }
    Ok(observed)
}

async fn setup_backend_refresh_runtime_observation_from_provider_state(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    state_key: &str,
    runtime_context: &Value,
) -> Result<()> {
    let Some((method, path)) =
        setup_backend_contract_state_route(contract, tenant, state.team.as_deref())
    else {
        return Ok(());
    };
    let Some(route_match) = find_declared_provider_http_route(
        &state.bundle_path,
        &method,
        &path,
        tenant,
        state.team.as_deref().unwrap_or("default"),
    )?
    else {
        return Ok(());
    };
    if !matches!(
        route_match.route.target,
        ProviderHttpRouteTarget::SetupComponent { .. }
    ) {
        return Ok(());
    }
    let response = invoke_declared_provider_http_route(
        state,
        &route_match,
        &method,
        &path,
        "",
        &HeaderMap::new(),
        Bytes::new(),
    )
    .await?;
    let Some(observed) = response
        .get("values")
        .and_then(|values| values.get(state_key))
        .filter(|value| !value.is_null())
        .cloned()
    else {
        return Ok(());
    };
    let observed = setup_backend_observation_with_runtime_context(observed, runtime_context);
    setup_backend_store_runtime_observation(stored, state_key, observed);
    Ok(())
}

fn setup_backend_contract_state_route(
    contract: &ProviderBackendContract,
    tenant: &str,
    team: Option<&str>,
) -> Option<(String, String)> {
    let route = contract
        .inline
        .get("routes")
        .and_then(|routes| routes.get("state"))
        .and_then(Value::as_str)
        .or_else(|| contract.inline.get("base_path").and_then(Value::as_str))?;
    let (method, path) = match route.split_once(' ') {
        Some((method, path)) => (method.trim(), path.trim()),
        None => ("GET", route.trim()),
    };
    let team = team.unwrap_or("default");
    Some((
        method.to_string(),
        path.replace("{tenant}", tenant).replace("{team}", team),
    ))
}

fn setup_backend_observation_with_runtime_context(
    observed: Value,
    runtime_context: &Value,
) -> Value {
    let observed_at = Value::Number(serde_json::Number::from(setup_backend_timestamp_ms() as u64));
    match observed {
        Value::Object(mut object) => {
            object.insert("runtime_context".to_string(), runtime_context.clone());
            object
                .entry("last_activity_received_at".to_string())
                .or_insert_with(|| observed_at.clone());
            Value::Object(object)
        }
        value => serde_json::json!({
            "value": value,
            "runtime_context": runtime_context,
            "last_activity_received_at": observed_at,
        }),
    }
}

fn setup_backend_store_runtime_observation(
    stored: &mut JsonMap<String, Value>,
    state_key: &str,
    observed: Value,
) {
    if state_key == "last_activity"
        && let Some(received_at) = observed.get("last_activity_received_at").cloned()
    {
        stored.insert("last_activity_received_at".to_string(), received_at);
    }
    stored.insert(state_key.to_string(), observed);
}

fn setup_backend_action_by_id<'a>(
    contract: &'a ProviderBackendContract,
    id: &str,
) -> Option<&'a Value> {
    contract
        .inline
        .get("actions")
        .and_then(Value::as_array)?
        .iter()
        .find(|action| action.get("id").and_then(Value::as_str) == Some(id))
}

fn setup_backend_oauth_action<'a>(
    contract: &'a ProviderBackendContract,
    kind: &str,
) -> Option<&'a Value> {
    contract
        .inline
        .get("actions")
        .and_then(Value::as_array)?
        .iter()
        .find(|action| {
            let Some(executor) = action.get("executor").and_then(Value::as_object) else {
                return false;
            };
            executor.get("kind").and_then(Value::as_str) == Some("oauth_device_code")
                && executor.get("oauth_kind").and_then(Value::as_str) == Some(kind)
        })
}

fn setup_backend_executor(action: &Value) -> Result<&Value> {
    action
        .get("executor")
        .ok_or_else(|| anyhow!("setup backend action missing executor"))
}

fn required_executor_str<'a>(executor: &'a Value, key: &str) -> Result<&'a str> {
    executor
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("setup backend executor missing {key}"))
}

fn setup_backend_config_mut(
    stored: &mut JsonMap<String, Value>,
) -> Result<&mut JsonMap<String, Value>> {
    stored
        .entry("config".to_string())
        .or_insert_with(|| Value::Object(JsonMap::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored config is not an object"))
}

fn setup_backend_config_str(config: &JsonMap<String, Value>, key: &str) -> String {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn setup_backend_oauth_client_id(
    executor: &Value,
    config: &JsonMap<String, Value>,
) -> Result<String> {
    let client_id_key = required_executor_str(executor, "client_id_config_key")?;
    let configured_client_id = setup_backend_config_str(config, client_id_key);
    if !configured_client_id.is_empty() {
        return Ok(configured_client_id);
    }
    Ok(executor
        .get("client_id_default")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string())
}

fn setup_backend_step_result(action: &Value, ok: bool, next: &str, result: Value) -> Value {
    crate::setup_backend_contract::step_result(action, ok, next, result)
}

fn setup_backend_runtime_blocked_result(action: &Value, detail: String) -> Value {
    setup_backend_step_result(
        action,
        false,
        "runtime/tunnel not running",
        serde_json::json!({
            "ok": false,
            "blocked": true,
            "error": "runtime/tunnel not running",
            "detail": detail,
        }),
    )
}

fn setup_backend_action_error(kind: &str, message: &str) -> Value {
    serde_json::json!({
        "ok": false,
        "error": message,
        "executor_kind": kind,
        "next": message,
        "result": {
            "ok": false,
            "unsupported": kind != "missing_action" && kind != "missing_executor_kind",
            "executor_kind": kind,
            "error": message,
        }
    })
}

fn setup_backend_error_chain(err: &anyhow::Error) -> String {
    err.chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn setup_backend_public_oauth_response(body: &Value) -> Value {
    let mut public = body.clone();
    if let Some(obj) = public.as_object_mut() {
        obj.remove("device_code");
        obj.remove("access_token");
        obj.remove("refresh_token");
        obj.remove("id_token");
    }
    public
}

fn setup_backend_device_login_payload(
    config: &JsonMap<String, Value>,
    user_code_key: &str,
    body: &Value,
) -> Value {
    let interval = body.get("interval").and_then(Value::as_u64).unwrap_or(5);
    let expires_in = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(900);
    serde_json::json!({
        "url": setup_backend_config_str(config, "oauth_verification_uri"),
        "userCode": setup_backend_config_str(config, user_code_key),
        "user_code": setup_backend_config_str(config, user_code_key),
        "interval": interval,
        "expiresIn": expires_in,
    })
}

fn setup_backend_timestamp_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn setup_backend_expand_template(
    state: &UiState,
    tenant: &str,
    config: &JsonMap<String, Value>,
    template: &str,
) -> String {
    let mut expanded = template
        .replace("{tenant}", tenant)
        .replace("{team}", state.team.as_deref().unwrap_or("default"))
        .replace("{env}", &state.env);
    if expanded.contains("{public_base_url}") {
        let public_base = setup_backend_config_str(config, "public_base_url")
            .trim_end_matches('/')
            .to_string();
        expanded = expanded.replace("{public_base_url}", &public_base);
    }
    for (key, value) in config {
        if let Some(value) = value.as_str() {
            expanded = expanded.replace(&format!("{{{key}}}"), value);
        }
    }
    expanded
}

fn setup_backend_provider_http_url(
    state: &UiState,
    tenant: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> Result<String> {
    if let Some(template) = executor
        .get("url_template")
        .or_else(|| executor.get("target_url_template"))
        .and_then(Value::as_str)
    {
        let url = setup_backend_expand_template(state, tenant, config, template);
        validate_setup_backend_provider_http_url(&url)?;
        return Ok(url);
    }

    let path_template = executor
        .get("path_template")
        .or_else(|| executor.get("target_path_template"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("provider_http executor requires url_template or path_template"))?;
    let path = setup_backend_expand_template(state, tenant, config, path_template);
    if !is_safe_same_origin_path(&path) {
        anyhow::bail!("provider_http executor path_template must resolve to a safe absolute path");
    }
    Ok(path)
}

fn validate_setup_backend_provider_http_url(url: &str) -> Result<()> {
    let parsed =
        Url::parse(url).with_context(|| format!("provider_http target is not a URL: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        anyhow::bail!("provider_http target must be an http(s) URL");
    }
    Ok(())
}

fn setup_backend_provider_http_payload(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    config: &JsonMap<String, Value>,
    action: &Value,
) -> Value {
    let executor = action.get("executor").unwrap_or(&Value::Null);
    if let Some(template) = executor
        .get("body")
        .or_else(|| executor.get("body_template"))
        .or_else(|| executor.get("request_body"))
    {
        return setup_backend_expand_json_template(state, tenant, config, template);
    }
    serde_json::json!({
        "provider_id": contract.provider_id,
        "tenant": tenant,
        "team": state.team.clone().unwrap_or_else(|| "default".to_string()),
        "env": state.env,
        "step": action.get("id").cloned().unwrap_or(Value::Null),
        "config": config,
    })
}

fn setup_backend_expand_json_template(
    state: &UiState,
    tenant: &str,
    config: &JsonMap<String, Value>,
    value: &Value,
) -> Value {
    match value {
        Value::String(template) => Value::String(setup_backend_expand_template(
            state, tenant, config, template,
        )),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| setup_backend_expand_json_template(state, tenant, config, item))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        setup_backend_expand_json_template(state, tenant, config, value),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

async fn setup_backend_json_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> Result<Value> {
    let mut request = client.request(method, url);
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("request failed: {url}"))?;
    let status = response.status().as_u16();
    let body = response.json::<Value>().await.unwrap_or(Value::Null);
    Ok(serde_json::json!({
        "ok": status < 400,
        "status": status,
        "body": body,
    }))
}

fn setup_backend_oauth_required_result(
    action: &Value,
    token_key: &str,
    reason: &str,
    response: &Value,
) -> Option<Value> {
    if !setup_backend_response_needs_oauth(response) {
        return None;
    }
    Some(setup_backend_step_result(
        action,
        false,
        &format!("complete OAuth for {token_key}, then retry"),
        serde_json::json!({
            "ok": false,
            "blocked": true,
            "error": "oauth_required",
            "reason": reason,
            "token_store_key": token_key,
            "resume_step": action.get("id").and_then(Value::as_str).unwrap_or_default(),
            "previous": response,
        }),
    ))
}

fn setup_backend_provider_http_oauth_required_result(
    contract: &ProviderBackendContract,
    action: &Value,
    response: &Value,
) -> Option<Value> {
    if !setup_backend_response_needs_oauth(response) {
        return None;
    }
    let token_key = setup_backend_oauth_token_key_for_action(contract, action, response)?;
    setup_backend_oauth_required_result(action, &token_key, "provider setup auth failed", response)
}

fn setup_backend_response_needs_oauth(response: &Value) -> bool {
    let status = response.get("status").and_then(Value::as_u64).unwrap_or(0);
    let body = response.get("body").unwrap_or(response);
    let text = body.to_string().to_ascii_lowercase();
    let says_unauthorized = status == 401
        || text.contains("http 401")
        || text.contains("status 401")
        || text.contains("unauthorized")
        || text.contains("invalid token");
    let says_expired = text.contains("expired")
        || text.contains("expiry")
        || text.contains("token exp")
        || text.contains("lifetime validation failed")
        || text.contains("token is expired");
    let says_invalid = text.contains("invalid token")
        || text.contains("access token is invalid")
        || text.contains("invalid access token");
    says_unauthorized && (says_expired || says_invalid)
}

fn setup_backend_oauth_token_key_for_action(
    contract: &ProviderBackendContract,
    action: &Value,
    response: &Value,
) -> Option<String> {
    if let Some(token_key) = action
        .get("executor")
        .and_then(|executor| {
            executor
                .get("token_store_key")
                .or_else(|| executor.get("auth_token_store_key"))
                .or_else(|| executor.get("oauth_token_store_key"))
        })
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(token_key.to_string());
    }
    if let Some(token_key) = response
        .get("token_store_key")
        .or_else(|| response.get("missing_token_store_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(token_key.to_string());
    }
    let oauth_token_keys = setup_backend_oauth_token_store_keys(contract);
    let mut action_matches = oauth_token_keys
        .iter()
        .filter(|token_key| setup_backend_value_mentions_token_key(action, token_key))
        .cloned()
        .collect::<Vec<_>>();
    action_matches.sort();
    action_matches.dedup();
    if action_matches.len() == 1 {
        return action_matches.into_iter().next();
    }
    if oauth_token_keys.len() == 1 {
        return oauth_token_keys.into_iter().next();
    }
    None
}

fn setup_backend_oauth_token_store_keys(contract: &ProviderBackendContract) -> Vec<String> {
    let mut keys = contract
        .inline
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| action.get("executor"))
        .filter(|executor| {
            executor.get("kind").and_then(Value::as_str) == Some("oauth_device_code")
        })
        .filter_map(|executor| executor.get("token_store_key").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    keys
}

fn setup_backend_value_mentions_token_key(value: &Value, token_key: &str) -> bool {
    match value {
        Value::String(text) => text == token_key || text.contains(&format!("{{{token_key}}}")),
        Value::Array(items) => items
            .iter()
            .any(|item| setup_backend_value_mentions_token_key(item, token_key)),
        Value::Object(object) => object.iter().any(|(key, value)| {
            key == token_key
                || key.contains(token_key)
                || setup_backend_value_mentions_token_key(value, token_key)
        }),
        _ => false,
    }
}

fn setup_backend_clear_oauth_resume_for_token(
    stored: &mut JsonMap<String, Value>,
    token_store_key: &str,
) {
    crate::setup_backend_contract::clear_oauth_resume_for_token(stored, token_store_key);
}

fn setup_backend_provider_pack_path(state: &UiState, provider_id: &str) -> Result<PathBuf> {
    let discovered = discovery::discover(&state.bundle_path)?;
    let provider = discovered
        .find_setup_target(provider_id)
        .ok_or_else(|| anyhow!("provider pack not found for setup backend asset"))?;
    Ok(provider.pack_path.clone())
}

fn load_setup_backend_contract_state(
    state: &UiState,
    provider_id: &str,
    tenant: &str,
) -> Result<JsonMap<String, Value>> {
    let team = state.team.as_deref().unwrap_or("default");
    let mut stored = crate::setup_backend_contract::load_backend_state(
        &state.bundle_path,
        &state.env,
        tenant,
        team,
        provider_id,
    )?;
    ensure_setup_backend_config_defaults(state, tenant, &mut stored)?;
    Ok(stored)
}

fn save_setup_backend_contract_state(
    state: &UiState,
    provider_id: &str,
    tenant: &str,
    stored: &JsonMap<String, Value>,
) -> Result<()> {
    let team = state.team.as_deref().unwrap_or("default");
    crate::setup_backend_contract::save_backend_state(
        &state.bundle_path,
        tenant,
        team,
        provider_id,
        stored,
    )?;
    Ok(())
}

fn ensure_setup_backend_config_defaults(
    state: &UiState,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
) -> Result<()> {
    let defaults = default_setup_backend_config(state, tenant);
    let config = stored
        .entry("config".to_string())
        .or_insert_with(|| Value::Object(JsonMap::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("stored config is not an object"))?;
    for (key, value) in defaults {
        if setup_backend_host_default_overrides_empty(&key)
            && config
                .get(&key)
                .and_then(Value::as_str)
                .is_some_and(|value| value.trim().is_empty())
        {
            config.insert(key, value);
        } else {
            config.entry(key).or_insert(value);
        }
    }
    Ok(())
}

fn setup_backend_host_default_overrides_empty(key: &str) -> bool {
    matches!(key, "public_base_url")
}

fn default_setup_backend_config(state: &UiState, tenant: &str) -> JsonMap<String, Value> {
    let runtime_base = configured_runtime_proxy_base_url()
        .or_else(|| setup_backend_runtime_local_base_url(state, tenant));
    default_setup_backend_config_with_runtime_base(state, tenant, runtime_base.as_deref())
}

fn default_setup_backend_config_with_runtime_base(
    state: &UiState,
    tenant: &str,
    runtime_base: Option<&str>,
) -> JsonMap<String, Value> {
    let mut config = JsonMap::new();
    config.insert("tenant".to_string(), Value::String(tenant.to_string()));
    config.insert(
        "team".to_string(),
        Value::String(state.team.clone().unwrap_or_else(|| "default".to_string())),
    );
    config.insert("env".to_string(), Value::String(state.env.clone()));
    setup_backend_apply_host_defaults_with_runtime_base(state, tenant, runtime_base, &mut config);
    config
}

fn setup_backend_apply_host_defaults(
    state: &UiState,
    tenant: &str,
    config: &mut JsonMap<String, Value>,
) {
    setup_backend_apply_host_defaults_with_runtime_base(
        state,
        tenant,
        configured_runtime_proxy_base_url().as_deref(),
        config,
    );
}

fn setup_backend_apply_host_defaults_with_runtime_base(
    state: &UiState,
    tenant: &str,
    _runtime_base: Option<&str>,
    config: &mut JsonMap<String, Value>,
) {
    setup_backend_refresh_ephemeral_public_base_url(state, tenant, config);
    if setup_backend_config_str(config, "public_base_url").is_empty()
        && let Some(public_base_url) = setup_backend_public_base_url(state, tenant)
    {
        config.insert(
            "public_base_url".to_string(),
            Value::String(public_base_url),
        );
    }
}

async fn setup_backend_prepare_runtime_public_base_for_observation(
    state: &UiState,
    tenant: &str,
    config: &mut JsonMap<String, Value>,
) -> Result<Option<String>> {
    if setup_backend_bundle_runtime_startable(state) {
        ensure_setup_runtime(state, tenant).await?;
    }
    setup_backend_adopt_runtime_public_base_url(state, tenant, config)
}

fn setup_backend_bundle_runtime_startable(state: &UiState) -> bool {
    state.bundle_path.is_dir()
        && (state.bundle_path.join("bundle.yaml").is_file()
            || state.bundle_path.join("greentic.yaml").is_file()
            || state.bundle_path.join("bundle.json").is_file())
}

fn setup_backend_adopt_runtime_public_base_url(
    state: &UiState,
    tenant: &str,
    config: &mut JsonMap<String, Value>,
) -> Result<Option<String>> {
    let Some(runtime_public_base_url) = setup_backend_runtime_public_base_url(state, tenant) else {
        return Ok(None);
    };
    if !setup_backend_public_base_url_is_external_https(&runtime_public_base_url)
        && !is_ephemeral_tunnel_public_base_url(&runtime_public_base_url)
    {
        return Ok(None);
    }
    let current = setup_backend_config_str(config, "public_base_url");
    if current.trim_end_matches('/') == runtime_public_base_url {
        return Ok(None);
    }
    if current.is_empty() || is_ephemeral_tunnel_public_base_url(&current) {
        config.insert(
            "public_base_url".to_string(),
            Value::String(runtime_public_base_url.clone()),
        );
        return Ok(Some(runtime_public_base_url));
    }
    Ok(None)
}

fn setup_backend_public_base_url_needs_runtime(
    state: &UiState,
    _tenant: &str,
    config: &JsonMap<String, Value>,
) -> bool {
    if !setup_backend_bundle_runtime_startable(state) {
        return false;
    }
    let current = setup_backend_config_str(config, "public_base_url");
    current.is_empty()
        || is_ephemeral_tunnel_public_base_url(&current)
        || !setup_backend_public_base_url_is_external_https(&current)
}

fn setup_backend_refresh_ephemeral_public_base_url(
    state: &UiState,
    tenant: &str,
    config: &mut JsonMap<String, Value>,
) {
    let current = setup_backend_config_str(config, "public_base_url");
    if current.is_empty() || !is_ephemeral_tunnel_public_base_url(&current) {
        return;
    }
    let Some(active) = setup_backend_runtime_public_base_url(state, tenant)
        .or_else(|| setup_backend_active_tunnel_public_base_url(state))
    else {
        return;
    };
    // Never downgrade a configured ephemeral tunnel to a non-public (e.g.
    // localhost) runtime base — an externally supplied tunnel stays authoritative.
    if !setup_backend_public_base_url_is_external_https(&active)
        && !is_ephemeral_tunnel_public_base_url(&active)
    {
        return;
    }
    if active != current.trim_end_matches('/') {
        config.insert("public_base_url".to_string(), Value::String(active));
    }
}

async fn setup_backend_refresh_public_tunnel_if_needed(
    state: &UiState,
    tenant: &str,
    config: &mut JsonMap<String, Value>,
) -> Result<Option<String>> {
    let current = setup_backend_config_str(config, "public_base_url");
    // Honor an already-reachable public base URL (e.g. an operator-supplied
    // tunnel) instead of starting and probing a fresh one.
    if !current.is_empty()
        && (is_ephemeral_tunnel_public_base_url(&current)
            || setup_backend_public_base_url_is_external_https(&current))
        && setup_backend_public_tunnel_responds(&current).await
    {
        return Ok(None);
    }
    let Some(mode) = setup_backend_tunnel_mode(state)?
        .or_else(|| setup_backend_infer_tunnel_mode_from_public_base_url(&current))
    else {
        return Ok(None);
    };
    if !matches!(mode.as_str(), "cloudflared" | "ngrok") {
        return Ok(None);
    }
    if !current.is_empty()
        && !is_ephemeral_tunnel_public_base_url(&current)
        && setup_backend_public_base_url_is_external_https(&current)
    {
        return Ok(None);
    }
    if let Some(runtime_public_base_url) = setup_backend_runtime_public_base_url(state, tenant) {
        if !setup_backend_public_base_url_is_external_https(&runtime_public_base_url)
            && !is_ephemeral_tunnel_public_base_url(&runtime_public_base_url)
        {
            // A setup-started runtime may persist its local HTTP base as the runtime
            // "public" URL when its own tunnel is disabled. Provider webhooks still
            // need a setup-owned public tunnel to that runtime.
        } else if is_ephemeral_tunnel_public_base_url(&runtime_public_base_url)
            && !setup_backend_public_tunnel_responds(&runtime_public_base_url).await
        {
            setup_backend_clear_setup_tunnel(state);
        } else {
            if current.trim_end_matches('/') == runtime_public_base_url {
                return Ok(None);
            }
            config.insert(
                "public_base_url".to_string(),
                Value::String(runtime_public_base_url.clone()),
            );
            return Ok(Some(runtime_public_base_url));
        }
    }
    let local_base_url = setup_backend_tunnel_local_base_url(state, tenant)?;
    let public_base_url = ensure_setup_tunnel(state, &mode, &local_base_url).await?;
    config.insert(
        "public_base_url".to_string(),
        Value::String(public_base_url.clone()),
    );
    Ok(Some(public_base_url))
}

fn setup_backend_public_base_url_is_external_https(public_base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(public_base_url.trim()) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    !matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn setup_backend_infer_tunnel_mode_from_public_base_url(public_base_url: &str) -> Option<String> {
    let url = url::Url::parse(public_base_url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    if host == "trycloudflare.com" || host.ends_with(".trycloudflare.com") {
        return Some("cloudflared".to_string());
    }
    if host.ends_with(".ngrok-free.app") || host.ends_with(".ngrok.io") {
        return Some("ngrok".to_string());
    }
    None
}

fn setup_backend_tunnel_mode(state: &UiState) -> Result<Option<String>> {
    if let Some(mode) = crate::platform_setup::load_tunnel_artifact(&state.bundle_path)?
        .and_then(|answers| answers.mode)
        .map(|mode| mode.trim().to_string())
        .filter(|mode| !mode.is_empty())
    {
        return Ok(Some(mode));
    }

    setup_backend_default_tunnel_mode(state)
}

fn setup_backend_default_tunnel_mode(state: &UiState) -> Result<Option<String>> {
    if prefill_has_cloud_deployment_targets(state.prefill_answers.as_ref()) {
        return Ok(Some("off".to_string()));
    }

    let deployment_targets =
        crate::deployment_targets::reconcile_deployment_targets(&state.bundle_path, &[])
            .with_context(|| {
                format!(
                    "resolve deployment targets for {}",
                    state.bundle_path.display()
                )
            })?;
    if deployment_targets
        .iter()
        .any(|record| matches!(record.target.as_str(), "aws" | "gcp" | "azure"))
    {
        return Ok(Some("off".to_string()));
    }

    let deployer_candidates =
        crate::deployment_targets::discover_deployer_pack_candidates(&state.bundle_path)
            .with_context(|| {
                format!(
                    "discover deployer packs for {}",
                    state.bundle_path.display()
                )
            })?;
    if deployer_candidates.is_empty() {
        return Ok(Some("cloudflared".to_string()));
    }

    Ok(None)
}

fn setup_backend_tunnel_local_base_url(state: &UiState, tenant: &str) -> Result<String> {
    configured_runtime_proxy_base_url()
        .or_else(|| setup_backend_runtime_local_base_url(state, tenant))
        .or_else(|| Some(state.local_base_url.trim_end_matches('/').to_string()))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("no local base URL available for setup tunnel"))
}

fn setup_backend_public_base_url(state: &UiState, tenant: &str) -> Option<String> {
    if let Some(value) = std::env::var("GREENTIC_SETUP_PUBLIC_BASE_URL")
        .ok()
        .or_else(|| std::env::var("GREENTIC_PUBLIC_BASE_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(value);
    }
    if let Some(value) = setup_backend_runtime_public_base_url(state, tenant) {
        return Some(value);
    }
    if let Some(value) = setup_backend_active_tunnel_public_base_url(state) {
        return Some(value);
    }
    crate::platform_setup::load_effective_static_routes_defaults(
        &state.bundle_path,
        tenant,
        state.team.as_deref(),
    )
    .ok()
    .flatten()
    .and_then(|policy| policy.public_base_url)
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty())
    .or_else(configured_runtime_proxy_base_url)
}

fn setup_backend_active_tunnel_public_base_url(state: &UiState) -> Option<String> {
    let mut guard = state.setup_tunnel.lock().ok()?;
    let tunnel = guard.as_mut()?;
    if !tunnel.is_running() {
        *guard = None;
        return None;
    }
    Some(tunnel.public_base_url.trim_end_matches('/').to_string()).filter(|value| !value.is_empty())
}

fn setup_backend_runtime_public_base_url(state: &UiState, tenant: &str) -> Option<String> {
    if let Some(value) = setup_backend_setup_runtime_info(state)
        .and_then(|info| info.public_base_url)
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(value);
    }
    crate::platform_setup::load_runtime_public_base_url(
        &state.bundle_path,
        tenant,
        state.team.as_deref(),
    )
    .ok()
    .flatten()
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty())
}

fn setup_backend_runtime_local_base_url(state: &UiState, tenant: &str) -> Option<String> {
    if let Some(value) = setup_backend_setup_runtime_info(state)
        .and_then(|info| info.local_base_url)
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(value);
    }
    crate::platform_setup::load_runtime_local_base_url(
        &state.bundle_path,
        tenant,
        state.team.as_deref(),
    )
    .ok()
    .flatten()
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty())
}

fn setup_backend_setup_runtime_info(state: &UiState) -> Option<SetupRuntimeInfo> {
    let guard = state.setup_runtime.lock().ok()?;
    let runtime = guard.as_ref()?;
    runtime.info.lock().ok().map(|info| info.clone())
}

fn setup_backend_runtime_context(
    state: &UiState,
    tenant: &str,
    config: &JsonMap<String, Value>,
) -> Value {
    let public_base_url = setup_backend_config_str(config, "public_base_url")
        .trim_end_matches('/')
        .to_string();
    let runtime_public_base_url =
        setup_backend_runtime_public_base_url(state, tenant).filter(|value| {
            setup_backend_public_base_url_is_external_https(value)
                || is_ephemeral_tunnel_public_base_url(value)
        });
    let active_tunnel_public_base_url =
        setup_backend_active_tunnel_public_base_url(state).or(runtime_public_base_url);
    let runtime_local_base_url = setup_backend_runtime_local_base_url(state, tenant);
    serde_json::json!({
        "public_base_url": if public_base_url.is_empty() { Value::Null } else { Value::String(public_base_url.clone()) },
        "public_base_url_is_ephemeral_tunnel": !public_base_url.is_empty() && is_ephemeral_tunnel_public_base_url(&public_base_url),
        "active_tunnel_public_base_url": active_tunnel_public_base_url,
        "runtime_local_base_url": runtime_local_base_url,
    })
}

async fn setup_backend_runtime_context_blocked(
    state: &UiState,
    runtime_context: &Value,
) -> Option<Value> {
    let public_base_url = runtime_context
        .get("public_base_url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let active_tunnel_public_base_url = runtime_context
        .get("active_tunnel_public_base_url")
        .and_then(Value::as_str);
    if runtime_context
        .get("public_base_url_is_ephemeral_tunnel")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && active_tunnel_public_base_url != Some(public_base_url)
    {
        return Some(serde_json::json!({
            "ok": false,
            "blocked": true,
            "error": "runtime/tunnel not running",
            "reason": "public_base_url came from an ephemeral setup tunnel, but that tunnel is not active for this setup session",
            "public_base_url": public_base_url,
            "active_tunnel_public_base_url": active_tunnel_public_base_url,
            "runtime_context": runtime_context,
        }));
    }
    if let Some(runtime_local_base_url) = runtime_context
        .get("runtime_local_base_url")
        .and_then(Value::as_str)
        && !setup_backend_runtime_base_responds(runtime_local_base_url).await
    {
        return Some(serde_json::json!({
            "ok": false,
            "blocked": true,
            "error": "runtime/tunnel not running",
            "reason": "runtime endpoint artifact exists, but the local runtime is not responding",
            "runtime_local_base_url": runtime_local_base_url,
            "setup_ui_url": state.local_base_url,
            "runtime_context": runtime_context,
        }));
    }
    None
}

async fn setup_backend_runtime_base_responds(base_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_millis(750))
        .build()
    else {
        return false;
    };
    client.get(base_url).send().await.is_ok()
}

async fn setup_backend_public_tunnel_responds(base_url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    else {
        return false;
    };
    client
        .get(base_url.trim_end_matches('/'))
        .send()
        .await
        .ok()
        .is_some_and(|response| response.status().as_u16() < 500)
}

fn setup_backend_runtime_context_current(value: &Value, current: &Value) -> bool {
    if value.is_null() {
        return false;
    }
    let current_public_base_url = current.get("public_base_url").and_then(Value::as_str);
    if current
        .get("public_base_url_is_ephemeral_tunnel")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && current
            .get("active_tunnel_public_base_url")
            .and_then(Value::as_str)
            != current_public_base_url
    {
        return false;
    }
    let Some(previous) = value.get("runtime_context") else {
        return !current
            .get("public_base_url_is_ephemeral_tunnel")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    };
    previous.get("public_base_url").and_then(Value::as_str) == current_public_base_url
}

fn is_ephemeral_tunnel_public_base_url(value: &str) -> bool {
    is_ephemeral_tunnel_url(value)
}

fn setup_backend_template_unresolved(value: &str) -> bool {
    value.contains('{') && value.contains('}')
}

fn render_setup_backend_contract_state(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: JsonMap<String, Value>,
) -> Value {
    let config = stored
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| default_setup_backend_config(state, tenant));
    let required_steps = setup_backend_required_steps(contract);
    let contract_blocked = setup_backend_contract_blocked(contract, &required_steps);
    let setup_result = stored
        .get("last_setup_result")
        .cloned()
        .unwrap_or(Value::Null);
    let raw_values = setup_backend_render_values(&config, &stored, setup_result.clone());
    let teams_app = setup_backend_render_teams_app(&stored);
    let items = if contract_blocked.is_some() {
        Vec::new()
    } else {
        setup_backend_contract_items(state, contract, tenant, &stored, &raw_values)
    };
    let values = setup_backend_filter_stale_action_values(
        contract,
        &items,
        raw_values,
        setup_result.clone(),
    );
    let reset = setup_backend_values_contain_stale_marker(&values);
    let ok = items
        .iter()
        .all(|item| item.get("state").and_then(Value::as_str) == Some("done"));
    let ok = ok && !items.is_empty() && contract_blocked.is_none();
    let next = if ok {
        "Setup complete.".to_string()
    } else if let Some(blocked) = contract_blocked.as_ref() {
        blocked
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("Setup backend contract is blocked.")
            .to_string()
    } else if setup_backend_pending_device_login(&setup_result) {
        setup_backend_device_login_next_message().to_string()
    } else if setup_result
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "Continue setup to run the next step.".to_string()
    } else if setup_result
        .get("next")
        .and_then(Value::as_str)
        .is_some_and(|next| !next.trim().is_empty())
    {
        setup_result
            .get("next")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        "Click Run next setup step.".to_string()
    };
    let route_ok = contract_blocked.is_none();
    let blocked = contract_blocked
        .or_else(|| stored.get("blocked").cloned())
        .or_else(|| setup_backend_blocked_from_result(&setup_result));
    let mut report = serde_json::json!({
        "ok": route_ok,
        "values": values,
        "teams_app": teams_app,
        "setup_status": {
            "ok": ok,
            "items": items,
            "selected": {
                "provider_id": contract.provider_id,
                "tenant": tenant,
                "team": state.team.clone().unwrap_or_else(|| "default".to_string()),
                "env": state.env,
            },
            "blocked": blocked,
            "last_step": if ok { Value::String("complete".to_string()) } else { setup_result.get("step").cloned().unwrap_or(Value::Null) },
            "next": next,
            "reset": reset,
        },
    });
    attach_final_setup_actions(state, &contract.provider_id, &mut report);
    report
}

fn attach_final_setup_actions(state: &UiState, provider_id: &str, report: &mut Value) {
    let Ok(Some(descriptor)) = find_setup_actions_descriptor(&state.bundle_path, provider_id)
    else {
        return;
    };
    let resolved =
        crate::setup_final_actions::resolve_final_setup_actions(provider_id, &descriptor, report);
    if let Some(obj) = report.as_object_mut() {
        obj.insert(
            "final_setup_actions".to_string(),
            serde_json::to_value(resolved.actions).unwrap_or_else(|_| Value::Array(Vec::new())),
        );
        if !resolved.diagnostics.is_empty() {
            obj.insert(
                "final_setup_action_diagnostics".to_string(),
                serde_json::to_value(resolved.diagnostics)
                    .unwrap_or_else(|_| Value::Array(Vec::new())),
            );
        }
    }
}

fn setup_backend_blocked_from_result(setup_result: &Value) -> Option<Value> {
    crate::setup_backend_contract::blocked_from_result(setup_result)
}

fn setup_backend_pending_device_login(setup_result: &Value) -> bool {
    setup_result
        .get("result")
        .and_then(|result| result.get("pending_device_login"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || setup_result
            .get("pending_device_login")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn setup_backend_device_login_next_message() -> &'static str {
    "Open the sign-in page, enter the code, then continue setup."
}

fn setup_backend_contract_items(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &JsonMap<String, Value>,
    values: &Value,
) -> Vec<Value> {
    let completed = stored
        .get("completed_steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let completed: std::collections::HashSet<&str> =
        completed.iter().filter_map(Value::as_str).collect();
    let mut previous_pending = false;
    setup_backend_required_steps(contract)
        .into_iter()
        .map(|step| {
            let action = setup_backend_action_by_id(contract, step);
            let cached_done = completed.contains(step)
                && action.is_none_or(|action| {
                    setup_backend_cached_completion_current(state, tenant, stored, values, action)
                });
            let independently_done = cached_done
                || action.is_some_and(|action| {
                    setup_backend_action_completion_current(state, tenant, stored, values, action)
                });
            let durable_done_after_pending = independently_done
                && action.is_some_and(setup_backend_action_is_runtime_context_durable);
            let done = independently_done && (!previous_pending || durable_done_after_pending);
            if !done {
                previous_pending = true;
            }
            serde_json::json!({
                "label": step.replace('_', " "),
                "state": if done { "done" } else { "pending" },
                "detail": Value::Null,
                "id": step,
            })
        })
        .collect()
}

fn setup_backend_render_values(
    config: &JsonMap<String, Value>,
    stored: &JsonMap<String, Value>,
    setup_result: Value,
) -> Value {
    let mut values = JsonMap::new();
    values.insert(
        "config".to_string(),
        Value::Object(setup_backend_public_config(config)),
    );
    values.insert("last_setup_result".to_string(), setup_result);
    values.insert(
        "backend".to_string(),
        stored.get("backend").cloned().unwrap_or(Value::Null),
    );
    for (key, value) in stored {
        if matches!(
            key.as_str(),
            "config" | "completed_steps" | "blocked" | "last_setup_result" | "teams_app"
        ) {
            continue;
        }
        values.insert(key.clone(), value.clone());
    }
    Value::Object(values)
}

fn setup_backend_filter_stale_action_values(
    contract: &ProviderBackendContract,
    items: &[Value],
    values: Value,
    setup_result: Value,
) -> Value {
    let mut values = values;
    let Some(values_obj) = values.as_object_mut() else {
        return values;
    };
    let item_states: std::collections::HashMap<&str, &str> = items
        .iter()
        .filter_map(|item| {
            Some((
                item.get("id")?.as_str()?,
                item.get("state")?.as_str().unwrap_or("pending"),
            ))
        })
        .collect();
    let setup_step = setup_result.get("step").and_then(Value::as_str);
    for action in contract
        .inline
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(step_id) = action.get("id").and_then(Value::as_str) else {
            continue;
        };
        if item_states.get(step_id) == Some(&"done") {
            continue;
        }
        let current_action_is_done =
            setup_step == Some(step_id) && item_states.get(step_id) == Some(&"done");
        if action
            .get("executor")
            .and_then(|executor| executor.get("kind"))
            .and_then(Value::as_str)
            == Some("oauth_device_code")
        {
            setup_backend_mark_oauth_action_value_stale(values_obj, action, step_id);
        }
        if setup_backend_action_is_runtime_context_durable(action) {
            continue;
        }
        let Some(state_key) = action
            .get("executor")
            .and_then(|executor| executor.get("state_store_key"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if current_action_is_done {
            continue;
        }
        if let Some(value) = values_obj.get_mut(state_key) {
            setup_backend_mark_action_value_stale(value, step_id);
        }
    }
    values
}

fn setup_backend_mark_action_value_stale(value: &mut Value, step_id: &str) {
    if let Some(object) = value.as_object_mut() {
        object.insert("ok".to_string(), Value::Bool(false));
        object.insert("stale".to_string(), Value::Bool(true));
        object.insert("stale_step".to_string(), Value::String(step_id.to_string()));
    }
}

fn setup_backend_mark_oauth_action_value_stale(
    values: &mut JsonMap<String, Value>,
    action: &Value,
    step_id: &str,
) {
    let oauth_kind = action
        .get("executor")
        .and_then(|executor| executor.get("oauth_kind"))
        .and_then(Value::as_str)
        .unwrap_or("default");
    let Some(oauth) = values.get_mut("oauth").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(value) = oauth.get_mut(oauth_kind) {
        setup_backend_mark_action_value_stale(value, step_id);
    }
}

fn setup_backend_values_contain_stale_marker(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("stale")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || object
                    .values()
                    .any(setup_backend_values_contain_stale_marker)
        }
        Value::Array(items) => items.iter().any(setup_backend_values_contain_stale_marker),
        _ => false,
    }
}

fn setup_backend_public_config(config: &JsonMap<String, Value>) -> JsonMap<String, Value> {
    let mut public = config.clone();
    for key in [
        "oauth_device_code",
        "graph_access_token",
        "azure_management_access_token",
        "bot_access_token",
        "access_token",
        "refresh_token",
        "id_token",
    ] {
        public.remove(key);
    }
    public
}

fn setup_backend_render_teams_app(stored: &JsonMap<String, Value>) -> Value {
    if let Some(value) = stored.get("teams_app") {
        return value.clone();
    }
    let publish = stored
        .get("last_teams_app_publish")
        .and_then(Value::as_object);
    let install = stored
        .get("last_teams_app_install")
        .and_then(Value::as_object);
    let add_to_teams_url = install
        .and_then(|value| value.get("add_to_teams_url"))
        .or_else(|| {
            install.and_then(|value| {
                value
                    .get("response")
                    .and_then(|response| response.get("add_to_teams_url"))
            })
        })
        .or_else(|| publish.and_then(|value| value.get("add_to_teams_url")))
        .or_else(|| {
            publish.and_then(|value| {
                value
                    .get("response")
                    .and_then(|response| response.get("add_to_teams_url"))
            })
        })
        .cloned()
        .unwrap_or(Value::Null);
    let open_bot_chat_url = install
        .and_then(|value| value.get("open_bot_chat_url"))
        .or_else(|| {
            install.and_then(|value| {
                value
                    .get("response")
                    .and_then(|response| response.get("open_bot_chat_url"))
            })
        })
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({
        "ok": !add_to_teams_url.is_null() || !open_bot_chat_url.is_null(),
        "add_to_teams_url": add_to_teams_url,
        "open_bot_chat_url": open_bot_chat_url,
    })
}

fn setup_backend_completion_met(values: &Value, completion: &Value) -> bool {
    crate::setup_backend_contract::completion_met(values, completion)
}

fn setup_backend_action_completion_current(
    state: &UiState,
    tenant: &str,
    stored: &JsonMap<String, Value>,
    values: &Value,
    action: &Value,
) -> bool {
    let Some(completion) = action.get("completion") else {
        return false;
    };
    if !setup_backend_completion_met(values, completion) {
        return false;
    }
    let Some(state_key) = action
        .get("executor")
        .and_then(|executor| executor.get("state_store_key"))
        .and_then(Value::as_str)
    else {
        return true;
    };
    let Some(stored_value) = stored.get(state_key) else {
        return true;
    };
    if setup_backend_last_result_matches_action(stored, action)
        && setup_backend_action_result_runtime_context_current(state, tenant, stored, action)
    {
        return true;
    }
    if setup_backend_action_is_runtime_context_durable(action) {
        return true;
    }
    let config = stored
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let current = setup_backend_runtime_context(state, tenant, &config);
    setup_backend_runtime_context_current(stored_value, &current)
}

fn setup_backend_cached_completion_current(
    state: &UiState,
    tenant: &str,
    stored: &JsonMap<String, Value>,
    values: &Value,
    action: &Value,
) -> bool {
    let Some(state_key) = action
        .get("executor")
        .and_then(|executor| executor.get("state_store_key"))
        .and_then(Value::as_str)
    else {
        return true;
    };
    let Some(stored_value) = stored.get(state_key) else {
        return true;
    };
    if setup_backend_action_is_runtime_context_durable(action) {
        return action
            .get("completion")
            .is_none_or(|completion| setup_backend_completion_met(values, completion));
    }
    let config = stored
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let current = setup_backend_runtime_context(state, tenant, &config);
    if !setup_backend_runtime_context_current(stored_value, &current) {
        return false;
    }
    action
        .get("completion")
        .is_none_or(|completion| setup_backend_completion_met(values, completion))
}

fn setup_backend_action_is_runtime_context_durable(action: &Value) -> bool {
    let action_id = action.get("id").and_then(Value::as_str);
    let state_key = action
        .get("executor")
        .and_then(|executor| executor.get("state_store_key"))
        .and_then(Value::as_str);
    matches!(
        action_id,
        Some("teams_app_publish" | "teams_app_user_install")
    ) || matches!(
        state_key,
        Some("last_teams_app_publish" | "last_teams_app_install")
    )
}

fn setup_backend_last_result_matches_action(
    stored: &JsonMap<String, Value>,
    action: &Value,
) -> bool {
    let Some(action_id) = action.get("id").and_then(Value::as_str) else {
        return false;
    };
    let Some(result) = stored.get("last_setup_result").and_then(Value::as_object) else {
        return false;
    };
    result.get("step").and_then(Value::as_str) == Some(action_id)
        && result.get("ok").and_then(Value::as_bool).unwrap_or(false)
}

fn setup_backend_action_result_runtime_context_current(
    state: &UiState,
    tenant: &str,
    stored: &JsonMap<String, Value>,
    action: &Value,
) -> bool {
    if setup_backend_action_is_runtime_context_durable(action) {
        return true;
    }
    let Some(result) = stored.get("last_setup_result").and_then(Value::as_object) else {
        return true;
    };
    let Some(result_body) = result.get("result") else {
        return true;
    };
    let Some(result_context) = result_body.get("runtime_context") else {
        return true;
    };
    let config = stored
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let current = setup_backend_runtime_context(state, tenant, &config);
    setup_backend_runtime_context_current(
        &serde_json::json!({ "runtime_context": result_context }),
        &current,
    )
}

fn setup_backend_required_steps(contract: &ProviderBackendContract) -> Vec<&str> {
    crate::setup_backend_contract::required_steps(&contract.inline)
}

fn setup_backend_contract_blocked(
    contract: &ProviderBackendContract,
    required_steps: &[&str],
) -> Option<Value> {
    if let Some(err) = contract.load_error.as_ref() {
        return Some(serde_json::json!({
            "title": "Setup backend contract could not be loaded",
            "summary": "Setup backend contract asset could not be loaded.",
            "detail": err,
        }));
    }
    if required_steps.is_empty() {
        return Some(serde_json::json!({
            "title": "Setup backend contract is incomplete",
            "summary": "Setup backend contract has no required_order steps.",
            "detail": "The pack must provide an effective greentic.setup.backend-contract.v1 contract with required_order.",
        }));
    }
    None
}

fn setup_backend_first_pending_step(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &JsonMap<String, Value>,
) -> String {
    if contract.load_error.is_some() || setup_backend_required_steps(contract).is_empty() {
        return "contract_blocked".to_string();
    }
    if let Some(token_store_key) = crate::setup_backend_contract::oauth_resume_token(stored)
        && let Some(action) = crate::setup_backend_contract::oauth_action_by_token_store_key(
            &contract.inline,
            token_store_key,
        )
        && let Some(action_id) = action.get("id").and_then(Value::as_str)
    {
        return action_id.to_string();
    }
    let config = stored
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let setup_result = stored
        .get("last_setup_result")
        .cloned()
        .unwrap_or(Value::Null);
    let values = setup_backend_render_values(&config, stored, setup_result);
    setup_backend_contract_items(state, contract, tenant, stored, &values)
        .into_iter()
        .find(|item| item.get("state").and_then(Value::as_str) != Some("done"))
        .and_then(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "complete".to_string())
}

fn is_safe_runtime_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

fn configured_runtime_proxy_base_url() -> Option<String> {
    std::env::var("GREENTIC_SETUP_RUNTIME_URL")
        .ok()
        .or_else(|| std::env::var("GREENTIC_RUNTIME_URL").ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| {
            Url::parse(value).ok().is_some_and(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some_and(|host| {
                        host.eq_ignore_ascii_case("localhost")
                            || host == "127.0.0.1"
                            || host == "::1"
                    })
            })
        })
}

async fn forward_runtime_request(
    method: axum::http::Method,
    target: &str,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response> {
    let client = reqwest::Client::new();
    let reqwest_method = reqwest::Method::from_bytes(method.as_str().as_bytes())?;
    let mut builder = client.request(reqwest_method, target);
    for (name, value) in headers.iter() {
        if matches!(
            name.as_str(),
            "host" | "connection" | "upgrade" | "transfer-encoding" | "content-length"
        ) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let upstream = builder.body(body.to_vec()).send().await?;
    let status = StatusCode::from_u16(upstream.status().as_u16())?;
    let upstream_headers = upstream.headers().clone();
    let bytes = upstream.bytes().await?;
    let mut response = Body::from(bytes.to_vec()).into_response();
    *response.status_mut() = status;
    for (name, value) in upstream_headers.iter() {
        if matches!(
            name.as_str(),
            "connection" | "upgrade" | "transfer-encoding" | "content-length"
        ) {
            continue;
        }
        if let Ok(header_value) = HeaderValue::from_bytes(value.as_bytes()) {
            response.headers_mut().insert(name.clone(), header_value);
        }
    }
    Ok(response)
}

fn status_text(status: StatusCode, text: &str) -> Response {
    let mut response = text.to_string().into_response();
    *response.status_mut() = status;
    response
}

fn content_type_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
    {
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn persist_provider_setup_event(state: &UiState, req: ProviderSetupEventRequest) -> Result<Value> {
    let provider_id = validate_log_path_segment(&req.provider_id, "provider_id")?;
    let tenant = req.tenant.unwrap_or_else(|| state.tenant.clone());
    let team = req.team.or_else(|| state.team.clone());
    let env = req.env.unwrap_or_else(|| state.env.clone());
    let tenant_segment = validate_log_path_segment(&tenant, "tenant")?;
    let team_segment = validate_log_path_segment(team.as_deref().unwrap_or("default"), "team")?;
    let env_segment = validate_log_path_segment(&env, "env")?;
    let event_name = validate_event_name(&req.event_name)?;
    let setup_session_id = req
        .setup_session_id
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| state.setup_session_id.clone());
    let setup_ui_url = req
        .setup_ui_url
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| state.local_base_url.clone());
    let event_detail = redact_provider_setup_event_detail(&req.event_detail);
    let current_step_id = req.current_step_id.unwrap_or_else(|| {
        provider_setup_event_detail_field(
            &event_detail,
            &[
                "currentStepId",
                "current_step_id",
                "stepId",
                "step_id",
                "step",
            ],
        )
    });
    let current_progress = req.current_progress.unwrap_or_else(|| {
        provider_setup_event_detail_field(
            &event_detail,
            &["currentProgress", "current_progress", "progress"],
        )
    });
    let action_name = req.action_name.unwrap_or_else(|| {
        provider_setup_event_detail_field(
            &event_detail,
            &["actionName", "action_name", "action", "name"],
        )
    });
    let request_method = req.request_method.unwrap_or_else(|| {
        provider_setup_event_detail_field(
            &event_detail,
            &["method", "requestMethod", "request_method"],
        )
    });
    let request_path = req.request_path.unwrap_or_else(|| {
        provider_setup_event_detail_field(
            &event_detail,
            &["path", "requestPath", "request_path", "url"],
        )
    });
    let http_status = req.http_status.unwrap_or_else(|| {
        provider_setup_event_detail_field(&event_detail, &["status", "httpStatus", "http_status"])
    });
    let response_body = req.response_body.unwrap_or_else(|| {
        provider_setup_event_detail_field(
            &event_detail,
            &["responseBody", "response_body", "body", "response"],
        )
    });
    let error = req.error.unwrap_or_else(|| {
        provider_setup_event_detail_field(&event_detail, &["error", "message", "detail"])
    });
    let correlation_id = req.correlation_id.unwrap_or_else(|| {
        provider_setup_event_detail_field(
            &event_detail,
            &[
                "correlationId",
                "correlation_id",
                "trace_id",
                "traceId",
                "request-id",
                "client-request-id",
            ],
        )
    });
    let record = serde_json::json!({
        "timestamp": unix_timestamp_millis(),
        "tenant": tenant,
        "team": team,
        "env": env,
        "provider_id": provider_id,
        "event_name": event_name,
        "current_step_id": current_step_id,
        "current_progress": current_progress,
        "action_name": action_name,
        "request_method": request_method,
        "request_path": request_path,
        "http_status": http_status,
        "response_body": response_body,
        "error": error,
        "correlation_id": correlation_id,
        "event_detail": event_detail,
        "setup_session_id": setup_session_id,
        "setup_ui_url": setup_ui_url,
    });
    let path = provider_setup_event_log_path(
        state,
        env_segment,
        tenant_segment,
        team_segment,
        provider_id,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create setup log dir {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open setup log {}", path.display()))?;
    serde_json::to_writer(&mut file, &record).context("serialize provider setup event")?;
    file.write_all(b"\n")
        .with_context(|| format!("append setup log {}", path.display()))?;
    Ok(record)
}

fn read_provider_setup_events(
    state: &UiState,
    query: &ProviderSetupEventsQuery,
) -> Result<Vec<Value>> {
    let provider_id = validate_log_path_segment(&query.provider_id, "provider_id")?;
    let tenant = query.tenant.clone().unwrap_or_else(|| state.tenant.clone());
    let team = query.team.clone().or_else(|| state.team.clone());
    let env = query.env.clone().unwrap_or_else(|| state.env.clone());
    let tenant_segment = validate_log_path_segment(&tenant, "tenant")?;
    let team_segment = validate_log_path_segment(team.as_deref().unwrap_or("default"), "team")?;
    let env_segment = validate_log_path_segment(&env, "env")?;
    let path = provider_setup_event_log_path(
        state,
        env_segment,
        tenant_segment,
        team_segment,
        provider_id,
    );
    let limit = query.limit.unwrap_or(200).clamp(1, 1000);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("open setup log {}", path.display())),
    };
    let mut events = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            events.push(value);
        }
    }
    if events.len() > limit {
        Ok(events.split_off(events.len() - limit))
    } else {
        Ok(events)
    }
}

fn provider_setup_event_log_path(
    state: &UiState,
    env: &str,
    tenant: &str,
    team: &str,
    provider_id: &str,
) -> PathBuf {
    state
        .bundle_path
        .join("state")
        .join("logs")
        .join("setup")
        .join(env)
        .join(tenant)
        .join(team)
        .join(format!("{provider_id}.jsonl"))
}

fn validate_log_path_segment<'a>(value: &'a str, name: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.contains('\\')
    {
        anyhow::bail!("invalid {name}");
    }
    Ok(value)
}

fn validate_event_name(value: &str) -> Result<&str> {
    let value = value.trim();
    if !value.starts_with("greentic-provider-setup-")
        || value.contains('/')
        || value.contains('\\')
        || value.len() > 160
    {
        anyhow::bail!("invalid provider setup event name");
    }
    Ok(value)
}

fn provider_setup_event_detail_field(value: &Value, names: &[&str]) -> Value {
    for name in names {
        if let Some(found) = provider_setup_event_detail_field_one(value, name) {
            return found;
        }
    }
    Value::Null
}

fn provider_setup_event_detail_field_one(value: &Value, name: &str) -> Option<Value> {
    let object = value.as_object()?;
    if let Some(found) = object.get(name) {
        return Some(found.clone());
    }
    let normalized = normalize_provider_setup_event_key(name);
    for (key, nested) in object {
        if normalize_provider_setup_event_key(key) == normalized {
            return Some(nested.clone());
        }
    }
    for nested in object.values() {
        if nested.is_object()
            && let Some(found) = provider_setup_event_detail_field_one(nested, name)
        {
            return Some(found);
        }
    }
    None
}

fn normalize_provider_setup_event_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| *ch != '_' && *ch != '-')
        .flat_map(char::to_lowercase)
        .collect()
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn redact_provider_setup_event_detail(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = JsonMap::new();
            for (key, value) in map {
                let normalized = key
                    .chars()
                    .filter(|ch| *ch != '_' && *ch != '-')
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                if is_secret_event_key(&normalized) {
                    redacted.insert(key.clone(), Value::String("[redacted]".to_string()));
                } else if normalized == "usercode" {
                    redacted.insert(
                        key.clone(),
                        Value::String(short_sha256_marker(value.as_str().unwrap_or_default())),
                    );
                } else {
                    redacted.insert(key.clone(), redact_provider_setup_event_detail(value));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(redact_provider_setup_event_detail)
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn is_secret_event_key(normalized_key: &str) -> bool {
    matches!(
        normalized_key,
        "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "clientsecret"
            | "botapppassword"
            | "devicecode"
            | "oauthdevicecode"
    )
}

fn short_sha256_marker(value: &str) -> String {
    use sha2::{Digest, Sha256};
    if value.is_empty() {
        return "[redacted]".to_string();
    }
    let digest = Sha256::digest(value.as_bytes());
    format!("[sha256:{}]", base16_lower_prefix(&digest, 12))
}

fn base16_lower_prefix(bytes: &[u8], chars: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(chars);
    for byte in bytes {
        if out.len() >= chars {
            break;
        }
        out.push(HEX[(byte >> 4) as usize] as char);
        if out.len() >= chars {
            break;
        }
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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
    let mut answers = req.answers;
    let provider_setup_status = req.provider_setup_status;
    let tunnel_mode = req.tunnel.as_deref().unwrap_or("off").to_string();

    // Persist tunnel config from the UI selection.
    if let Some(mode) = req.tunnel.as_deref() {
        let tunnel = crate::platform_setup::TunnelAnswers {
            mode: Some(mode.to_string()),
        };
        let _ = crate::platform_setup::persist_tunnel_artifact(&state.bundle_path, &tunnel);
    }

    let setup_public_base_url = if should_start_setup_tunnel(&tunnel_mode, &answers) {
        match ensure_setup_tunnel(state.as_ref(), &tunnel_mode, &state.local_base_url).await {
            Ok(url) => {
                inject_setup_public_base_url(&mut answers, &url);
                Some(url)
            }
            Err(err) => {
                return Json(ExecutionResult {
                    success: false,
                    stdout: String::new(),
                    stderr: format!("Failed to start setup tunnel: {err}"),
                    manual_steps: vec![],
                    provider_setup_status,
                });
            }
        }
    } else {
        None
    };

    let bundle_path_for_repack = bundle_path.clone();
    let mut result = tokio::task::spawn_blocking(move || {
        execute_setup(&bundle_path, &tenant, team.as_deref(), &env, answers)
    })
    .await
    .unwrap_or_else(|e| ExecutionResult {
        success: false,
        stdout: String::new(),
        stderr: format!("Task panicked: {e}"),
        manual_steps: vec![],
        provider_setup_status: JsonMap::new(),
    });
    result.provider_setup_status = provider_setup_status.clone();
    if let Some(public_base_url) = setup_public_base_url.as_deref()
        && result.success
    {
        result.stdout = append_line(
            &result.stdout,
            &format!("Setup tunnel public_base_url: {public_base_url}"),
        );
    }

    // After a successful UI setup, re-pack the extracted bundle dir back
    // to its original `.gtbundle` archive (or copy it to a directory
    // output) so the on-disk artifact reflects the answers the user just
    // saved. Without this the simple-mode CLI did the write-back but the
    // UI mode silently dropped it — see bin/greentic_setup.rs:run_ui_mode.
    if result.success
        && let Some(target) = state.output_target.clone()
    {
        let repack = tokio::task::spawn_blocking(move || -> Result<String, anyhow::Error> {
            use crate::cli_helpers::{SetupOutputTarget, copy_dir_recursive};
            use crate::gtbundle;
            match target {
                SetupOutputTarget::Archive(out) => {
                    gtbundle::create_gtbundle(&bundle_path_for_repack, &out).with_context(
                        || {
                            format!(
                                "failed to write configured .gtbundle archive to {}",
                                out.display()
                            )
                        },
                    )?;
                    Ok(format!("Configured bundle written to: {}", out.display()))
                }
                SetupOutputTarget::Directory(out) => {
                    if out.exists() {
                        if out.is_dir() {
                            std::fs::remove_dir_all(&out).with_context(|| {
                                format!(
                                    "failed to replace existing bundle directory {}",
                                    out.display()
                                )
                            })?;
                        } else {
                            std::fs::remove_file(&out).with_context(|| {
                                format!("failed to replace existing bundle file {}", out.display())
                            })?;
                        }
                    }
                    copy_dir_recursive(&bundle_path_for_repack, &out, false)
                        .context("failed to write configured local bundle directory")?;
                    Ok(format!("Configured bundle written to: {}", out.display()))
                }
            }
        })
        .await;
        match repack {
            Ok(Ok(msg)) => result.stdout.push_str(&format!("\n{msg}\n")),
            Ok(Err(e)) => {
                result.success = false;
                result
                    .stderr
                    .push_str(&format!("\nWrite-back failed: {e:#}\n"));
            }
            Err(e) => {
                result.success = false;
                result
                    .stderr
                    .push_str(&format!("\nWrite-back panicked: {e}\n"));
            }
        }
    }

    *state.result.lock().unwrap() = Some(result.clone());
    Json(result)
}

async fn get_result(State(state): State<std::sync::Arc<UiState>>) -> Json<Value> {
    let result = state.result.lock().unwrap().clone();
    match result {
        Some(result) => Json(serde_json::json!({
            "finished": true,
            "result": result,
            "success": result.success,
        })),
        None => Json(serde_json::json!({
            "finished": false,
        })),
    }
}

async fn post_provider_setup_event(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<ProviderSetupEventRequest>,
) -> Response {
    match persist_provider_setup_event(&state, req) {
        Ok(record) => Json(serde_json::json!({
            "ok": true,
            "record": record,
        }))
        .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "error": err.to_string(),
            })),
        )
            .into_response(),
    }
}

async fn get_provider_setup_events(
    State(state): State<std::sync::Arc<UiState>>,
    Query(query): Query<ProviderSetupEventsQuery>,
) -> Response {
    match read_provider_setup_events(&state, &query) {
        Ok(events) => Json(serde_json::json!({
            "ok": true,
            "events": events,
        }))
        .into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "error": err.to_string(),
            })),
        )
            .into_response(),
    }
}

async fn post_draft(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<DraftSaveRequest>,
) -> Json<Value> {
    if let Some(mode) = req.tunnel.as_deref() {
        let tunnel = crate::platform_setup::TunnelAnswers {
            mode: Some(mode.to_string()),
        };
        if let Err(err) =
            crate::platform_setup::persist_tunnel_artifact(&state.bundle_path, &tunnel)
        {
            return Json(serde_json::json!({
                "ok": false,
                "error": err.to_string(),
            }));
        }
    }
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

async fn post_setup_action(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<SetupActionRequest>,
) -> Json<Value> {
    match execute_setup_action(state.as_ref(), req).await {
        Ok(value) => Json(value),
        Err(err) => Json(serde_json::json!({
            "ok": false,
            "error": err.to_string(),
        })),
    }
}

async fn post_setup_public_url(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<SetupPublicUrlRequest>,
) -> Json<Value> {
    match ensure_setup_public_url(state.as_ref(), req).await {
        Ok(public_base_url) => Json(serde_json::json!({
            "ok": true,
            "public_base_url": public_base_url,
        })),
        Err(err) => Json(serde_json::json!({
            "ok": false,
            "error": err.to_string(),
        })),
    }
}

async fn ensure_setup_public_url(state: &UiState, req: SetupPublicUrlRequest) -> Result<String> {
    let mode = req
        .tunnel
        .as_deref()
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(ToString::to_string)
        .or_else(|| setup_backend_tunnel_mode(state).ok().flatten())
        .unwrap_or_else(|| "off".to_string());
    if !matches!(mode.as_str(), "cloudflared" | "ngrok") {
        anyhow::bail!("setup tunnel is disabled");
    }
    let tunnel = crate::platform_setup::TunnelAnswers {
        mode: Some(mode.clone()),
    };
    crate::platform_setup::persist_tunnel_artifact(&state.bundle_path, &tunnel)?;
    let _tenant = req.tenant.unwrap_or_else(|| state.tenant.clone());
    let _team = req.team.or_else(|| state.team.clone());
    let _env = req.env.unwrap_or_else(|| state.env.clone());
    ensure_setup_tunnel(state, &mode, &state.local_base_url).await
}

async fn execute_setup_action(state: &UiState, req: SetupActionRequest) -> Result<Value> {
    let tenant = req.tenant.unwrap_or_else(|| state.tenant.clone());
    let team = req.team.or_else(|| state.team.clone());
    let env = req.env.unwrap_or_else(|| state.env.clone());
    let mut answers = req.answers;
    let tunnel_mode = setup_action_tunnel_mode(state, req.tunnel.as_deref())?;
    ensure_setup_action_provider_answers(&mut answers, &req.provider_id);
    if let Some(mode) = req.tunnel.as_deref() {
        let tunnel = crate::platform_setup::TunnelAnswers {
            mode: Some(mode.to_string()),
        };
        crate::platform_setup::persist_tunnel_artifact(&state.bundle_path, &tunnel)?;
    }
    if should_start_setup_tunnel(&tunnel_mode, &answers) {
        let url = ensure_setup_tunnel(state, &tunnel_mode, &state.local_base_url).await?;
        inject_setup_public_base_url(&mut answers, &url);
    }
    persist_ui_draft(&state.bundle_path, &tenant, team.as_deref(), &env, &answers).await?;

    let discovered = discovery::discover(&state.bundle_path)?;
    let provider = discovered
        .find_setup_target(&req.provider_id)
        .ok_or_else(|| anyhow!("provider not found: {}", req.provider_id))?;
    let descriptor = load_setup_actions_descriptor(provider)
        .ok_or_else(|| anyhow!("provider has no setup actions: {}", req.provider_id))?;
    let action = descriptor
        .get("actions")
        .and_then(Value::as_array)
        .and_then(|actions| {
            actions
                .iter()
                .find(|action| action.get("id").and_then(Value::as_str) == Some(&req.action_id))
        })
        .ok_or_else(|| anyhow!("setup action not found: {}", req.action_id))?;
    if action.get("kind").and_then(Value::as_str) != Some("oauth_install_button") {
        anyhow::bail!("setup action kind is not executable in this phase");
    }
    let registration = action
        .get("registration")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("setup action missing registration metadata"))?;
    let mut config = answers
        .get(&req.provider_id)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    config.insert("tenant".to_string(), Value::String(tenant.clone()));
    config.insert(
        "team".to_string(),
        Value::String(team.clone().unwrap_or_else(|| "default".to_string())),
    );
    config.insert("env".to_string(), Value::String(env.clone()));
    setup_backend_apply_host_defaults(state, &tenant, &mut config);

    let request = Value::Object(config.clone());
    let setup_config = SetupConfig {
        tenant: tenant.clone(),
        team: team.clone(),
        env: env.clone(),
        offline: false,
        verbose: state.advanced,
    };
    let output = if let Some(result) = registration
        .get("result")
        .or_else(|| registration.get("mock_result"))
        .or_else(|| registration.get("outputs"))
    {
        result.clone()
    } else {
        let component_ref = registration
            .get("component_ref")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("setup action registration missing component_ref"))?;
        let op = registration
            .get("op")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("setup action registration missing op"))?;
        invoke_setup_component_operation_blocking(
            state.bundle_path.clone(),
            provider.pack_path.clone(),
            component_ref.to_string(),
            op.to_string(),
            request,
            setup_config,
        )
        .await?
    };
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        return Ok(serde_json::json!({
            "ok": false,
            "provider_id": req.provider_id,
            "action_id": req.action_id,
            "error": output.get("error").and_then(Value::as_str).unwrap_or("setup action failed"),
            "output": redact_setup_action_output(&output),
        }));
    }

    merge_scalar_values(&mut config, &output);
    let final_url_context = SetupActionFinalUrlContext {
        bundle_root: &state.bundle_path,
        tenant: &tenant,
        team: team.as_deref(),
        provider_id: &req.provider_id,
        action_id: &req.action_id,
    };
    if let Some((key, url)) =
        setup_action_final_url_value(&descriptor, action, &config, &final_url_context)?
    {
        config.insert(key, Value::String(url));
    }
    crate::qa::persist::persist_all_config_as_secrets(
        &state.bundle_path,
        &env,
        &tenant,
        team.as_deref(),
        &req.provider_id,
        &Value::Object(config.clone()),
        Some(&provider.pack_path),
    )
    .await?;
    let safe_values = public_setup_action_values(&config);
    Ok(serde_json::json!({
        "ok": true,
        "provider_id": req.provider_id,
        "action_id": req.action_id,
        "values": safe_values,
        "state": {
            "values": safe_values,
            "setup_status": { "ok": true }
        },
        "setup_status": { "ok": true },
        "output": redact_setup_action_output(&output),
    }))
}

fn setup_action_tunnel_mode(state: &UiState, requested: Option<&str>) -> Result<String> {
    if let Some(mode) = requested
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .map(ToString::to_string)
    {
        return Ok(mode);
    }
    Ok(setup_backend_tunnel_mode(state)?.unwrap_or_else(|| "off".to_string()))
}

fn ensure_setup_action_provider_answers(answers: &mut JsonMap<String, Value>, provider_id: &str) {
    answers
        .entry(provider_id.to_string())
        .or_insert_with(|| Value::Object(JsonMap::new()));
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

async fn get_oauth_callback(
    State(state): State<std::sync::Arc<UiState>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let code = query.get("code").cloned().unwrap_or_default();
    let oauth_state = query.get("state").cloned().unwrap_or_default();
    if code.is_empty() || oauth_state.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            oauth_callback_page(
                false,
                "OAuth setup failed",
                "OAuth callback missing code or state.",
            ),
        );
    }
    match crate::oauth_callback::complete_oauth_callback(
        &state.bundle_path,
        &state.env,
        &crate::oauth_callback::OAuthCallbackInput {
            code,
            state: oauth_state,
        },
        "messaging.oauth.v1",
    )
    .await
    {
        Ok(report) => {
            let message = format!(
                "OAuth setup complete for {} ({}/{})",
                report.provider_id, report.tenant, report.team
            );
            (
                axum::http::StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                oauth_callback_page(
                    true,
                    "OAuth setup complete",
                    &format!("{message}. You can close this tab and return to setup."),
                ),
            )
        }
        Err(err) => (
            axum::http::StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            oauth_callback_page(false, "OAuth setup failed", &err.to_string()),
        ),
    }
}

fn oauth_callback_page(success: bool, title: &str, message: &str) -> String {
    let status_class = if success { "success" } else { "error" };
    let close_script = if success {
        r#"<script>
setTimeout(function () {
  window.close();
}, 800);
</script>"#
    } else {
        ""
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <style>
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; background: #f6f8fb; color: #17202a; }}
    main {{ width: min(520px, calc(100vw - 32px)); padding: 28px; border: 1px solid #d7dee8; border-radius: 8px; background: #fff; box-shadow: 0 16px 40px rgba(15, 23, 42, .08); }}
    h1 {{ margin: 0 0 12px; font-size: 1.35rem; line-height: 1.25; }}
    p {{ margin: 0; line-height: 1.55; color: #465466; }}
    .success h1 {{ color: #087f5b; }}
    .error h1 {{ color: #b42318; }}
  </style>
</head>
<body>
  <main class="{status_class}">
    <h1>{title}</h1>
    <p>{message}</p>
  </main>
  {close_script}
</body>
</html>"#,
        title = html_escape(title),
        message = html_escape(message),
        status_class = status_class,
        close_script = close_script
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

async fn post_shutdown(State(state): State<std::sync::Arc<UiState>>) {
    let _ = state.shutdown_tx.send(());
}

// ── Execution ──

fn append_line(existing: &str, line: &str) -> String {
    if existing.trim().is_empty() {
        line.to_string()
    } else {
        format!("{existing}\n{line}")
    }
}

async fn ensure_setup_tunnel(state: &UiState, mode: &str, local_base_url: &str) -> Result<String> {
    if let Some(existing) = active_setup_tunnel_public_base_url(state, mode, local_base_url)? {
        if setup_backend_public_tunnel_responds(&existing).await {
            return Ok(existing);
        }
        anyhow::bail!("{mode} setup tunnel URL did not become reachable: {existing}");
    }

    let _start_guard = state.setup_tunnel_start.lock().await;
    if let Some(existing) = active_setup_tunnel_public_base_url(state, mode, local_base_url)? {
        if setup_backend_public_tunnel_responds(&existing).await {
            return Ok(existing);
        }
        anyhow::bail!("{mode} setup tunnel URL did not become reachable: {existing}");
    }

    let local_base_url = local_base_url.trim_end_matches('/').to_string();
    let mode_for_task = mode.to_string();
    let local_base_url_for_task = local_base_url.clone();
    let tunnel = tokio::task::spawn_blocking(move || {
        start_setup_tunnel(&mode_for_task, &local_base_url_for_task)
    })
    .await
    .map_err(|err| anyhow!("setup tunnel task failed: {err}"))??;
    let public_base_url = tunnel.public_base_url.clone();
    if wait_for_setup_public_tunnel(&public_base_url).await {
        let mut guard = state
            .setup_tunnel
            .lock()
            .map_err(|_| anyhow!("setup tunnel lock poisoned"))?;
        *guard = Some(tunnel);
        return Ok(public_base_url);
    }
    drop(tunnel);
    anyhow::bail!("{mode} setup tunnel URL did not become reachable: {public_base_url}")
}

fn active_setup_tunnel_public_base_url(
    state: &UiState,
    mode: &str,
    local_base_url: &str,
) -> Result<Option<String>> {
    let mut guard = state
        .setup_tunnel
        .lock()
        .map_err(|_| anyhow!("setup tunnel lock poisoned"))?;
    Ok(
        if let Some(tunnel) = guard.as_mut()
            && tunnel.mode == mode
            && tunnel.local_base_url == local_base_url.trim_end_matches('/')
            && tunnel.is_running()
        {
            Some(tunnel.public_base_url.clone())
        } else {
            None
        },
    )
}

fn setup_backend_clear_setup_tunnel(state: &UiState) {
    if let Ok(mut guard) = state.setup_tunnel.lock() {
        *guard = None;
    }
}

async fn wait_for_setup_public_tunnel(public_base_url: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    while tokio::time::Instant::now() < deadline {
        if setup_backend_public_tunnel_responds(public_base_url).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

async fn ensure_setup_runtime(state: &UiState, tenant: &str) -> Result<()> {
    let _start_guard = state.setup_runtime_start.lock().await;
    if let Some(runtime_base_url) = setup_backend_setup_runtime_info(state)
        .and_then(|info| info.local_base_url)
        .filter(|value| !value.trim().is_empty())
        && setup_backend_runtime_base_responds(runtime_base_url.trim_end_matches('/')).await
    {
        return Ok(());
    }
    if let Some(runtime_base_url) = crate::platform_setup::load_runtime_local_base_url(
        &state.bundle_path,
        tenant,
        state.team.as_deref(),
    )
    .ok()
    .flatten()
    .map(|value| value.trim().trim_end_matches('/').to_string())
    .filter(|value| !value.is_empty())
        && setup_backend_runtime_base_responds(&runtime_base_url).await
    {
        return Ok(());
    }

    {
        let mut guard = state
            .setup_runtime
            .lock()
            .map_err(|_| anyhow!("setup runtime lock poisoned"))?;
        if let Some(runtime) = guard.as_mut() {
            match runtime.child.try_wait() {
                Ok(Some(status)) => {
                    *guard = None;
                    eprintln!("Setup-started runtime exited before observation: {status}");
                }
                Ok(None) => {}
                Err(err) => {
                    *guard = None;
                    eprintln!("Could not inspect setup-started runtime: {err}");
                }
            }
        }
        if guard.is_none() {
            let system_log_line_floor =
                setup_runtime_system_log_line_count(&state.bundle_path).ok();
            let mut child = Command::new("greentic-start")
                .arg("start")
                .arg("--bundle")
                .arg(&state.bundle_path)
                .arg("--cloudflared")
                .arg("off")
                .arg("--no-browser")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .with_context(|| format!("start runtime for {}", state.bundle_path.display()))?;
            let info = Arc::new(Mutex::new(SetupRuntimeInfo {
                system_log_line_floor,
                ..SetupRuntimeInfo::default()
            }));
            if let Some(stdout) = child.stdout.take() {
                spawn_setup_runtime_log_reader(stdout, info.clone());
            }
            if let Some(stderr) = child.stderr.take() {
                spawn_setup_runtime_log_reader(stderr, info.clone());
            }
            *guard = Some(SetupRuntime { child, info });
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(runtime_base_url) = setup_backend_setup_runtime_info(state)
            .and_then(|info| info.local_base_url)
            .filter(|value| !value.trim().is_empty())
            && setup_backend_runtime_base_responds(&runtime_base_url).await
        {
            return Ok(());
        }
        {
            let mut guard = state
                .setup_runtime
                .lock()
                .map_err(|_| anyhow!("setup runtime lock poisoned"))?;
            if let Some(runtime) = guard.as_mut()
                && let Some(status) = runtime.child.try_wait().context("inspect setup runtime")?
            {
                *guard = None;
                return Err(anyhow!(
                    "setup-started runtime exited before it was ready: {status}"
                ));
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!("runtime did not become ready within 90 seconds"));
        }
        tokio::time::sleep(Duration::from_millis(750)).await;
    }
}

fn setup_runtime_system_log_line_count(bundle_path: &Path) -> Result<usize> {
    let log_path = bundle_path.join("logs").join("system.log");
    let file = match std::fs::File::open(&log_path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err).with_context(|| format!("read {}", log_path.display())),
    };
    Ok(std::io::BufReader::new(file).lines().count())
}

fn spawn_setup_runtime_log_reader<R>(stream: R, info: Arc<Mutex<SetupRuntimeInfo>>)
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stream);
        for line in reader.lines().map_while(std::result::Result::ok) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(mut guard) = info.lock() {
                if let Some(value) = setup_runtime_log_value(trimmed, "HTTP:") {
                    guard.local_base_url = Some(value);
                }
                if let Some(value) = setup_runtime_log_value(trimmed, "Public:") {
                    guard.public_base_url = Some(value);
                }
                if trimmed.starts_with("Ready.") {
                    guard.ready = true;
                }
            }
        }
    });
}

fn setup_runtime_log_value(line: &str, label: &str) -> Option<String> {
    let value = line.strip_prefix(label)?.trim().trim_end_matches('/');
    (!value.is_empty()).then(|| value.to_string())
}

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
                provider_setup_status: JsonMap::new(),
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
        bundle_name: crate::bundle::read_bundle_name(bundle_path).ok().flatten(),
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
                provider_setup_status: JsonMap::new(),
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
                provider_setup_status: JsonMap::new(),
            }
        }
        Err(e) => ExecutionResult {
            success: false,
            stdout,
            stderr: format!("Execution failed: {e}"),
            manual_steps: vec![],
            provider_setup_status: JsonMap::new(),
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

fn merge_scalar_values(target: &mut JsonMap<String, Value>, source: &Value) {
    let Some(object) = source.as_object() else {
        return;
    };
    for (key, value) in object {
        match value {
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                target.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
}

fn public_setup_action_values(values: &JsonMap<String, Value>) -> Value {
    Value::Object(
        values
            .iter()
            .filter(|(key, value)| {
                !is_setup_action_secret_key(key)
                    && matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

fn redact_setup_action_output(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    if is_setup_action_secret_key(key) {
                        (key.clone(), Value::String("[redacted]".to_string()))
                    } else {
                        (key.clone(), redact_setup_action_output(value))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_setup_action_output).collect()),
        _ => value.clone(),
    }
}

fn is_setup_action_secret_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    normalized.contains("accesstoken")
        || normalized.contains("refreshtoken")
        || normalized.contains("idtoken")
        || normalized.contains("devicecode")
        || normalized.contains("clientsecret")
        || normalized.contains("signingsecret")
        || normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("credential")
        || normalized.ends_with("token")
}

struct SetupActionFinalUrlContext<'a> {
    bundle_root: &'a Path,
    tenant: &'a str,
    team: Option<&'a str>,
    provider_id: &'a str,
    action_id: &'a str,
}

fn setup_action_final_url_value(
    descriptor: &Value,
    action: &Value,
    config: &JsonMap<String, Value>,
    context: &SetupActionFinalUrlContext<'_>,
) -> Result<Option<(String, String)>> {
    let key = setup_action_final_url_key(descriptor);
    if let Some(key) = key.as_deref()
        && let Some(url) = config
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        return Ok(Some((key.to_string(), url.to_string())));
    }
    let Some(url) = build_oauth_install_url(
        context.bundle_root,
        action,
        config,
        context.tenant,
        context.team,
        context.provider_id,
        context.action_id,
    )?
    else {
        return Ok(None);
    };
    Ok(Some((
        key.unwrap_or_else(|| "oauth_authorize_url".to_string()),
        url,
    )))
}

fn setup_action_final_url_key(descriptor: &Value) -> Option<String> {
    descriptor
        .get("actions")
        .and_then(Value::as_array)?
        .iter()
        .filter(|action| action.get("kind").and_then(Value::as_str) == Some("deep_link"))
        .find_map(|action| {
            let template = action.get("url_template").and_then(Value::as_str)?;
            if let Some(name) = template
                .strip_prefix('{')
                .and_then(|value| value.strip_suffix('}'))
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                return Some(name.to_string());
            }
            action
                .get("requires")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(Value::as_str)
                .find(|value| value.ends_with("_url"))
                .map(ToString::to_string)
        })
}

fn build_oauth_install_url(
    bundle_root: &Path,
    action: &Value,
    config: &JsonMap<String, Value>,
    tenant: &str,
    team: Option<&str>,
    provider_id: &str,
    action_id: &str,
) -> Result<Option<String>> {
    let Some(authorize_url) = config
        .get("oauth_authorize_url")
        .and_then(Value::as_str)
        .or_else(|| action.get("authorize_url").and_then(Value::as_str))
    else {
        return Ok(None);
    };
    let mut parsed = Url::parse(authorize_url).context("setup action authorize_url is invalid")?;
    if let Some(client_id_field) = action.get("client_id_field").and_then(Value::as_str)
        && let Some(client_id) = config.get(client_id_field).and_then(Value::as_str)
        && !client_id.trim().is_empty()
    {
        set_url_query_key(&mut parsed, "client_id", client_id.trim());
    }
    if let Some(scopes) = action.get("scopes").and_then(Value::as_array) {
        let scopes = scopes
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        if !scopes.is_empty() {
            let separator = action
                .get("scope_separator")
                .and_then(Value::as_str)
                .unwrap_or(",");
            if !url_query_contains(&parsed, "scope") {
                parsed
                    .query_pairs_mut()
                    .append_pair("scope", &scopes.join(separator));
            }
        }
    }
    if let Some(redirect_path) = action.get("redirect_path").and_then(Value::as_str)
        && let Some(public_base_url) = config.get("public_base_url").and_then(Value::as_str)
        && !public_base_url.trim().is_empty()
    {
        let redirect_uri = format!(
            "{}{}",
            public_base_url.trim().trim_end_matches('/'),
            if redirect_path.starts_with('/') {
                redirect_path.to_string()
            } else {
                format!("/{redirect_path}")
            }
        );
        set_url_query_key(&mut parsed, "redirect_uri", &redirect_uri);
    }
    let mut persisted_action = action.clone();
    let Some(object) = persisted_action.as_object_mut() else {
        anyhow::bail!("setup action must be an object");
    };
    object.insert("id".to_string(), Value::String(action_id.to_string()));
    object.insert(
        "provider_id".to_string(),
        Value::String(provider_id.to_string()),
    );
    remove_url_query_key(&mut parsed, "state");
    object.insert(
        "authorize_url".to_string(),
        Value::String(parsed.to_string()),
    );
    let mut actions = crate::setup_actions::extract_setup_actions(
        provider_id,
        tenant,
        team,
        &serde_json::json!({ "setup_actions": [persisted_action] }),
    )?;
    crate::setup_actions::sign_pending_oauth_actions(bundle_root, &mut actions)?;
    crate::setup_actions::persist_setup_actions(bundle_root, &actions)?;
    Ok(actions.into_iter().find_map(|action| action.authorize_url))
}

fn url_query_contains(url: &Url, key: &str) -> bool {
    url.query_pairs().any(|(candidate, _)| candidate == key)
}

fn remove_url_query_key(url: &mut Url, key: &str) {
    replace_url_query_pairs(url, |candidate, _| candidate != key);
}

fn set_url_query_key(url: &mut Url, key: &str, value: &str) {
    replace_url_query_pairs(url, |candidate, _| candidate != key);
    url.query_pairs_mut().append_pair(key, value);
}

fn replace_url_query_pairs<F>(url: &mut Url, keep: F)
where
    F: Fn(&str, &str) -> bool,
{
    let pairs = url
        .query_pairs()
        .filter(|(candidate, value)| keep(candidate, value))
        .map(|(candidate, value)| (candidate.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    if !pairs.is_empty() {
        url.query_pairs_mut().extend_pairs(pairs);
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

    let (list_columns, min_rows, max_rows) = q
        .list
        .as_ref()
        .map(|list| {
            let cols: Vec<ListColumnInfo> = list
                .fields
                .iter()
                .map(|c| ListColumnInfo {
                    id: c.id.clone(),
                    title: c.title.clone(),
                    kind: format!("{:?}", c.kind),
                    required: c.required,
                    help: c.description.clone(),
                    placeholder: None,
                    choices: c.choices.clone(),
                    default_value: c.default_value.clone(),
                    // multilingual is set by the caller via overlay_setup_extras —
                    // qa-spec QuestionSpec has no slot for it, so we leave it
                    // false here and let the UI loop fix it up from
                    // SetupQuestionExtras.column_multilingual.
                    multilingual: false,
                })
                .collect();
            (Some(cols), list.min_items, list.max_items)
        })
        .unwrap_or((None, None, None));

    QuestionInfo {
        id: q.id.clone(),
        title,
        kind: format!("{:?}", q.kind),
        required: q.required,
        secret: q.secret,
        default_value: q.default_value.clone(),
        saved_value: None,
        saved_rows: None,
        help,
        choices: q.choices.clone(),
        visible_if,
        placeholder: None,
        group: None,
        docs_url: None,
        list_columns,
        min_rows,
        max_rows,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProviderSetupEventRequest, UiState, build_router, persist_provider_setup_event,
        persist_ui_draft, prefill_has_cloud_deployment_targets, read_provider_setup_events,
        redact_provider_setup_event_detail,
    };
    use crate::secrets::open_dev_store;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use greentic_secrets_lib::{SecretFormat, SecretsStore};
    use serde_json::{Map as JsonMap, Value, json};
    use std::io::Write;
    use std::sync::Mutex;
    use tokio::sync::broadcast;
    use tower::ServiceExt;
    use zip::write::SimpleFileOptions;

    fn test_ui_state(bundle_root: &std::path::Path) -> std::sync::Arc<UiState> {
        let (shutdown_tx, _) = broadcast::channel(1);
        std::sync::Arc::new(UiState {
            bundle_path: bundle_root.to_path_buf(),
            tenant: "demo".to_string(),
            team: Some("support".to_string()),
            env: "dev".to_string(),
            advanced: false,
            locale: None,
            prefill_answers: None,
            output_target: None,
            local_base_url: "http://127.0.0.1:12345".to_string(),
            setup_session_id: "test-session".to_string(),
            setup_tunnel: Mutex::new(None),
            setup_tunnel_start: tokio::sync::Mutex::new(()),
            setup_runtime: Mutex::new(None),
            setup_runtime_start: tokio::sync::Mutex::new(()),
            shutdown_tx,
            result: Mutex::new(None),
        })
    }

    fn test_jwt_with_exp(exp: u64) -> String {
        let header = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"alg":"none"}"#,
        );
        let claims = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            format!(r#"{{"exp":{exp}}}"#),
        );
        format!("{header}.{claims}.")
    }

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

    fn write_pack_with_setup_backend_contract(
        path: &std::path::Path,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Contract Provider",
                "extensions": {
                    "greentic.setup.backend-contract.v1": {
                        "inline": {
                            "schema_id": "greentic.setup.backend-contract.v1",
                            "schema_version": "1.0.0",
                            "provider_id": provider_id,
                            "base_path": format!("/v1/messaging/setup/{provider_id}/{{tenant}}"),
                            "routes": {
                                "state": format!("GET /v1/messaging/setup/{provider_id}/{{tenant}}"),
                                "next": format!("POST /v1/messaging/setup/{provider_id}/{{tenant}}/next"),
                                "config": format!("POST /v1/messaging/setup/{provider_id}/{{tenant}}/config")
                            },
                            "server_owned_config_keys": [
                                "oauth_kind",
                                "oauth_device_code",
                                "oauth_user_code",
                                "graph_access_token",
                                "azure_management_access_token",
                                "bot_access_token"
                            ],
                            "required_order": [
                                "admin_consent",
                                "publish",
                                "first_runtime_evidence"
                            ],
                            "state_shape": {
                                "setup_status": {
                                    "ok": "boolean",
                                    "items": "array",
                                    "next": "string"
                                },
                                "values": {
                                    "config": "object"
                                }
                            }
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        zip.finish()?;
        Ok(())
    }

    fn write_pack_with_setup_machine(
        path: &std::path::Path,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Machine Provider",
                "extensions": {
                    "greentic.setup.machine.v1": {
                        "inline": {
                            "version": 1,
                            "id": "ui-machine",
                            "display_name": "UI Machine",
                            "entry_step": "start",
                            "steps": [
                                {
                                    "id": "start",
                                    "kind": "manual_action",
                                    "title": "Start",
                                    "auto_complete": true,
                                    "on_success": "complete"
                                }
                            ]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        zip.finish()?;
        Ok(())
    }

    fn write_pack_with_final_setup_actions(
        path: &std::path::Path,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Action Provider",
                "extensions": {
                    "greentic.setup.actions.v1": {
                        "kind": "greentic.setup.actions.v1",
                        "inline": {
                            "schema_id": "greentic.setup.actions.v1",
                            "provider_id": provider_id,
                            "actions": [{
                                "id": "add-to-provider",
                                "label": "Add to Provider",
                                "kind": "deep_link",
                                "url_template": "{add_url}",
                                "requires": ["add_url"],
                                "visible_when": {
                                    "setup_status.ok": true
                                }
                            }]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        zip.finish()?;
        Ok(())
    }

    fn write_pack_with_legacy_setup_actions(
        path: &std::path::Path,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Legacy Action Provider"
            })
            .to_string()
            .as_bytes(),
        )?;
        zip.start_file("assets/setup.yaml", SimpleFileOptions::default())?;
        zip.write_all(
            br#"
provider_id: generic
version: 1
title: Generic provider setup
setup_actions:
  - id: create_app
    label: Create Provider App
    kind: oauth_install_button
    provider_id: provider-alias
    authorize_url: "https://provider.example/install"
    redirect_path: "/oauth/callback/provider"
    client_id_field: provider_client_id
    scopes:
      - chat:write
    registration:
      component_ref: provider-setup
      op: setup_app_registration
      mock_result:
        ok: true
        provider_client_id: generated-client
        provider_client_secret: generated-secret
        oauth_authorize_url: https://provider.example/install?client_id=stale-client&redirect_uri=https%3A%2F%2Fold.example.test%2Fcallback&state=provider-state
"#,
        )?;
        zip.finish()?;
        Ok(())
    }

    fn write_pack_with_final_and_legacy_setup_actions(
        path: &std::path::Path,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Combined Action Provider",
                "extensions": {
                    "greentic.setup.actions.v1": {
                        "kind": "greentic.setup.actions.v1",
                        "inline": {
                            "schema_id": "greentic.setup.actions.v1",
                            "provider_id": provider_id,
                            "actions": [{
                                "id": "add-to-provider",
                                "label": "Add to Provider",
                                "kind": "deep_link",
                                "url_template": "{add_url}",
                                "requires": ["add_url"],
                                "visible_when": {
                                    "setup_status.ok": true
                                }
                            }]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        zip.start_file("assets/setup.yaml", SimpleFileOptions::default())?;
        zip.write_all(
            br#"
provider_id: generic
version: 1
title: Generic provider setup
setup_actions:
  - id: create_app
    label: Create Provider App
    kind: oauth_install_button
    provider_id: provider-alias
    authorize_url: "https://provider.example/install"
    redirect_path: "/oauth/callback/provider"
    client_id_field: provider_client_id
    scopes:
      - chat:write
    registration:
      component_ref: provider-setup
      op: setup_app_registration
      mock_result:
        ok: true
        provider_client_id: generated-client
        provider_client_secret: generated-secret
        oauth_authorize_url: https://provider.example/install?client_id=stale-client&redirect_uri=https%3A%2F%2Fold.example.test%2Fcallback&state=provider-state
"#,
        )?;
        zip.finish()?;
        Ok(())
    }

    fn write_pack_with_secret_and_public_questions(
        path: &std::path::Path,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Question Provider"
            })
            .to_string()
            .as_bytes(),
        )?;
        zip.start_file("assets/setup.yaml", SimpleFileOptions::default())?;
        zip.write_all(
            br#"
provider_id: generic
version: 1
title: Generic provider setup
questions:
  - name: provider_public_field
    title: Public field
    kind: string
    required: false
  - name: provider_secret_field
    title: Secret field
    kind: string
    required: true
    secret: true
"#,
        )?;
        zip.finish()?;
        Ok(())
    }

    fn write_pack_with_asset_setup_backend_contract(
        path: &std::path::Path,
        provider_id: &str,
        write_asset: bool,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Asset Contract Provider",
                "extensions": {
                    "greentic.setup.backend-contract.v1": {
                        "inline": {
                            "schema_id": "greentic.setup.backend-contract.v1",
                            "provider_id": provider_id,
                            "asset": "assets/setup/backend-contract.json"
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        if write_asset {
            zip.start_file(
                "assets/setup/backend-contract.json",
                SimpleFileOptions::default(),
            )?;
            zip.write_all(
                json!({
                    "schema_id": "greentic.setup.backend-contract.v1",
                    "schema_version": "1.0.0",
                    "provider_id": provider_id,
                    "base_path": format!("/v1/messaging/setup/{provider_id}/{{tenant}}"),
                    "routes": {
                        "state": format!("GET /v1/messaging/setup/{provider_id}/{{tenant}}"),
                        "next": format!("POST /v1/messaging/setup/{provider_id}/{{tenant}}/next"),
                        "config": format!("POST /v1/messaging/setup/{provider_id}/{{tenant}}/config")
                    },
                    "server_owned_config_keys": [
                        "oauth_kind",
                        "oauth_device_code",
                        "oauth_user_code",
                        "graph_access_token",
                        "azure_management_access_token",
                        "bot_access_token"
                    ],
                    "required_order": [
                        "graph_admin_consent",
                        "bot_app_identity",
                        "bot_framework_endpoint_registration"
                    ],
                    "states": [
                        {"id": "graph_admin_consent"},
                        {"id": "bot_app_identity"},
                        {"id": "bot_framework_endpoint_registration"}
                    ],
                    "guards": [
                        {"id": "server-owned-oauth-state"}
                    ]
                })
                .to_string()
                .as_bytes(),
            )?;
        }
        zip.finish()?;
        Ok(())
    }

    fn write_pack_with_unsupported_setup_action(
        path: &std::path::Path,
        provider_id: &str,
    ) -> anyhow::Result<()> {
        let file = std::fs::File::create(path)?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("pack.manifest.json", SimpleFileOptions::default())?;
        zip.write_all(
            json!({
                "pack_id": provider_id,
                "display_name": "Unsupported Action Provider",
                "extensions": {
                    "greentic.setup.backend-contract.v1": {
                        "inline": {
                            "schema_id": "greentic.setup.backend-contract.v1",
                            "schema_version": "1.0.0",
                            "provider_id": provider_id,
                            "base_path": format!("/v1/messaging/setup/{provider_id}/{{tenant}}"),
                            "routes": {
                                "state": format!("GET /v1/messaging/setup/{provider_id}/{{tenant}}"),
                                "next": format!("POST /v1/messaging/setup/{provider_id}/{{tenant}}/next"),
                                "config": format!("POST /v1/messaging/setup/{provider_id}/{{tenant}}/config")
                            },
                            "required_order": ["custom_step"],
                            "actions_schema_id": "greentic.setup.actions.v1",
                            "actions": [{
                                "id": "custom_step",
                                "executor": {
                                    "kind": "future_executor"
                                }
                            }]
                        }
                    }
                }
            })
            .to_string()
            .as_bytes(),
        )?;
        zip.finish()?;
        Ok(())
    }

    #[test]
    fn oauth_callback_page_tells_user_to_close_success_tab() {
        let page = super::oauth_callback_page(
            true,
            "OAuth setup complete",
            "OAuth setup complete for messaging-slack. You can close this tab.",
        );

        assert!(page.contains("window.close()"));
        assert!(page.contains("You can close this tab"));
    }

    #[test]
    fn oauth_device_code_client_id_uses_executor_default_without_materializing_config() {
        let executor = json!({
            "kind": "oauth_device_code",
            "client_id_config_key": "graph_setup_client_id",
            "client_id_default": "14d82eec-204b-4c2f-b7e8-296a70dab67e",
            "client_id_default_name": "Microsoft Graph Command Line Tools"
        });
        let config = JsonMap::new();

        let client_id = super::setup_backend_oauth_client_id(&executor, &config).unwrap();

        assert_eq!(client_id, "14d82eec-204b-4c2f-b7e8-296a70dab67e");
        assert!(config.get("graph_setup_client_id").is_none());
    }

    #[test]
    fn oauth_device_code_client_id_stays_empty_without_config_or_default() {
        let executor = json!({
            "kind": "oauth_device_code",
            "client_id_config_key": "graph_setup_client_id"
        });
        let config = JsonMap::new();

        let client_id = super::setup_backend_oauth_client_id(&executor, &config).unwrap();

        assert!(client_id.is_empty());
    }

    #[test]
    fn setup_backend_public_config_hides_server_owned_device_code_and_tokens() {
        let mut config = JsonMap::new();
        config.insert("oauth_kind".to_string(), Value::String("graph".to_string()));
        config.insert(
            "oauth_user_code".to_string(),
            Value::String("ABCD-EFGH".to_string()),
        );
        config.insert(
            "oauth_device_code".to_string(),
            Value::String("raw-device-code".to_string()),
        );
        config.insert(
            "graph_access_token".to_string(),
            Value::String("raw-access-token".to_string()),
        );

        let public = super::setup_backend_public_config(&config);

        assert_eq!(public["oauth_kind"], "graph");
        assert_eq!(public["oauth_user_code"], "ABCD-EFGH");
        assert!(public.get("oauth_device_code").is_none());
        assert!(public.get("graph_access_token").is_none());
    }

    #[test]
    fn setup_backend_defaults_include_public_base_url_from_static_routes() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::platform_setup::persist_static_routes_artifact(
            temp.path(),
            &crate::platform_setup::StaticRoutesPolicy {
                public_web_enabled: true,
                public_base_url: Some("https://runtime.example.com/base/".to_string()),
                ..crate::platform_setup::StaticRoutesPolicy::default()
            },
        )
        .expect("static routes");
        let state = test_ui_state(temp.path());

        let config = super::default_setup_backend_config_with_runtime_base(&state, "demo", None);

        assert_eq!(
            config["public_base_url"],
            "https://runtime.example.com/base"
        );
    }

    #[test]
    fn setup_backend_defaults_replace_empty_host_values() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::platform_setup::persist_static_routes_artifact(
            temp.path(),
            &crate::platform_setup::StaticRoutesPolicy {
                public_web_enabled: true,
                public_base_url: Some("https://runtime.example.com".to_string()),
                ..crate::platform_setup::StaticRoutesPolicy::default()
            },
        )
        .expect("static routes");
        let state = test_ui_state(temp.path());
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": ""
            }),
        );

        super::ensure_setup_backend_config_defaults(&state, "demo", &mut stored).unwrap();
        let config = stored["config"].as_object().unwrap();

        assert_eq!(config["public_base_url"], "https://runtime.example.com");
    }

    #[test]
    fn setup_backend_defaults_do_not_include_provider_setup_runtime_base_url() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());

        let config = super::default_setup_backend_config_with_runtime_base(
            &state,
            "demo",
            Some("http://127.0.0.1:9101/"),
        );

        assert!(config.get("provider_setup_base_url").is_none());
    }

    #[test]
    fn setup_backend_defaults_do_not_include_provider_setup_base_from_runtime_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let runtime_dir = temp
            .path()
            .join("state")
            .join("runtime")
            .join("demo.support");
        std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
        std::fs::write(
            runtime_dir.join("endpoints.json"),
            json!({
                "tenant": "demo",
                "team": "support",
                "gateway_listen_addr": "127.0.0.1",
                "gateway_port": 8081
            })
            .to_string(),
        )
        .expect("runtime endpoints");
        let state = test_ui_state(temp.path());

        let config = super::default_setup_backend_config(&state, "demo");

        assert!(config.get("provider_setup_base_url").is_none());
    }

    #[test]
    fn setup_backend_infers_tunnel_mode_from_ephemeral_public_base_url() {
        assert_eq!(
            super::setup_backend_infer_tunnel_mode_from_public_base_url(
                "https://demo.trycloudflare.com"
            )
            .as_deref(),
            Some("cloudflared")
        );
        assert_eq!(
            super::setup_backend_infer_tunnel_mode_from_public_base_url(
                "https://demo.ngrok-free.app"
            )
            .as_deref(),
            Some("ngrok")
        );
        assert_eq!(
            super::setup_backend_infer_tunnel_mode_from_public_base_url(
                "https://runtime.example.com"
            ),
            None
        );
    }

    #[test]
    fn setup_backend_tunnel_mode_defaults_to_cloudflared_for_local_setup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());

        let mode = super::setup_backend_tunnel_mode(&state).expect("tunnel mode");

        assert_eq!(mode.as_deref(), Some("cloudflared"));
    }

    #[test]
    fn setup_backend_tunnel_mode_honors_persisted_off() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::platform_setup::persist_tunnel_artifact(
            temp.path(),
            &crate::platform_setup::TunnelAnswers {
                mode: Some("off".to_string()),
            },
        )
        .expect("persist tunnel");
        let state = test_ui_state(temp.path());

        let mode = super::setup_backend_tunnel_mode(&state).expect("tunnel mode");

        assert_eq!(mode.as_deref(), Some("off"));
    }

    #[test]
    fn setup_backend_tunnel_mode_honors_persisted_ngrok() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::platform_setup::persist_tunnel_artifact(
            temp.path(),
            &crate::platform_setup::TunnelAnswers {
                mode: Some("ngrok".to_string()),
            },
        )
        .expect("persist tunnel");
        let state = test_ui_state(temp.path());

        let mode = super::setup_backend_tunnel_mode(&state).expect("tunnel mode");

        assert_eq!(mode.as_deref(), Some("ngrok"));
    }

    #[test]
    fn setup_action_tunnel_mode_uses_persisted_selection_when_request_omits_tunnel() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::platform_setup::persist_tunnel_artifact(
            temp.path(),
            &crate::platform_setup::TunnelAnswers {
                mode: Some("ngrok".to_string()),
            },
        )
        .expect("persist tunnel");
        let state = test_ui_state(temp.path());

        assert_eq!(
            super::setup_action_tunnel_mode(&state, None).unwrap(),
            "ngrok"
        );
        assert_eq!(
            super::setup_action_tunnel_mode(&state, Some("cloudflared")).unwrap(),
            "cloudflared"
        );
    }

    #[test]
    fn setup_action_provider_answers_allow_public_base_url_injection_for_empty_request() {
        let mut answers = JsonMap::new();

        super::ensure_setup_action_provider_answers(&mut answers, "messaging-example");

        assert!(crate::setup_tunnel::should_start_setup_tunnel(
            "ngrok", &answers
        ));
        crate::setup_tunnel::inject_setup_public_base_url(
            &mut answers,
            "https://example.ngrok-free.app",
        );
        assert_eq!(
            answers["messaging-example"]["public_base_url"],
            json!("https://example.ngrok-free.app")
        );
    }

    #[test]
    fn setup_backend_tunnel_mode_does_not_default_when_deployer_pack_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let packs_dir = temp.path().join("packs");
        std::fs::create_dir_all(&packs_dir).expect("packs dir");
        std::fs::write(packs_dir.join("terraform.gtpack"), b"placeholder").expect("pack");
        let state = test_ui_state(temp.path());

        let mode = super::setup_backend_tunnel_mode(&state).expect("tunnel mode");

        assert_eq!(mode, None);
    }

    #[test]
    fn setup_backend_completion_rejects_stale_ephemeral_tunnel_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["register_endpoint"],
                "actions": [{
                    "id": "register_endpoint",
                    "completion": {
                        "state_path": "last_reconcile.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_reconcile"
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": "https://old.trycloudflare.com"
            }),
        );
        stored.insert("last_reconcile".to_string(), json!({"ok": true}));

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);

        assert_eq!(rendered["setup_status"]["ok"], false);
        assert_eq!(rendered["setup_status"]["items"][0]["state"], "pending");
    }

    #[test]
    fn setup_backend_completion_accepts_current_successful_action_with_ephemeral_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["register_endpoint", "publish"],
                "actions": [{
                    "id": "register_endpoint",
                    "completion": {
                        "state_path": "last_reconcile.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_reconcile"
                    }
                }, {
                    "id": "publish",
                    "completion": {
                        "state_path": "last_publish.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_publish"
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": "https://new.trycloudflare.com"
            }),
        );
        stored.insert(
            "last_reconcile".to_string(),
            json!({
                "ok": true,
                "runtime_context": {
                    "public_base_url": "https://new.trycloudflare.com",
                    "public_base_url_is_ephemeral_tunnel": true,
                    "active_tunnel_public_base_url": null
                }
            }),
        );
        stored.insert(
            "last_setup_result".to_string(),
            json!({
                "step": "register_endpoint",
                "ok": true,
                "next": "click again"
            }),
        );

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);

        assert_eq!(rendered["setup_status"]["ok"], false);
        assert_eq!(rendered["setup_status"]["items"][0]["state"], "done");
        assert_eq!(rendered["setup_status"]["items"][1]["state"], "pending");
        assert_eq!(
            rendered["setup_status"]["next"],
            "Continue setup to run the next step."
        );
    }

    #[test]
    fn setup_backend_hides_current_downstream_output_when_prior_step_pending() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["register_endpoint", "publish"],
                "actions": [{
                    "id": "register_endpoint",
                    "completion": {
                        "state_path": "last_reconcile.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_reconcile"
                    }
                }, {
                    "id": "publish",
                    "completion": {
                        "state_path": "last_publish.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_publish"
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": "https://stale.trycloudflare.com"
            }),
        );
        stored.insert(
            "last_reconcile".to_string(),
            json!({
                "ok": true,
                "runtime_context": {
                    "public_base_url": "https://stale.trycloudflare.com",
                    "public_base_url_is_ephemeral_tunnel": true,
                    "active_tunnel_public_base_url": null
                }
            }),
        );
        stored.insert(
            "last_publish".to_string(),
            json!({"ok": true, "url": "https://example.com"}),
        );
        stored.insert(
            "last_setup_result".to_string(),
            json!({
                "step": "publish",
                "ok": true,
                "next": "click again"
            }),
        );

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);

        assert_eq!(rendered["setup_status"]["items"][0]["state"], "pending");
        assert_eq!(rendered["setup_status"]["items"][1]["state"], "pending");
        assert_eq!(rendered["values"]["last_publish"]["ok"], false);
        assert_eq!(rendered["values"]["last_publish"]["stale"], true);
    }

    #[test]
    fn setup_backend_completion_rejects_changed_public_base_url_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["register_endpoint"],
                "actions": [{
                    "id": "register_endpoint",
                    "completion": {
                        "state_path": "last_reconcile.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_reconcile"
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": "https://new.example.com"
            }),
        );
        stored.insert(
            "last_reconcile".to_string(),
            json!({
                "ok": true,
                "runtime_context": {
                    "public_base_url": "https://old.example.com"
                }
            }),
        );

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);

        assert_eq!(rendered["setup_status"]["ok"], false);
        assert_eq!(rendered["setup_status"]["items"][0]["state"], "pending");
    }

    #[test]
    fn setup_backend_oauth_completion_keeps_done_when_access_token_expires() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["management_consent"],
                "actions": [{
                    "id": "management_consent",
                    "completion": {
                        "state_path": "oauth.management.ok",
                        "exists": true
                    },
                    "executor": {
                        "kind": "oauth_device_code",
                        "oauth_kind": "management",
                        "token_store_key": "management_access_token"
                    }
                }]
            }),
            load_error: None,
        };
        let expired_token = test_jwt_with_exp(1);
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "management_access_token": expired_token
            }),
        );
        stored.insert(
            "oauth".to_string(),
            json!({
                "management": {
                    "ok": true,
                    "token_store_key": "management_access_token"
                }
            }),
        );

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);

        assert_eq!(rendered["setup_status"]["ok"], true);
        assert_eq!(rendered["setup_status"]["items"][0]["state"], "done");
    }

    #[test]
    fn setup_backend_oauth_required_resume_routes_to_matching_oauth_action() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "generic-provider".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["auth", "publish"],
                "actions": [{
                    "id": "auth",
                    "completion": {
                        "state_path": "oauth.default.ok",
                        "exists": true
                    },
                    "executor": {
                        "kind": "oauth_device_code",
                        "oauth_kind": "default",
                        "token_store_key": "access_token"
                    }
                }, {
                    "id": "publish",
                    "completion": {
                        "state_path": "last_publish.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_publish"
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "access_token": "expired-token"
            }),
        );
        stored.insert(
            "oauth".to_string(),
            json!({
                "default": {
                    "ok": true,
                    "token_store_key": "access_token"
                }
            }),
        );
        stored.insert("last_publish".to_string(), json!({"ok": true}));

        let publish_action = super::setup_backend_action_by_id(&contract, "publish").unwrap();
        let result = super::setup_backend_oauth_required_result(
            publish_action,
            "access_token",
            "authenticated request failed",
            &json!({
                "ok": false,
                "status": 401,
                "body": { "error": "Lifetime validation failed, the token is expired." }
            }),
        )
        .unwrap();
        crate::setup_backend_contract::update_oauth_resume(&mut stored, &result);

        let first_pending =
            super::setup_backend_first_pending_step(&state, &contract, "demo", &stored);

        assert_eq!(first_pending, "auth");
        assert_eq!(stored["oauth_resume"]["token_store_key"], "access_token");
        assert_eq!(stored["oauth_resume"]["resume_step"], "publish");
    }

    #[test]
    fn setup_backend_provider_http_oauth_required_uses_token_key_from_action_body() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "generic-provider".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["management_auth", "graph_auth", "register"],
                "actions": [{
                    "id": "management_auth",
                    "completion": {
                        "state_path": "oauth.management.ok",
                        "exists": true
                    },
                    "executor": {
                        "kind": "oauth_device_code",
                        "oauth_kind": "management",
                        "token_store_key": "management_access_token"
                    }
                }, {
                    "id": "graph_auth",
                    "completion": {
                        "state_path": "oauth.graph.ok",
                        "exists": true
                    },
                    "executor": {
                        "kind": "oauth_device_code",
                        "oauth_kind": "graph",
                        "token_store_key": "graph_access_token"
                    }
                }, {
                    "id": "register",
                    "completion": {
                        "state_path": "last_register.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_register",
                        "body": {
                            "access_token": "{management_access_token}"
                        }
                    }
                }]
            }),
            load_error: None,
        };
        let action = super::setup_backend_action_by_id(&contract, "register").unwrap();
        let response = json!({
            "ok": false,
            "response": {
                "ok": false,
                "blocked": true,
                "error": "request failed (HTTP 401): token is expired"
            }
        });

        let result =
            super::setup_backend_provider_http_oauth_required_result(&contract, action, &response)
                .unwrap();
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "management_access_token": "expired-management-token",
                "graph_access_token": "still-present"
            }),
        );
        stored.insert(
            "oauth".to_string(),
            json!({
                "management": {
                    "ok": true,
                    "token_store_key": "management_access_token"
                },
                "graph": {
                    "ok": true,
                    "token_store_key": "graph_access_token"
                }
            }),
        );
        crate::setup_backend_contract::update_oauth_resume(&mut stored, &result);

        let first_pending =
            super::setup_backend_first_pending_step(&state, &contract, "demo", &stored);

        assert_eq!(result["result"]["error"], "oauth_required");
        assert_eq!(
            result["result"]["token_store_key"],
            "management_access_token"
        );
        assert_eq!(first_pending, "management_auth");
    }

    #[test]
    fn setup_backend_provider_http_oauth_required_handles_invalid_access_token() {
        let contract = super::ProviderBackendContract {
            provider_id: "generic-provider".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["auth", "discover"],
                "actions": [{
                    "id": "auth",
                    "completion": {
                        "state_path": "oauth.default.ok",
                        "exists": true
                    },
                    "executor": {
                        "kind": "oauth_device_code",
                        "token_store_key": "access_token"
                    }
                }, {
                    "id": "discover",
                    "completion": {
                        "state_path": "last_discover.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_discover",
                        "body": {
                            "access_token": "{access_token}"
                        }
                    }
                }]
            }),
            load_error: None,
        };
        let action = super::setup_backend_action_by_id(&contract, "discover").unwrap();
        let response = json!({
            "ok": false,
            "response": {
                "ok": false,
                "blocked": true,
                "error": "Azure subscription discovery failed (HTTP 401): The access token is invalid."
            }
        });

        let result =
            super::setup_backend_provider_http_oauth_required_result(&contract, action, &response)
                .expect("invalid token should require OAuth");

        assert_eq!(result["result"]["error"], "oauth_required");
        assert_eq!(result["result"]["token_store_key"], "access_token");
    }

    #[test]
    fn setup_backend_oauth_resume_clears_after_matching_token_refresh() {
        let mut stored = JsonMap::new();
        stored.insert(
            "oauth_resume".to_string(),
            json!({
                "token_store_key": "access_token",
                "resume_step": "publish"
            }),
        );

        super::setup_backend_clear_oauth_resume_for_token(&mut stored, "other_token");
        assert!(stored.get("oauth_resume").is_some());

        super::setup_backend_clear_oauth_resume_for_token(&mut stored, "access_token");
        assert!(stored.get("oauth_resume").is_none());
    }

    #[test]
    fn setup_backend_render_keeps_completed_outputs_when_token_expires() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "generic-provider".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["auth", "publish"],
                "actions": [{
                    "id": "auth",
                    "completion": {
                        "state_path": "oauth.default.ok",
                        "exists": true
                    },
                    "executor": {
                        "kind": "oauth_device_code",
                        "token_store_key": "access_token"
                    }
                }, {
                    "id": "publish",
                    "completion": {
                        "state_path": "last_publish.ok",
                        "equals": true
                    },
                    "executor": {
                        "kind": "provider_http",
                        "state_store_key": "last_publish"
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "access_token": test_jwt_with_exp(1)
            }),
        );
        stored.insert(
            "oauth".to_string(),
            json!({
                "default": {
                    "ok": true,
                    "token_store_key": "access_token"
                }
            }),
        );
        stored.insert(
            "last_publish".to_string(),
            json!({"ok": true, "url": "https://example.com"}),
        );

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);

        assert_eq!(rendered["setup_status"]["ok"], true);
        assert_eq!(rendered["setup_status"]["items"][0]["state"], "done");
        assert_eq!(rendered["setup_status"]["items"][1]["state"], "done");
        assert_eq!(rendered["setup_status"]["reset"], false);
        assert_eq!(rendered["values"]["oauth"]["default"]["ok"], true);
        assert_eq!(rendered["values"]["last_publish"]["ok"], true);
    }

    #[test]
    fn setup_web_component_accepts_server_reset_state() {
        let source = r#"
  _isStaleState(nextState) {
    if (!this._state) return false;
    const current = this._stateRank(this._state);
    const next = this._stateRank(nextState);
    if (next.done < current.done) return true;
    return false;
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(!patched.contains("setup_status.reset === true"));
        assert!(patched.contains("if (next.done < current.done) return true;"));
    }

    #[test]
    fn setup_web_component_pending_device_login_is_not_error_outcome() {
        let source = r#"
  _outcomeMessage(result) {
    if (!result || typeof result !== "object") return "";
    if (result.ok === false) {
      return this._providerSetupError(result) || result.next || result.error || this._t("actionFailed");
    }
    return "";
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(patched.contains("pending_device_login"));
        assert!(patched.contains("return this._providerSetupError(result)"));
    }

    #[test]
    fn setup_web_component_allows_retryable_blocked_action() {
        let source = r#"
  _currentAction() {
    const status = this._status();
    if (status.blocked) {
      return {
        kind: "blocked-refresh",
        label: this._t("refreshAfterManualAction")
      };
    }
    return { kind: "continue", label: this._t("continue") || "Continue setup" };
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(patched.contains("status.blocked && !status.blocked.retryable"));
        assert!(patched.contains("kind: \"blocked-refresh\""));
    }

    #[test]
    fn setup_web_component_verifies_teams_install_after_publish_without_session_flag() {
        let source = r#"
  _currentAction() {
    if (publish.ok && !install.ok && addToTeamsUrl) {
      if (this._manualActions.addToTeamsOpened) {
        return {
          kind: "continue",
          label: this._t("verifyTeamsInstall")
        };
      }
      return {
        kind: "add-to-teams",
        label: this._t("addToTeams"),
        url: addToTeamsUrl
      };
    }
  }

  _actionHtml(action) {
    return `<button type="button" class="primary" data-action="run-current">${this._escape(action.label || this._t("continue"))}</button>`;
  }

  async _executeManagedAction(action) {
    let waitAction = action;
    if (waitAction.kind === "continue") {
      const result = await this._request("POST", this._endpoint("next"), this._collectConfig());
    }
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(
            patched.contains("greentic-setup always offers install verification after publish")
        );
        assert!(patched.contains("label: this._t(\"verifyTeamsInstall\")"));
        assert!(patched.contains("stepId: \"teams_app_user_install\""));
        assert!(patched.contains("addToTeamsUrl"));
        assert!(patched.contains("greentic-setup can run a targeted setup action"));
        assert!(patched.contains("/action/${encodeURIComponent(waitAction.stepId)}"));
        assert!(patched.contains("greentic-setup exposes Add to Teams next to Verify"));
        assert!(!patched.contains("kind: \"add-to-teams\""));
    }

    #[test]
    fn setup_web_component_ignores_stale_runtime_observation_for_bot_chat_action() {
        let source = r#"
  _currentAction() {
    const values = this._state && this._state.values || {};
    const firstMessage = values.last_activity || values.last_webchat_conversation;
    if (install.ok && !firstMessage && openBotChatUrl) {
      return {
        kind: "open-chat",
        label: this._t("openBotChat"),
        url: openBotChatUrl
      };
    }
  }

  _waitingForFirstBotMessage() {
    const values = this._state && this._state.values || {};
    return Boolean(
      install.ok
        && (installData.open_bot_chat_url || teams.open_bot_chat_url)
        && !(values.last_activity || values.last_webchat_conversation)
    );
  }

  _snapshot() {
    const values = this._state && this._state.values || {};
    return {
      firstMessage: Boolean(values.last_activity || values.last_webchat_conversation)
    };
  }

  async _preflightAction(action) {
    if (!action || action.kind !== "continue") return "";
    const pending = this._currentPendingStepId();
    if (pending === "first_bot_framework_post") {
      return await this._runtimeIngressPreflight();
    }
    return "";
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(patched.contains("greentic-setup ignores stale runtime observations"));
        assert!(patched.contains("value && !value.stale"));
        assert!(patched.contains("!value.stale)"));
        assert!(
            patched.contains("greentic-setup waits for the Bot Framework endpoint registration")
        );
        assert!(patched.contains("pendingStep !== \"first_bot_framework_post\""));
        assert!(patched.contains("greentic-setup lets the backend/runtime observation path"));
        assert!(!patched.contains("return await this._runtimeIngressPreflight();"));
        assert!(!patched.contains(
            "const firstMessage = values.last_activity || values.last_webchat_conversation;"
        ));
        assert!(!patched.contains(
            "firstMessage: Boolean(values.last_activity || values.last_webchat_conversation)"
        ));
    }

    #[test]
    fn setup_web_component_suppresses_action_only_after_observed_completion() {
        let source = r#"
  _currentAction() {
    const complete = items.length > 0 && items.every((item) => item.state === "done");
    const values = this._state && this._state.values || {};
    const firstMessage = values.last_activity || values.last_webchat_conversation;

    if (install.ok && !firstMessage && openBotChatUrl) {
      return {
        kind: "open-chat",
        label: this._t("openBotChat"),
        url: openBotChatUrl
      };
    }

    if (!complete) {
      return {
        kind: "continue",
        label: this._t("continue") || "Continue setup"
      };
    }

    if (openBotChatUrl) {
      return {
        kind: "open-chat",
        label: this._t("openBotChat"),
        url: openBotChatUrl
      };
    }

    return {
      kind: "refresh",
      label: this._t("refresh")
    };
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(patched.contains("greentic-setup has no next action after observed completion"));
        assert!(patched.contains("if (complete && firstMessage)"));
        assert!(patched.contains("return null;"));
        assert!(
            patched
                .find("if (install.ok && !firstMessage && openBotChatUrl)")
                .unwrap()
                < patched.find("if (complete && firstMessage)").unwrap()
        );
    }

    #[test]
    fn setup_web_component_oauth_resume_keeps_device_login_action_visible() {
        let source = r#"
  _oauthComplete(kind) {
    const values = this._state && this._state.values || {};
    const oauth = values.oauth || {};
    return Boolean(oauth[kind || "default"] && oauth[kind || "default"].ok);
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(patched.contains("greentic-setup oauth_resume keeps refreshed OAuth incomplete"));
        assert!(patched.contains("tokenKey === \"azure_management_access_token\""));
        assert!(patched.contains("tokenKey === \"graph_access_token\""));
        assert!(patched.contains("return false;"));
    }

    #[test]
    fn setup_web_component_prefers_latest_oauth_response_code_over_stale_config_code() {
        let source = r#"
  _pendingLoginFromState() {
    const cfg = this._config();
    const values = this._state && this._state.values || {};
    const response = values.last_oauth && values.last_oauth.response || {};
    const oauthKind = this._oauthKind();
    const codeKey = oauthKind === "management" ? "azure_management_user_code" : "oauth_user_code";
    const userCode = cfg[codeKey] || response.user_code || response.userCode;
    if (!userCode) return null;
  }
"#;

        let patched = super::patch_setup_web_component_reset_guard(source.to_string());

        assert!(patched.contains("greentic-setup prefers newest OAuth response code"));
        assert!(patched.contains("response.user_code || response.userCode || cfg[codeKey]"));
        assert!(!patched.contains("cfg[codeKey] || response.user_code"));
    }

    #[test]
    fn setup_backend_marks_runtime_tunnel_block_retryable() {
        let setup_result = json!({
            "ok": false,
            "next": "runtime/tunnel not running",
            "result": {
                "blocked": true,
                "error": "runtime/tunnel not running"
            }
        });

        let blocked = super::setup_backend_blocked_from_result(&setup_result).unwrap();

        assert_eq!(blocked["retryable"], json!(true));
        assert_eq!(blocked["summary"], json!("runtime/tunnel not running"));
    }

    #[tokio::test]
    async fn runtime_observation_blocks_stale_ephemeral_tunnel_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        crate::platform_setup::persist_tunnel_artifact(
            temp.path(),
            &crate::platform_setup::TunnelAnswers {
                mode: Some("off".to_string()),
            },
        )
        .expect("persist tunnel off");
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["runtime_activity"]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": "https://old.trycloudflare.com"
            }),
        );
        stored.insert("last_activity".to_string(), json!({"ok": true}));
        let action = json!({
            "id": "runtime_activity",
            "executor": {
                "kind": "runtime_observation",
                "state_store_key": "last_activity"
            }
        });

        let result = super::setup_backend_execute_runtime_observation(
            &state,
            &contract,
            "demo",
            &mut stored,
            &action,
        )
        .await
        .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["next"], "runtime/tunnel not running");
        assert_eq!(result["result"]["error"], "runtime/tunnel not running");
        assert_eq!(
            stored
                .get("config")
                .and_then(|config| config.get("public_base_url")),
            Some(&json!("https://old.trycloudflare.com"))
        );
    }

    #[test]
    fn setup_backend_state_refreshes_runtime_observation_from_logs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let logs = temp.path().join("logs");
        std::fs::create_dir_all(&logs).expect("logs dir");
        std::fs::write(
            logs.join("system.log"),
            r#"2026-06-19T12:00:00Z INFO [fast2flow:gate] enter tenant=demo team=Some("support")"#,
        )
        .expect("system log");
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-teams".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["first_bot_framework_post"],
                "actions": [{
                    "id": "first_bot_framework_post",
                    "executor": {
                        "kind": "runtime_observation",
                        "source": "greentic-start",
                        "event": "bot_framework_activity_received",
                        "state_store_key": "last_activity"
                    },
                    "completion": {
                        "state_path": "last_activity",
                        "exists": true
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": "https://runtime.example.com"
            }),
        );
        super::save_setup_backend_contract_state(&state, &contract.provider_id, "demo", &stored)
            .expect("save state");

        let rendered =
            super::setup_backend_contract_state(&state, &contract, "demo").expect("render state");

        assert_eq!(rendered["setup_status"]["ok"], true);
        assert_eq!(rendered["setup_status"]["items"][0]["state"], "done");
        assert_eq!(
            rendered["values"]["last_activity"]["event"],
            "bot_framework_activity_received"
        );
        assert!(rendered["values"]["last_activity_received_at"].is_number());
    }

    #[test]
    fn completed_provider_http_step_is_pending_when_runtime_context_is_stale() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-teams".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": [
                    "bot_framework_endpoint_registration",
                    "first_bot_framework_post"
                ],
                "actions": [
                    {
                        "id": "bot_framework_endpoint_registration",
                        "executor": {
                            "kind": "provider_http",
                            "state_store_key": "last_reconcile"
                        },
                        "completion": {
                            "state_path": "last_reconcile.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "first_bot_framework_post",
                        "executor": {
                            "kind": "runtime_observation",
                            "state_store_key": "last_activity"
                        },
                        "completion": {
                            "state_path": "last_activity",
                            "exists": true
                        }
                    }
                ]
            }),
            load_error: None,
        };
        let stored = JsonMap::from_iter([
            (
                "config".to_string(),
                json!({
                    "tenant": "demo",
                    "team": "support",
                    "public_base_url": "https://old.trycloudflare.com"
                }),
            ),
            (
                "completed_steps".to_string(),
                json!([
                    "bot_framework_endpoint_registration",
                    "first_bot_framework_post"
                ]),
            ),
            (
                "last_reconcile".to_string(),
                json!({
                    "ok": true,
                    "runtime_context": {
                        "public_base_url": "https://old.trycloudflare.com",
                        "public_base_url_is_ephemeral_tunnel": true,
                        "active_tunnel_public_base_url": "https://old.trycloudflare.com"
                    }
                }),
            ),
            ("last_activity".to_string(), json!({"ok": true})),
        ]);

        let next = super::setup_backend_first_pending_step(&state, &contract, "demo", &stored);

        assert_eq!(next, "bot_framework_endpoint_registration");
    }

    #[test]
    fn teams_publish_and_install_stay_done_when_only_tunnel_context_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-teams".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": [
                    "bot_framework_endpoint_registration",
                    "teams_app_publish",
                    "teams_app_user_install",
                    "first_bot_framework_post"
                ],
                "actions": [
                    {
                        "id": "bot_framework_endpoint_registration",
                        "executor": {
                            "kind": "provider_http",
                            "state_store_key": "last_reconcile"
                        },
                        "completion": {
                            "state_path": "last_reconcile.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "teams_app_publish",
                        "executor": {
                            "kind": "provider_http",
                            "state_store_key": "last_teams_app_publish"
                        },
                        "completion": {
                            "state_path": "last_teams_app_publish.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "teams_app_user_install",
                        "executor": {
                            "kind": "provider_http",
                            "state_store_key": "last_teams_app_install"
                        },
                        "completion": {
                            "state_path": "last_teams_app_install.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "first_bot_framework_post",
                        "executor": {
                            "kind": "runtime_observation",
                            "state_store_key": "last_activity"
                        },
                        "completion": {
                            "state_path": "last_activity",
                            "exists": true
                        }
                    }
                ]
            }),
            load_error: None,
        };
        let stored = JsonMap::from_iter([
            (
                "config".to_string(),
                json!({
                    "tenant": "demo",
                    "team": "default",
                    "public_base_url": "https://new.example.com"
                }),
            ),
            (
                "last_reconcile".to_string(),
                json!({
                    "ok": true,
                    "runtime_context": {
                        "public_base_url": "https://new.example.com"
                    }
                }),
            ),
            (
                "last_teams_app_publish".to_string(),
                json!({
                    "ok": true,
                    "response": {
                        "add_to_teams_url": "https://teams.microsoft.com/l/app/app-id"
                    },
                    "runtime_context": {
                        "public_base_url": "https://old.example.com"
                    }
                }),
            ),
            (
                "last_teams_app_install".to_string(),
                json!({
                    "ok": true,
                    "response": {
                        "action": "exists",
                        "open_bot_chat_url": "https://teams.microsoft.com/l/chat/0/0"
                    },
                    "runtime_context": {
                        "public_base_url": "https://old.example.com"
                    }
                }),
            ),
        ]);

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored.clone());
        let next = super::setup_backend_first_pending_step(&state, &contract, "demo", &stored);

        assert_eq!(next, "first_bot_framework_post");
        assert_eq!(rendered["values"]["last_teams_app_publish"]["ok"], true);
        assert_eq!(rendered["values"]["last_teams_app_install"]["ok"], true);
        assert_eq!(
            rendered["values"]["last_teams_app_install"]["stale"],
            Value::Null
        );
        assert_eq!(
            rendered["teams_app"]["open_bot_chat_url"],
            "https://teams.microsoft.com/l/chat/0/0"
        );
    }

    #[test]
    fn teams_publish_and_install_render_done_even_when_endpoint_must_be_refreshed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-teams".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": [
                    "bot_framework_endpoint_registration",
                    "teams_app_publish",
                    "teams_app_user_install",
                    "first_bot_framework_post"
                ],
                "actions": [
                    {
                        "id": "bot_framework_endpoint_registration",
                        "executor": {
                            "kind": "provider_http",
                            "state_store_key": "last_reconcile"
                        },
                        "completion": {
                            "state_path": "last_reconcile.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "teams_app_publish",
                        "executor": {
                            "kind": "provider_http",
                            "state_store_key": "last_teams_app_publish"
                        },
                        "completion": {
                            "state_path": "last_teams_app_publish.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "teams_app_user_install",
                        "executor": {
                            "kind": "provider_http",
                            "state_store_key": "last_teams_app_install"
                        },
                        "completion": {
                            "state_path": "last_teams_app_install.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "first_bot_framework_post",
                        "executor": {
                            "kind": "runtime_observation",
                            "state_store_key": "last_activity"
                        },
                        "completion": {
                            "state_path": "last_activity",
                            "exists": true
                        }
                    }
                ]
            }),
            load_error: None,
        };
        let stored = JsonMap::from_iter([
            (
                "config".to_string(),
                json!({
                    "tenant": "demo",
                    "team": "default",
                    "public_base_url": "https://new.example.com"
                }),
            ),
            (
                "last_reconcile".to_string(),
                json!({
                    "ok": true,
                    "runtime_context": {
                        "public_base_url": "https://old.example.com"
                    }
                }),
            ),
            (
                "last_teams_app_publish".to_string(),
                json!({
                    "ok": true,
                    "response": {
                        "add_to_teams_url": "https://teams.microsoft.com/l/app/app-id"
                    }
                }),
            ),
            (
                "last_teams_app_install".to_string(),
                json!({
                    "ok": true,
                    "response": {
                        "open_bot_chat_url": "https://teams.microsoft.com/l/chat/0/0"
                    }
                }),
            ),
        ]);

        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);
        let items = rendered["setup_status"]["items"].as_array().unwrap();

        assert_eq!(items[0]["state"], "pending");
        assert_eq!(items[1]["state"], "done");
        assert_eq!(items[2]["state"], "done");
        assert_eq!(items[3]["state"], "pending");
    }

    #[tokio::test]
    async fn provider_http_executor_url_template_calls_external_endpoint() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test provider");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let app = axum::Router::new().route(
            "/v1/setup/register",
            axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
                axum::Json(json!({
                    "ok": true,
                    "received": body,
                }))
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test provider server");
        });

        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["register_endpoint"],
                "actions": [{
                    "id": "register_endpoint",
                    "completion": {
                        "state_path": "last_reconcile.ok",
                        "equals": true
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "bot_app_id": "app-id",
                "bot_app_password": "app-password",
                "public_base_url": "https://runtime.example.com",
                "provider_setup_base_url": base_url
            }),
        );
        let action = json!({
            "id": "register_endpoint",
            "executor": {
                "kind": "provider_http",
                "url_template": "{provider_setup_base_url}/v1/setup/register",
                "body": {
                    "provider_id": "messaging-example",
                    "bot_app_id": "{bot_app_id}",
                    "bot_app_password": "{bot_app_password}",
                    "messaging_endpoint": "{public_base_url}/v1/messaging/ingress/{tenant}/{team}",
                    "tenant": "{tenant}",
                    "team": "{team}"
                },
                "state_store_key": "last_reconcile"
            }
        });

        let result = super::setup_backend_execute_provider_http(
            &state,
            &contract,
            "demo",
            &mut stored,
            &action,
        )
        .await
        .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(stored["last_reconcile"]["ok"], true);
        assert_eq!(
            stored["last_reconcile"]["response"]["body"]["received"]["messaging_endpoint"],
            "https://runtime.example.com/v1/messaging/ingress/demo/support"
        );
        assert_eq!(
            stored["last_reconcile"]["response"]["body"]["received"]["bot_app_id"],
            "app-id"
        );

        stored.insert("last_setup_result".to_string(), result);
        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);
        assert_eq!(rendered["setup_status"]["ok"], true);
        server.abort();
    }

    #[tokio::test]
    async fn provider_http_executor_does_not_start_runtime_for_later_observation() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test provider");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let app = axum::Router::new().route(
            "/v1/setup/register",
            axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
                axum::Json(json!({
                    "ok": true,
                    "received": body,
                }))
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test provider server");
        });

        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            temp.path().join("bundle.yaml"),
            "schema_version: 1\nbundle_id: runtime-startable\n",
        )
        .expect("bundle yaml");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["register_endpoint", "runtime_activity"],
                "actions": [
                    {
                        "id": "register_endpoint",
                        "executor": {
                            "kind": "provider_http",
                            "url_template": "{provider_setup_base_url}/v1/setup/register",
                            "body": {
                                "messaging_endpoint": "{public_base_url}/v1/messaging/ingress/{tenant}/{team}",
                                "tenant": "{tenant}",
                                "team": "{team}"
                            },
                            "state_store_key": "last_reconcile"
                        },
                        "completion": {
                            "state_path": "last_reconcile.ok",
                            "equals": true
                        }
                    },
                    {
                        "id": "runtime_activity",
                        "executor": {
                            "kind": "runtime_observation",
                            "state_store_key": "last_activity"
                        },
                        "completion": {
                            "state_path": "last_activity",
                            "exists": true
                        }
                    }
                ]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "public_base_url": "https://runtime.example.com",
                "provider_setup_base_url": base_url
            }),
        );
        let action = super::setup_backend_action_by_id(&contract, "register_endpoint")
            .expect("register action")
            .clone();

        let result = super::setup_backend_execute_provider_http(
            &state,
            &contract,
            "demo",
            &mut stored,
            &action,
        )
        .await
        .unwrap();

        assert_eq!(result["ok"], true);
        assert_eq!(stored["last_reconcile"]["ok"], true);
        assert_eq!(
            stored["last_reconcile"]["response"]["body"]["received"]["messaging_endpoint"],
            "https://runtime.example.com/v1/messaging/ingress/demo/support"
        );
        server.abort();
    }

    #[tokio::test]
    async fn provider_http_executor_path_template_requires_declared_pack_route() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-example".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "required_order": ["register_endpoint"],
                "actions": [{
                    "id": "register_endpoint",
                    "completion": {
                        "state_path": "last_reconcile.ok",
                        "equals": true
                    }
                }]
            }),
            load_error: None,
        };
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "tenant": "demo",
                "team": "support",
                "bot_app_id": "app-id",
                "bot_app_password": "app-password",
                "public_base_url": "https://runtime.example.com",
                "provider_setup_base_url": "http://127.0.0.1:9"
            }),
        );
        let action = json!({
            "id": "register_endpoint",
            "executor": {
                "kind": "provider_http",
                "path_template": "/v1/setup/register",
                "body": {
                    "provider_id": "messaging-example",
                    "tenant": "{tenant}",
                    "team": "{team}"
                },
                "state_store_key": "last_reconcile"
            }
        });

        let result = super::setup_backend_execute_provider_http(
            &state,
            &contract,
            "demo",
            &mut stored,
            &action,
        )
        .await
        .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(
            result["result"]["error"],
            "provider_http target /v1/setup/register is not declared by pack greentic.http-routes.v1"
        );
        assert!(stored.get("last_reconcile").is_none());
    }

    #[tokio::test]
    async fn graph_application_executor_uses_shared_retryable_missing_token_result() {
        let contract = super::ProviderBackendContract {
            provider_id: "messaging-teams".to_string(),
            inline: json!({
                "schema_id": "greentic.setup.backend-contract.v1",
                "provider_id": "messaging-teams",
            }),
            load_error: None,
        };
        let action = json!({
            "id": "register_app",
            "executor": {
                "kind": "microsoft_graph_application",
                "graph_token_store_key": "graph_access_token",
                "app_id_config_key": "bot_app_id",
                "client_secret_config_key": "bot_client_secret",
                "display_name_config_key": "bot_display_name"
            }
        });
        let mut stored = JsonMap::new();
        stored.insert(
            "config".to_string(),
            json!({
                "bot_display_name": "Demo Bot"
            }),
        );

        let result =
            super::setup_backend_execute_graph_application(&contract, "demo", &mut stored, &action)
                .await
                .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(
            result["result"]["missing_token_store_key"],
            "graph_access_token"
        );
        assert_eq!(result["result"]["blocked"], true);
        assert_eq!(result["result"]["retryable"], true);
        assert!(stored.get("last_app_registration").is_none());
    }

    #[test]
    fn declared_provider_http_route_matches_tenant_team_and_wildcard() {
        let route = super::DeclaredProviderHttpRoute {
            provider_id: "messaging-teams".to_string(),
            pack_path: std::path::PathBuf::from("messaging-teams.gtpack"),
            methods: vec!["POST".to_string()],
            target: super::ProviderHttpRouteTarget::SetupComponent {
                component_ref: "messaging-teams-setup".to_string(),
                op: "handle_http".to_string(),
            },
            segments: super::parse_provider_http_route_pattern(
                "/v1/messaging/setup/messaging-teams/{tenant}/{team}/{action*}",
            ),
        };
        let request_segments = vec![
            "v1",
            "messaging",
            "setup",
            "messaging-teams",
            "demo",
            "support",
            "publish",
        ];

        assert_eq!(
            super::match_provider_http_route(&route, &request_segments, "fallback", "default"),
            Some(("demo".to_string(), "support".to_string()))
        );
        assert_eq!(
            super::match_provider_http_route(
                &route,
                &["v1", "messaging", "setup", "other", "demo"],
                "fallback",
                "default",
            ),
            None
        );
    }

    #[test]
    fn provider_setup_event_redaction_removes_tokens_and_hashes_user_code() {
        let redacted = redact_provider_setup_event_detail(&json!({
            "state": {
                "access_token": "access-secret",
                "refresh-token": "refresh-secret",
                "idToken": "id-secret",
                "client_secret": "client-secret",
                "bot_app_password": "bot-password",
                "device_code": "device-secret",
                "oauth_device_code": "oauth-device-secret",
                "user_code": "ABCD-EFGH",
                "step": "wait_for_graph_login",
                "next": "continue",
                "status": 200,
                "error_codes": [1, 2],
                "trace_id": "trace-1",
                "request-id": "request-1"
            }
        }));

        let state = &redacted["state"];
        assert_eq!(state["access_token"], "[redacted]");
        assert_eq!(state["refresh-token"], "[redacted]");
        assert_eq!(state["idToken"], "[redacted]");
        assert_eq!(state["client_secret"], "[redacted]");
        assert_eq!(state["bot_app_password"], "[redacted]");
        assert_eq!(state["device_code"], "[redacted]");
        assert_eq!(state["oauth_device_code"], "[redacted]");
        assert!(state["user_code"].as_str().unwrap().starts_with("[sha256:"));
        assert_eq!(state["step"], "wait_for_graph_login");
        assert_eq!(state["next"], "continue");
        assert_eq!(state["status"], 200);
        assert_eq!(state["trace_id"], "trace-1");
        assert_eq!(state["request-id"], "request-1");
    }

    #[tokio::test]
    async fn setup_backend_contract_is_exposed_and_handles_state_without_runtime_proxy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_setup_backend_contract(
            &providers.join("messaging-contract.gtpack"),
            "messaging-contract",
        )
        .expect("pack");

        let state = test_ui_state(temp.path());
        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("providers response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let provider = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["provider_id"] == "messaging-contract")
            .expect("provider");
        assert_eq!(
            provider["setup_backend_contract"]["schema_id"],
            "greentic.setup.backend-contract.v1"
        );

        let state = test_ui_state(temp.path());
        let app = build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messaging/setup/messaging-contract/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("state response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        if body["ok"] != true {
            panic!("{body}");
        }
        assert_eq!(body["setup_status"]["ok"], false);
        assert_eq!(body["setup_status"]["items"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn setup_machine_is_exposed_and_advances_through_ui_route() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_setup_machine(
            &providers.join("messaging-machine.gtpack"),
            "messaging-machine",
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("providers response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let provider = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["provider_id"] == "messaging-machine")
            .expect("provider");
        assert_eq!(
            provider["setup_machine"]["schema_id"],
            "greentic.setup.machine.v1"
        );
        assert_eq!(provider["setup_machine"]["id"], "ui-machine");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messaging/setup/messaging-machine/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("state response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["setup_status"]["ok"], false);
        assert_eq!(body["setup_status"]["items"][0]["id"], "start");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messaging/setup/messaging-machine/demo/next")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .expect("next response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["setup_status"]["ok"], true);
        assert_eq!(body["setup_status"]["last_step"], "complete");
        let state = crate::setup_machine::load_setup_machine_state(
            &crate::setup_machine::setup_machine_state_path(
                temp.path(),
                "demo",
                "support",
                "messaging-machine",
            ),
        )
        .expect("machine state");
        assert_eq!(
            state.status,
            crate::setup_machine::SetupMachineStatus::Complete
        );
    }

    #[tokio::test]
    async fn setup_actions_pack_extension_is_exposed_for_setup_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_final_setup_actions(
            &providers.join("messaging-actions.gtpack"),
            "messaging-actions",
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("providers response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let provider = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["provider_id"] == "messaging-actions")
            .expect("provider");

        assert_eq!(
            provider["setup_actions"]["schema_id"],
            "greentic.setup.actions.v1"
        );
        assert_eq!(
            provider["setup_actions"]["actions"][0]["url_template"],
            "{add_url}"
        );
    }

    #[tokio::test]
    async fn legacy_setup_actions_are_exposed_for_setup_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_legacy_setup_actions(
            &providers.join("messaging-legacy-actions.gtpack"),
            "messaging-legacy-actions",
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("providers response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let provider = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["provider_id"] == "messaging-legacy-actions")
            .expect("provider");

        assert_eq!(
            provider["setup_actions"]["schema_id"],
            "greentic.setup.actions.v1"
        );
        assert_eq!(
            provider["setup_actions"]["actions"][0]["kind"],
            "oauth_install_button"
        );
        assert_eq!(
            provider["setup_actions"]["actions"][0]["provider_id"],
            "messaging-legacy-actions"
        );
        assert_eq!(
            provider["setup_actions"]["actions"][0]["authorize_url"],
            "https://provider.example/install"
        );
    }

    #[tokio::test]
    async fn setup_actions_extension_and_legacy_actions_are_both_exposed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_final_and_legacy_setup_actions(
            &providers.join("messaging-combined-actions.gtpack"),
            "messaging-combined-actions",
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("providers response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let provider = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["provider_id"] == "messaging-combined-actions")
            .expect("provider");
        let actions = provider["setup_actions"]["actions"].as_array().unwrap();

        assert!(actions.iter().any(|action| {
            action["kind"] == "oauth_install_button"
                && action["authorize_url"] == "https://provider.example/install"
        }));
        assert!(actions.iter().any(|action| {
            action["kind"] == "deep_link" && action["url_template"] == "{add_url}"
        }));
    }

    #[tokio::test]
    async fn setup_action_endpoint_runs_registration_and_returns_final_url_value() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_final_and_legacy_setup_actions(
            &providers.join("messaging-combined-actions.gtpack"),
            "messaging-combined-actions",
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/setup-action")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "provider_id": "messaging-combined-actions",
                            "action_id": "create_app",
                            "tenant": "demo",
                            "team": "support",
                            "env": "dev",
                            "tunnel": "off",
                            "answers": {
                                "messaging-combined-actions": {
                                    "public_base_url": "https://runtime.example.test"
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("setup action response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();

        assert!(body["ok"] == true, "{body}");
        let add_url = body["values"]["add_url"].as_str().expect("add url");
        assert!(add_url.starts_with("https://provider.example/install?"));
        assert!(add_url.contains("client_id=generated-client"));
        assert!(!add_url.contains("client_id=stale-client"));
        assert!(add_url.contains("scope=chat%3Awrite"));
        assert!(add_url.contains(
            "redirect_uri=https%3A%2F%2Fruntime.example.test%2Foauth%2Fcallback%2Fprovider"
        ));
        assert!(!add_url.contains("old.example.test"));
        assert!(add_url.contains("state="));
        assert!(body["values"].get("provider_client_secret").is_none());
        assert_eq!(
            body["output"]["provider_client_secret"],
            Value::String("[redacted]".to_string())
        );
        let action = crate::setup_actions::load_setup_action(
            temp.path(),
            "demo",
            "support",
            "messaging-combined-actions",
            "create_app",
        )
        .unwrap()
        .expect("persisted setup action");
        assert_eq!(
            action.status,
            crate::setup_actions::SetupActionStatus::Pending
        );
        assert!(action.state.is_some());
        assert_eq!(action.authorize_url.as_deref(), Some(add_url));
        assert_eq!(action.provider_id, "messaging-combined-actions");
        let state = url::Url::parse(add_url)
            .unwrap()
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("signed state");
        assert_ne!(state, "provider-state");
        let key = crate::setup_actions::load_or_create_signing_key(temp.path()).unwrap();
        let payload = crate::setup_actions::validate_oauth_state(
            &state,
            &key,
            Some("messaging-combined-actions"),
            Some("demo"),
            Some("support"),
            crate::setup_actions::current_epoch_secs(),
        )
        .unwrap();
        assert_eq!(payload.action_id, "create_app");
    }

    #[tokio::test]
    async fn setup_action_endpoint_returns_oauth_url_without_deep_link_action() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_legacy_setup_actions(
            &providers.join("messaging-legacy-actions.gtpack"),
            "messaging-legacy-actions",
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/setup-action")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "provider_id": "messaging-legacy-actions",
                            "action_id": "create_app",
                            "tenant": "demo",
                            "team": "support",
                            "env": "dev",
                            "tunnel": "off",
                            "answers": {
                                "messaging-legacy-actions": {
                                    "public_base_url": "https://runtime.example.test"
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("setup action response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();

        assert!(body["ok"] == true, "{body}");
        let add_url = body["values"]["oauth_authorize_url"]
            .as_str()
            .expect("oauth authorize url");
        assert!(add_url.starts_with("https://provider.example/install?"));
        assert!(add_url.contains("client_id=generated-client"));
        assert!(!add_url.contains("client_id=stale-client"));
        assert!(add_url.contains("scope=chat%3Awrite"));
        assert!(add_url.contains(
            "redirect_uri=https%3A%2F%2Fruntime.example.test%2Foauth%2Fcallback%2Fprovider"
        ));
        assert!(!add_url.contains("old.example.test"));
        assert!(add_url.contains("state="));
        assert!(!add_url.contains("provider-state"));
        let action = crate::setup_actions::load_setup_action(
            temp.path(),
            "demo",
            "support",
            "messaging-legacy-actions",
            "create_app",
        )
        .unwrap()
        .expect("persisted setup action");
        assert_eq!(action.authorize_url.as_deref(), Some(add_url));
    }

    #[tokio::test]
    async fn draft_endpoint_persists_selected_tunnel_without_answers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let app = build_router(test_ui_state(temp.path()));

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/draft")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "tenant": "demo",
                            "team": "support",
                            "env": "dev",
                            "tunnel": "ngrok",
                            "answers": {}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("draft response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["ok"] == true, "{body}");

        let tunnel = crate::platform_setup::load_tunnel_artifact(temp.path())
            .unwrap()
            .expect("tunnel artifact");
        assert_eq!(tunnel.mode.as_deref(), Some("ngrok"));
    }

    #[tokio::test]
    async fn provider_api_does_not_return_saved_values_for_secret_questions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_secret_and_public_questions(
            &providers.join("messaging-question-provider.gtpack"),
            "messaging-question-provider",
        )
        .expect("pack");

        let store = open_dev_store(temp.path()).expect("open store");
        store
            .put(
                &crate::canonical_secret_uri(
                    "dev",
                    "demo",
                    Some("support"),
                    "messaging-question-provider",
                    "provider_public_field",
                ),
                SecretFormat::Text,
                b"public-value",
            )
            .await
            .expect("store public");
        store
            .put(
                &crate::canonical_secret_uri(
                    "dev",
                    "demo",
                    Some("support"),
                    "messaging-question-provider",
                    "provider_secret_field",
                ),
                SecretFormat::Text,
                b"secret-value",
            )
            .await
            .expect("store secret");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("providers response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let questions = body["provider_forms"]
            .as_array()
            .unwrap()
            .iter()
            .find(|form| form["provider_id"] == "messaging-question-provider")
            .expect("provider form")["questions"]
            .as_array()
            .unwrap();
        let public = questions
            .iter()
            .find(|question| question["id"] == "provider_public_field")
            .expect("public question");
        let secret = questions
            .iter()
            .find(|question| question["id"] == "provider_secret_field")
            .expect("secret question");

        assert_eq!(public["saved_value"], "public-value");
        assert!(secret.get("saved_value").is_none());
    }

    #[test]
    fn setup_ui_resolves_final_actions_even_when_setup_result_failed() {
        let app_js = include_str!("../../assets/setup-ui/app.js");
        assert!(app_js.contains("var finalActions = resolveFinalSetupActions();"));
        assert!(!app_js.contains("var finalActions = ok ? resolveFinalSetupActions() : [];"));
    }

    #[test]
    fn setup_ui_resolves_final_actions_from_provider_answers() {
        let app_js = include_str!("../../assets/setup-ui/app.js");
        assert!(
            app_js.contains(
                "mergeFinalSetupActionContext(context, scope && scope.answers && scope.answers[provider.provider_id]);"
            ),
            "final setup actions must see provider form answers such as bot_username and bot_email"
        );
    }

    #[test]
    fn setup_ui_opens_returned_install_url_after_provider_setup_action() {
        let app_js = include_str!("../../assets/setup-ui/app.js");
        assert!(app_js.contains("var popup = openProviderInstallWindow();"));
        assert!(app_js.contains("var installUrl = setupActionReturnedFinalUrl(provider, result);"));
        assert!(app_js.contains("navigateProviderInstallWindow(popup, installUrl);"));
    }

    #[test]
    fn setup_ui_provider_setup_action_back_and_continue_do_not_trap_admin() {
        let app_js = include_str!("../../assets/setup-ui/app.js");
        assert!(app_js.contains("var canContinue = completed || !!error;"));
        assert!(app_js.contains("providerHasFormQuestions(p)"));
        assert!(app_js.contains("state.phase = \"providers\";"));
    }

    #[test]
    fn setup_ui_serializes_setup_tunnel_start_without_retrying_new_tunnels() {
        let ui_rs = include_str!("mod.rs");
        assert!(ui_rs.contains("setup_tunnel_start: AsyncMutex<()>"));
        assert!(ui_rs.contains("let _start_guard = state.setup_tunnel_start.lock().await;"));
        assert!(ui_rs.contains("Duration::from_secs(45)"));
    }

    #[test]
    fn setup_ui_persists_selected_tunnel_before_provider_steps() {
        let app_js = include_str!("../../assets/setup-ui/app.js");
        assert!(app_js.contains("state.defaultTunnel = scopeData.tunnel ||"));
        assert!(app_js.contains("if (scope.tunnel) payload.tunnel = scope.tunnel;"));
        assert!(app_js.contains("Object.keys(payload.answers).length === 0 && !payload.tunnel"));
        assert!(app_js.contains(
            "persistDraftNow().finally(function () {\n        state.phase = \"providers\";"
        ));
    }

    #[test]
    fn setup_ui_web_component_continue_is_not_blocked_by_partial_or_failed_setup() {
        let app_js = include_str!("../../assets/setup-ui/app.js");
        assert!(app_js.contains("function markCanContinue(detail)"));
        assert!(app_js.contains("markCanContinue(detail);"));
        assert!(app_js.contains("if (submit) submit.disabled = false;"));
        assert!(app_js.contains("complete: existing.complete === true"));
        assert!(app_js.contains("continued: true"));
        assert!(app_js.contains("return !detailProviderId || detailProviderId === providerId;"));
    }

    #[test]
    fn setup_runtime_start_is_serialized() {
        let ui_rs = include_str!("mod.rs");
        assert!(ui_rs.contains("setup_runtime_start: AsyncMutex<()>"));
        assert!(ui_rs.contains("let _start_guard = state.setup_runtime_start.lock().await;"));
    }

    #[test]
    fn setup_ui_prefills_required_public_base_url_from_selected_tunnel() {
        let app_js = include_str!("../../assets/setup-ui/app.js");
        assert!(app_js.contains("providerNeedsGeneratedPublicBaseUrl(scope, p, form)"));
        assert!(app_js.contains("fetch(\"/api/setup-public-url\""));
        assert!(app_js.contains("store.public_base_url = result.public_base_url;"));
        assert!(app_js.contains("q.id === \"public_base_url\" && q.required === true"));
    }

    #[test]
    fn setup_public_url_endpoint_reuses_setup_tunnel_lifecycle() {
        let ui_rs = include_str!("mod.rs");
        assert!(ui_rs.contains(".route(\"/api/setup-public-url\", post(post_setup_public_url))"));
        assert!(ui_rs.contains("async fn ensure_setup_public_url"));
        assert!(ui_rs.contains("ensure_setup_tunnel(state, &mode, &state.local_base_url).await"));
    }

    #[tokio::test]
    async fn setup_backend_contract_descriptor_loads_asset_contract() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_asset_setup_backend_contract(
            &providers.join("messaging-teams.gtpack"),
            "messaging-teams",
            true,
        )
        .expect("pack");

        let state = test_ui_state(temp.path());
        let app = build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("providers response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let provider = body["providers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|provider| provider["provider_id"] == "messaging-teams")
            .expect("provider");
        assert_eq!(
            provider["setup_backend_contract"]["descriptor"]["asset"],
            "assets/setup/backend-contract.json"
        );
        assert_eq!(
            provider["setup_backend_contract"]["required_order"][0],
            "graph_admin_consent"
        );

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messaging/setup/messaging-teams/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("state response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let items = body["setup_status"]["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert_eq!(items[0]["id"], "graph_admin_consent");
        assert_eq!(items[0]["state"], "pending");
        assert_eq!(body["setup_status"]["ok"], false);
    }

    #[tokio::test]
    async fn setup_backend_contract_missing_asset_is_blocked_not_complete() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_asset_setup_backend_contract(
            &providers.join("messaging-teams.gtpack"),
            "messaging-teams",
            false,
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/v1/messaging/setup/messaging-teams/demo")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("state response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["setup_status"]["ok"], false);
        assert_eq!(body["setup_status"]["items"].as_array().unwrap().len(), 0);
        assert_eq!(
            body["setup_status"]["blocked"]["title"],
            "Setup backend contract could not be loaded"
        );
        assert_ne!(body["setup_status"]["last_step"], "complete");
        assert_ne!(body["setup_status"]["next"], "Setup complete.");
    }

    #[tokio::test]
    async fn setup_backend_next_names_unsupported_executor_kind() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_unsupported_setup_action(
            &providers.join("messaging-unsupported.gtpack"),
            "messaging-unsupported",
        )
        .expect("pack");

        let state = test_ui_state(temp.path());
        let app = build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messaging/setup/messaging-unsupported/demo/next")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .expect("next response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let result = &body["values"]["last_setup_result"];
        assert_eq!(result["ok"], false);
        assert_eq!(result["result"]["executor_kind"], "future_executor");
        assert_eq!(
            result["next"],
            "setup backend executor kind is not implemented: future_executor"
        );
        let events = read_provider_setup_events(
            &state,
            &super::ProviderSetupEventsQuery {
                provider_id: "messaging-unsupported".to_string(),
                tenant: Some("demo".to_string()),
                team: Some("support".to_string()),
                env: Some("dev".to_string()),
                limit: Some(10),
            },
        )
        .expect("diagnostic events");
        let diagnostic = events
            .iter()
            .find(|event| event["event_name"] == "greentic-provider-setup-backend-next")
            .expect("backend next diagnostic");
        assert_eq!(diagnostic["current_step_id"], "custom_step");
        assert_eq!(
            diagnostic["event_detail"]["selected_executor"]["kind"],
            "future_executor"
        );
        assert_eq!(
            diagnostic["event_detail"]["request"]["path"],
            "/v1/messaging/setup/messaging-unsupported/demo/next"
        );
    }

    #[tokio::test]
    async fn setup_backend_contract_config_ignores_server_owned_browser_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let providers = temp.path().join("providers/messaging");
        std::fs::create_dir_all(&providers).expect("providers");
        write_pack_with_setup_backend_contract(
            &providers.join("messaging-contract.gtpack"),
            "messaging-contract",
        )
        .expect("pack");

        let app = build_router(test_ui_state(temp.path()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messaging/setup/messaging-contract/demo/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "config": {
                                "safe_key": "safe",
                                "oauth_device_code": "browser-device-code",
                                "graph_access_token": "browser-token",
                                "bot_access_token": "browser-bot-token"
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .expect("config response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let config = &body["values"]["config"];
        assert_eq!(config["safe_key"], "safe");
        assert!(config.get("oauth_device_code").is_none());
        assert!(config.get("graph_access_token").is_none());
        assert!(config.get("bot_access_token").is_none());
    }

    #[tokio::test]
    async fn provider_setup_event_route_writes_and_reads_jsonl() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let app = build_router(state.clone());
        let request = Request::builder()
            .method("POST")
            .uri("/api/provider-setup-events")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "provider_id": "messaging-test",
                    "event_name": "greentic-provider-setup-result",
                    "event_detail": {
                        "providerId": "messaging-test",
                        "access_token": "secret",
                        "step": "publish"
                    },
                    "tenant": "demo",
                    "team": "support",
                    "env": "dev",
                    "setup_session_id": "browser-session",
                    "setup_ui_url": "http://127.0.0.1:9999"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        let log_path = temp
            .path()
            .join("state/logs/setup/dev/demo/support/messaging-test.jsonl");
        let log = std::fs::read_to_string(&log_path).expect("log");
        assert!(log.contains(r#""event_name":"greentic-provider-setup-result""#));
        assert!(log.contains(r#""access_token":"[redacted]""#));
        assert!(!log.contains("secret"));

        let app = build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/provider-setup-events?provider_id=messaging-test&tenant=demo&team=support&env=dev")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["events"].as_array().unwrap().len(), 1);
        assert_eq!(body["events"][0]["event_detail"]["step"], "publish");
        assert_eq!(body["events"][0]["current_step_id"], "publish");
        assert_eq!(body["events"][0]["http_status"], Value::Null);
    }

    #[test]
    fn provider_setup_event_helpers_persist_under_scoped_log_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());
        let record = persist_provider_setup_event(
            &state,
            ProviderSetupEventRequest {
                provider_id: "messaging-test".to_string(),
                event_name: "greentic-provider-setup-state".to_string(),
                event_detail: json!({"user_code": "CODE-1234", "status": 200}),
                current_step_id: None,
                current_progress: None,
                action_name: None,
                request_method: None,
                request_path: None,
                http_status: None,
                response_body: None,
                error: None,
                correlation_id: None,
                tenant: None,
                team: None,
                env: None,
                setup_session_id: None,
                setup_ui_url: None,
            },
        )
        .expect("persist");
        assert_eq!(record["tenant"], "demo");
        assert_eq!(record["team"], "support");
        assert_eq!(record["env"], "dev");
        assert_eq!(record["http_status"], 200);

        let events = read_provider_setup_events(
            &state,
            &super::ProviderSetupEventsQuery {
                provider_id: "messaging-test".to_string(),
                tenant: None,
                team: None,
                env: None,
                limit: None,
            },
        )
        .expect("read");
        assert_eq!(events.len(), 1);
        assert!(
            events[0]["event_detail"]["user_code"]
                .as_str()
                .unwrap()
                .starts_with("[sha256:")
        );
    }

    #[test]
    fn setup_ui_asset_forwards_generic_provider_setup_events() {
        let js = super::assets::APP_JS;
        assert!(js.contains("PROVIDER_SETUP_EVENT_NAMES"));
        assert!(js.contains("greentic-provider-setup-action-start"));
        assert!(js.contains("greentic-provider-setup-complete"));
        assert!(js.contains("postProviderSetupEvent"));
        assert!(js.contains("/api/provider-setup-events"));
        assert!(js.contains("sanitizeProviderSetupEventDetail"));
        assert!(js.contains("__greenticSetupTestHooks"));
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

    #[test]
    fn detects_cloud_deploy_targets_in_prefill_answers() {
        let cloud_prefill = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "platform_setup": {
                "deployment_targets": [
                    { "target": "runtime" },
                    { "target": "aws" }
                ]
            }
        }))
        .expect("cloud prefill");
        assert!(prefill_has_cloud_deployment_targets(Some(&cloud_prefill)));

        let local_prefill = serde_json::from_value::<JsonMap<String, Value>>(json!({
            "platform_setup": {
                "deployment_targets": [
                    { "target": "runtime" },
                    { "target": "single-vm" }
                ]
            }
        }))
        .expect("local prefill");
        assert!(!prefill_has_cloud_deployment_targets(Some(&local_prefill)));
    }
}
