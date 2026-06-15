//! Web-based setup UI server.
//!
//! Launches an Axum HTTP server on a random port, opens the browser, and serves
//! a single-page app that drives the setup wizard through the same FormSpec
//! infrastructure as the terminal wizard.

mod assets;

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value};
use tokio::sync::broadcast;
use url::Url;

use crate::cli_i18n::CliI18n;
use crate::engine::{SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::platform_setup::StaticRoutesPolicy;
use crate::qa::wizard;
use crate::setup_tunnel::{
    SetupTunnel, inject_setup_public_base_url, should_start_setup_tunnel, start_setup_tunnel,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_web_component: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    setup_backend_contract: Option<Value>,
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
}

#[derive(Serialize)]
struct ScopeResponse {
    tenant: String,
    team: Option<String>,
    env: String,
    detected_tenant: Option<String>,
    cloud_deploy: bool,
}

#[derive(Serialize, Clone)]
struct ExecutionResult {
    success: bool,
    stdout: String,
    stderr: String,
    manual_steps: Vec<crate::webhook::ProviderInstruction>,
    #[serde(default)]
    pending_setup_actions: Vec<crate::setup_actions::SetupAction>,
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
        .route("/api/execute", post(post_execute))
        .route("/api/oauth-device/start", post(post_oauth_device_start))
        .route("/api/oauth-device/poll", post(post_oauth_device_poll))
        .route("/api/export", post(post_export))
        .route("/api/decrypt", post(post_decrypt))
        .route("/oauth/callback/{provider}", get(get_oauth_callback))
        .route("/v1/web/{*asset_path}", get(get_declared_static_asset))
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

    Json(ScopeResponse {
        tenant: effective_tenant,
        team: state.team.clone(),
        env: cli_env.clone(),
        detected_tenant,
        cloud_deploy,
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
            ProviderInfo {
                provider_id: p.provider_id.clone(),
                display_name: p.display_name.clone(),
                domain: p.domain.clone(),
                question_count: form.as_ref().map(|f| f.questions.len()).unwrap_or(0),
                setup_web_component,
                setup_backend_contract,
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
                            info.saved_value = Some(val);
                        } else if let Some(val) = saved.and_then(|m| m.get(&q.id)) {
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

    let Some(runtime_base) = configured_runtime_proxy_base_url() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
            "ok": false,
            "blocked": true,
            "error": "Provider setup service is not running",
            "expected": format!("/v1/messaging/setup/{proxy_path}"),
            "configure": "Set GREENTIC_SETUP_RUNTIME_URL to the active local runtime base URL."
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

fn setup_backend_contract_state(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
) -> Result<Value> {
    let stored = load_setup_backend_contract_state(state, &contract.provider_id, tenant)?;
    Ok(render_setup_backend_contract_state(
        state, contract, tenant, stored,
    ))
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
    if let Some(incoming) = incoming.as_object() {
        let server_owned = setup_backend_server_owned_keys(contract);
        let config = stored
            .entry("config".to_string())
            .or_insert_with(|| Value::Object(default_setup_backend_config(state, tenant)))
            .as_object_mut()
            .ok_or_else(|| anyhow!("stored config is not an object"))?;
        for (key, value) in incoming {
            if server_owned.contains(key.as_str()) {
                continue;
            }
            config.insert(key.clone(), value.clone());
        }
    }
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
    let next_step = setup_backend_first_pending_step(contract, &stored);
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
    stored.insert("last_setup_result".to_string(), result.clone());
    let state_after = render_setup_backend_contract_state(state, contract, tenant, stored.clone());
    save_setup_backend_contract_state(state, &contract.provider_id, tenant, &stored)?;
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
    stored.insert("last_setup_result".to_string(), result.clone());
    save_setup_backend_contract_state(state, &contract.provider_id, tenant, &stored)?;
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
    stored.insert("last_setup_result".to_string(), result.clone());
    save_setup_backend_contract_state(state, &contract.provider_id, tenant, &stored)?;
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
        "bot_framework_registration" => {
            setup_backend_execute_bot_framework_registration(
                state, contract, tenant, stored, action,
            )
            .await
        }
        "microsoft_graph_teams_app_catalog_publish" => {
            setup_backend_execute_teams_app_publish(state, contract, tenant, stored, action).await
        }
        "microsoft_graph_teams_app_user_install" => {
            setup_backend_execute_teams_app_user_install(state, contract, tenant, stored, action)
                .await
        }
        "runtime_observation" => {
            setup_backend_execute_runtime_observation(contract, tenant, stored, action)
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
    let login = setup_backend_device_login_payload(config);
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
        "authorize in the opened browser, then wait for setup to continue",
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
    config.remove("oauth_kind");
    config.remove("oauth_client_id");
    config.remove("oauth_token_url");
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

async fn setup_backend_execute_graph_application(
    contract: &ProviderBackendContract,
    _tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    let token_key = required_executor_str(executor, "graph_token_store_key")?;
    let token = setup_backend_config_str(config, token_key);
    if token.is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            &format!("complete OAuth for {token_key}, then retry"),
            serde_json::json!({ "ok": false, "missing_token_store_key": token_key }),
        ));
    }
    let app_id_key = required_executor_str(executor, "app_id_config_key")?;
    let secret_key = required_executor_str(executor, "client_secret_config_key")?;
    let display_name_key = required_executor_str(executor, "display_name_config_key")?;
    let configured_app_id = setup_backend_config_str(config, app_id_key);
    let mut display_name = setup_backend_config_str(config, display_name_key);
    if display_name.is_empty() {
        display_name = "Greentic Bot".to_string();
    }
    let client = reqwest::Client::new();
    let select = "id,appId,displayName,signInAudience";
    let filter = if configured_app_id.is_empty() {
        format!(
            "displayName eq '{}'",
            setup_backend_odata_string(&display_name)
        )
    } else {
        format!(
            "appId eq '{}'",
            setup_backend_odata_string(&configured_app_id)
        )
    };
    let lookup_url = format!(
        "https://graph.microsoft.com/v1.0/applications?$filter={}&$select={}",
        setup_backend_url_encode(&filter),
        setup_backend_url_encode(select)
    );
    let lookup = setup_backend_json_request(
        &client,
        reqwest::Method::GET,
        &lookup_url,
        Some(&token),
        None,
    )
    .await?;
    if !lookup.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(setup_backend_step_result(
            action,
            false,
            "Microsoft Graph application lookup failed.",
            lookup,
        ));
    }
    let items = lookup
        .get("body")
        .and_then(|body| body.get("value"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (app, action_name) = if let Some(app) = items.first() {
        (
            app.clone(),
            if configured_app_id.is_empty() {
                "reuse"
            } else {
                "reuse_by_app_id"
            },
        )
    } else {
        if !configured_app_id.is_empty() {
            return Ok(setup_backend_step_result(
                action,
                false,
                "configured app id was not found in Microsoft Graph applications",
                serde_json::json!({ "ok": false, "configured_app_id": configured_app_id }),
            ));
        }
        let create = setup_backend_json_request(
            &client,
            reqwest::Method::POST,
            "https://graph.microsoft.com/v1.0/applications",
            Some(&token),
            Some(serde_json::json!({
                "displayName": display_name,
                "signInAudience": executor
                    .get("sign_in_audience")
                    .and_then(Value::as_str)
                    .unwrap_or("AzureADMultipleOrgs"),
            })),
        )
        .await?;
        if !create.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(setup_backend_step_result(
                action,
                false,
                "Microsoft Graph application create failed.",
                create,
            ));
        }
        (create.get("body").cloned().unwrap_or(Value::Null), "create")
    };
    let object_id = app.get("id").and_then(Value::as_str).unwrap_or_default();
    let app_id = app.get("appId").and_then(Value::as_str).unwrap_or_default();
    if !app_id.is_empty() {
        config.insert(app_id_key.to_string(), Value::String(app_id.to_string()));
    }
    config.insert(
        display_name_key.to_string(),
        Value::String(display_name.clone()),
    );
    let mut secret_action = "keep_existing_secret";
    if setup_backend_config_str(config, secret_key).is_empty() {
        if object_id.is_empty() {
            return Ok(setup_backend_step_result(
                action,
                false,
                "app object id missing; cannot add password",
                serde_json::json!({ "ok": false, "app": app }),
            ));
        }
        let password_display_name = executor
            .get("password_display_name")
            .and_then(Value::as_str)
            .unwrap_or("setup secret");
        let url = format!("https://graph.microsoft.com/v1.0/applications/{object_id}/addPassword");
        let secret = setup_backend_json_request(
            &client,
            reqwest::Method::POST,
            &url,
            Some(&token),
            Some(serde_json::json!({
                "passwordCredential": { "displayName": password_display_name }
            })),
        )
        .await?;
        if !secret.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(setup_backend_step_result(
                action,
                false,
                "Microsoft Graph addPassword failed.",
                secret,
            ));
        }
        if let Some(secret_text) = secret
            .get("body")
            .and_then(|body| body.get("secretText"))
            .and_then(Value::as_str)
        {
            config.insert(
                secret_key.to_string(),
                Value::String(secret_text.to_string()),
            );
            secret_action = "generated_secret";
        }
    }
    let result = serde_json::json!({
        "ok": true,
        "action": action_name,
        "secret_action": secret_action,
        "app_id": app_id,
        "bot_app_id": app_id,
        "app_object_id": object_id,
        "display_name": display_name,
        "provider_id": contract.provider_id,
    });
    stored.insert("last_app_registration".to_string(), result.clone());
    Ok(setup_backend_step_result(
        action,
        true,
        "click again to continue setup",
        result,
    ))
}

async fn setup_backend_execute_bot_framework_registration(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    setup_backend_apply_host_defaults(state, tenant, config);
    let app_id_key = required_executor_str(executor, "bot_app_id_config_key")?;
    let password_key = required_executor_str(executor, "bot_app_password_config_key")?;
    let app_id = setup_backend_config_str(config, app_id_key);
    let password = setup_backend_config_str(config, password_key);
    if app_id.is_empty() || password.is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            &format!("{app_id_key} and {password_key} are required"),
            serde_json::json!({ "ok": false, "missing_config_keys": [app_id_key, password_key] }),
        ));
    }
    let endpoint_template = required_executor_str(executor, "messaging_endpoint_template")?;
    let target = setup_backend_expand_template(state, tenant, config, endpoint_template);
    if setup_backend_template_unresolved(&target) {
        return Ok(setup_backend_missing_host_capability_result(
            action,
            "public_base_url",
            "public_base_url",
        ));
    }
    let Some(registration_url_template) = executor
        .get("registration_url_template")
        .or_else(|| executor.get("url_template"))
        .and_then(Value::as_str)
    else {
        return Ok(setup_backend_step_result(
            action,
            false,
            "bot_framework_registration executor requires registration_url_template or url_template",
            serde_json::json!({
                "ok": false,
                "missing_executor_field": "registration_url_template",
                "target_messaging_endpoint": target,
            }),
        ));
    };
    let registration_url =
        setup_backend_expand_template(state, tenant, config, registration_url_template);
    if setup_backend_template_unresolved(&registration_url) || registration_url.trim().is_empty() {
        let capability = setup_backend_executor_host_capability(executor)
            .unwrap_or("bot_framework_registration");
        return Ok(setup_backend_missing_host_capability_result(
            action,
            capability,
            "bot_framework_registration_url",
        ));
    }
    let client = reqwest::Client::new();
    let response = match setup_backend_json_request(
        &client,
        reqwest::Method::POST,
        &registration_url,
        None,
        Some(serde_json::json!({
            "bot_app_id": app_id,
            "bot_app_password": password,
            "messaging_endpoint": target,
            "channel": executor.get("channel").cloned().unwrap_or(Value::Null),
            "provider_id": contract.provider_id,
            "tenant": tenant,
            "team": state.team.clone().unwrap_or_else(|| "default".to_string()),
        })),
    )
    .await
    {
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
                    "target": registration_url,
                    "detail": err.to_string(),
                }),
            ));
        }
    };
    let ok = response.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_reconcile");
    let result = serde_json::json!({
        "ok": ok,
        "target_messaging_endpoint": target,
        "registration_url": registration_url,
        "registration": response,
    });
    stored.insert(state_key.to_string(), result.clone());
    Ok(setup_backend_step_result(
        action,
        ok,
        if ok {
            "click again to continue setup"
        } else {
            "fix Bot Framework registration endpoint and retry"
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
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    let token_key = required_executor_str(executor, "graph_token_store_key")?;
    let token = setup_backend_config_str(config, token_key);
    if token.is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            &format!("complete OAuth for {token_key}, then retry"),
            serde_json::json!({ "ok": false, "missing_token_store_key": token_key }),
        ));
    }
    let app_id_key = required_executor_str(executor, "teams_app_id_config_key")?;
    let version_key = required_executor_str(executor, "teams_app_version_config_key")?;
    let bot_app_id_key = required_executor_str(executor, "bot_app_id_config_key")?;
    if setup_backend_config_str(config, app_id_key).is_empty() {
        let fallback = setup_backend_config_str(config, bot_app_id_key);
        if fallback.is_empty() {
            return Ok(setup_backend_step_result(
                action,
                false,
                &format!("{app_id_key} or {bot_app_id_key} is required"),
                serde_json::json!({ "ok": false, "missing_config_keys": [app_id_key, bot_app_id_key] }),
            ));
        }
        config.insert(app_id_key.to_string(), Value::String(fallback));
    }
    if setup_backend_config_str(config, version_key).is_empty() {
        config.insert(version_key.to_string(), Value::String("1.0.0".to_string()));
    }
    let package = setup_backend_build_teams_app_package(state, contract, tenant, config, executor)?;
    let client = reqwest::Client::new();
    let app_id = setup_backend_config_str(config, app_id_key);
    let filter = format!("externalId eq '{}'", setup_backend_odata_string(&app_id));
    let lookup_url = format!(
        "https://graph.microsoft.com/v1.0/appCatalogs/teamsApps?$filter={}&$select={}",
        setup_backend_url_encode(&filter),
        setup_backend_url_encode("id,externalId,displayName,distributionMethod")
    );
    let lookup = setup_backend_json_request(
        &client,
        reqwest::Method::GET,
        &lookup_url,
        Some(&token),
        None,
    )
    .await?;
    if !lookup.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(setup_backend_step_result(
            action,
            false,
            "Teams app catalog lookup failed.",
            lookup,
        ));
    }
    let items = lookup
        .get("body")
        .and_then(|body| body.get("value"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let (url, action_name, catalog_app_id) = if let Some(item) = items.first() {
        let catalog_app_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
        (
            format!(
                "https://graph.microsoft.com/v1.0/appCatalogs/teamsApps/{}/appDefinitions",
                setup_backend_url_encode(catalog_app_id)
            ),
            "update",
            catalog_app_id.to_string(),
        )
    } else {
        (
            "https://graph.microsoft.com/v1.0/appCatalogs/teamsApps".to_string(),
            "publish",
            String::new(),
        )
    };
    let published =
        setup_backend_binary_request(&client, reqwest::Method::POST, &url, &token, package).await?;
    if !published
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(setup_backend_step_result(
            action,
            false,
            "Teams app catalog publish failed.",
            published,
        ));
    }
    let body_catalog_id = published
        .get("body")
        .and_then(|body| body.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let catalog_app_id = if catalog_app_id.is_empty() {
        body_catalog_id.to_string()
    } else {
        catalog_app_id
    };
    let add_to_teams_url = setup_backend_expand_executor_links(state, tenant, config, executor)
        .get("add_to_teams_url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            if catalog_app_id.is_empty() {
                String::new()
            } else {
                format!(
                    "https://teams.microsoft.com/l/app/{}?source=app-details-dialog",
                    setup_backend_url_encode(&catalog_app_id)
                )
            }
        });
    let result = serde_json::json!({
        "ok": true,
        "action": action_name,
        "teams_app_id": app_id,
        "catalog_app_id": catalog_app_id,
        "manifest_version": setup_backend_config_str(config, version_key),
        "add_to_teams_url": add_to_teams_url,
    });
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_teams_app_publish");
    stored.insert(state_key.to_string(), result.clone());
    Ok(setup_backend_step_result(
        action,
        true,
        "open the Add to Teams link, install the app, then continue",
        result,
    ))
}

async fn setup_backend_execute_teams_app_user_install(
    state: &UiState,
    _contract: &ProviderBackendContract,
    tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let config = setup_backend_config_mut(stored)?;
    let token_key = required_executor_str(executor, "graph_token_store_key")?;
    let token = setup_backend_config_str(config, token_key);
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_teams_app_install");
    let links = setup_backend_expand_executor_links(state, tenant, config, executor);
    let publish = stored
        .get("last_teams_app_publish")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let catalog_app_id = publish
        .get("catalog_app_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if catalog_app_id.is_empty() {
        return Ok(setup_backend_step_result(
            action,
            false,
            "publish the Teams app before installing it for the signed-in user",
            serde_json::json!({ "ok": false, "error": "missing_catalog_app_id", "links": links }),
        ));
    }
    if token.is_empty() {
        let result = serde_json::json!({
            "ok": true,
            "action": "manual_unverified",
            "catalog_app_id": catalog_app_id,
            "warning": format!("{} is unavailable; continuing with manual install links", token_key),
            "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
            "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
        });
        stored.insert(state_key.to_string(), result.clone());
        return Ok(setup_backend_step_result(
            action,
            true,
            "open the bot chat link and send a message",
            result,
        ));
    }
    let client = reqwest::Client::new();
    let installed = setup_backend_json_request(
        &client,
        reqwest::Method::GET,
        "https://graph.microsoft.com/v1.0/me/teamwork/installedApps?$expand=teamsApp",
        Some(&token),
        None,
    )
    .await?;
    if !installed
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let result = serde_json::json!({
            "ok": true,
            "action": "manual_unverified",
            "catalog_app_id": catalog_app_id,
            "warning": "Graph could not verify installed apps; continuing with manual install links",
            "previous": installed,
            "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
            "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
        });
        stored.insert(state_key.to_string(), result.clone());
        return Ok(setup_backend_step_result(
            action,
            true,
            "open the bot chat link and send a message",
            result,
        ));
    }
    let items = installed
        .get("body")
        .and_then(|body| body.get("value"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let existing = items.iter().find(|item| {
        item.get("teamsApp")
            .and_then(|teams_app| teams_app.get("id"))
            .and_then(Value::as_str)
            == Some(catalog_app_id.as_str())
    });
    let (action_name, installed_app_id) = if let Some(item) = existing {
        (
            "keep",
            item.get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    } else {
        let created = setup_backend_json_request(
            &client,
            reqwest::Method::POST,
            "https://graph.microsoft.com/v1.0/me/teamwork/installedApps",
            Some(&token),
            Some(serde_json::json!({
                "teamsApp@odata.bind": format!("https://graph.microsoft.com/v1.0/appCatalogs/teamsApps/{catalog_app_id}")
            })),
        )
        .await?;
        if !created.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            let result = serde_json::json!({
                "ok": true,
                "action": "manual_unverified",
                "catalog_app_id": catalog_app_id,
                "warning": "Graph could not install the app; continuing with manual install links",
                "previous": created,
                "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
                "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
            });
            stored.insert(state_key.to_string(), result.clone());
            return Ok(setup_backend_step_result(
                action,
                true,
                "open the bot chat link and send a message",
                result,
            ));
        }
        (
            "install",
            created
                .get("body")
                .and_then(|body| body.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    };
    let result = serde_json::json!({
        "ok": true,
        "action": action_name,
        "catalog_app_id": catalog_app_id,
        "installed_app_id": installed_app_id,
        "add_to_teams_url": links.get("add_to_teams_url").cloned().unwrap_or(Value::Null),
        "open_bot_chat_url": links.get("open_bot_chat_url").cloned().unwrap_or(Value::Null),
    });
    stored.insert(state_key.to_string(), result.clone());
    Ok(setup_backend_step_result(
        action,
        true,
        "open the bot chat link and send a message",
        result,
    ))
}

fn setup_backend_execute_runtime_observation(
    contract: &ProviderBackendContract,
    _tenant: &str,
    stored: &mut JsonMap<String, Value>,
    action: &Value,
) -> Result<Value> {
    let executor = setup_backend_executor(action)?;
    let state_key = executor
        .get("state_store_key")
        .and_then(Value::as_str)
        .unwrap_or("last_activity");
    if stored.get(state_key).is_some_and(|value| !value.is_null()) {
        return Ok(setup_backend_step_result(
            action,
            true,
            "runtime observation is present",
            serde_json::json!({ "ok": true, "state_store_key": state_key }),
        ));
    }
    Ok(setup_backend_step_result(
        action,
        false,
        "wait for the runtime observation, then refresh",
        serde_json::json!({
            "ok": false,
            "waiting": true,
            "provider_id": executor
                .get("provider_id")
                .and_then(Value::as_str)
                .unwrap_or(&contract.provider_id),
            "source": executor.get("source").cloned().unwrap_or(Value::Null),
            "event": executor.get("event").cloned().unwrap_or(Value::Null),
            "state_store_key": state_key,
        }),
    ))
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
    serde_json::json!({
        "ok": ok,
        "step": action.get("id").and_then(Value::as_str).unwrap_or_default(),
        "next": next,
        "result": result,
    })
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

fn setup_backend_device_login_payload(config: &JsonMap<String, Value>) -> Value {
    serde_json::json!({
        "url": setup_backend_config_str(config, "oauth_verification_uri"),
        "userCode": setup_backend_config_str(config, "oauth_user_code"),
        "user_code": setup_backend_config_str(config, "oauth_user_code"),
        "interval": 5,
        "expiresIn": 900,
    })
}

fn setup_backend_timestamp_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn setup_backend_odata_string(value: &str) -> String {
    value.replace('\'', "''")
}

fn setup_backend_url_encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
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

fn setup_backend_expand_executor_links(
    state: &UiState,
    tenant: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> Value {
    let mut links = JsonMap::new();
    if let Some(raw_links) = executor.get("links").and_then(Value::as_object) {
        for (key, value) in raw_links {
            if let Some(template) = value.as_str() {
                let output_key = key.strip_suffix("_template").unwrap_or(key).to_string();
                links.insert(
                    output_key,
                    Value::String(setup_backend_expand_template(
                        state, tenant, config, template,
                    )),
                );
            }
        }
    }
    Value::Object(links)
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

async fn setup_backend_binary_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
    bearer: &str,
    payload: Vec<u8>,
) -> Result<Value> {
    let response = client
        .request(method, url)
        .bearer_auth(bearer)
        .header("Content-Type", "application/zip")
        .body(payload)
        .send()
        .await
        .with_context(|| format!("request failed: {url}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    Ok(serde_json::json!({
        "ok": status < 400,
        "status": status,
        "body": body,
    }))
}

fn setup_backend_build_teams_app_package(
    state: &UiState,
    contract: &ProviderBackendContract,
    tenant: &str,
    config: &JsonMap<String, Value>,
    executor: &Value,
) -> Result<Vec<u8>> {
    let manifest_asset = required_executor_str(executor, "manifest_template_asset")?;
    let base = executor
        .get("package_assets_base")
        .and_then(Value::as_str)
        .unwrap_or("assets/teams-app");
    if !is_safe_pack_relative_path(manifest_asset) || !is_safe_pack_relative_path(base) {
        anyhow::bail!("teams app package asset path is not safe");
    }
    let provider_pack = setup_backend_provider_pack_path(state, &contract.provider_id)?;
    let mut manifest = read_pack_json_asset(&provider_pack, manifest_asset)?;
    setup_backend_replace_json_templates(state, tenant, config, &mut manifest);

    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut out);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("manifest.json", options)?;
        let manifest_text = serde_json::to_vec_pretty(&manifest)?;
        std::io::Write::write_all(&mut writer, &manifest_text)?;
        for name in ["color.png", "outline.png"] {
            let asset = format!("{}/{}", base.trim_end_matches('/'), name);
            if let Ok(bytes) = read_pack_binary_asset(&provider_pack, &asset) {
                writer.start_file(name, options)?;
                std::io::Write::write_all(&mut writer, &bytes)?;
            }
        }
        writer.finish()?;
    }
    Ok(out.into_inner())
}

fn setup_backend_provider_pack_path(state: &UiState, provider_id: &str) -> Result<PathBuf> {
    let discovered = discovery::discover(&state.bundle_path)?;
    let provider = discovered
        .find_setup_target(provider_id)
        .ok_or_else(|| anyhow!("provider pack not found for setup backend asset"))?;
    Ok(provider.pack_path.clone())
}

fn read_pack_binary_asset(pack_path: &Path, entry_name: &str) -> Result<Vec<u8>> {
    let file = std::fs::File::open(pack_path)
        .with_context(|| format!("open provider pack {}", pack_path.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("provider pack is not a zip archive")?;
    let mut entry = archive
        .by_name(entry_name)
        .with_context(|| format!("provider pack missing {entry_name}"))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes)?;
    Ok(bytes)
}

fn setup_backend_replace_json_templates(
    state: &UiState,
    tenant: &str,
    config: &JsonMap<String, Value>,
    value: &mut Value,
) {
    match value {
        Value::String(text) => {
            *text = setup_backend_expand_template(state, tenant, config, text);
        }
        Value::Array(items) => {
            for item in items {
                setup_backend_replace_json_templates(state, tenant, config, item);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                setup_backend_replace_json_templates(state, tenant, config, value);
            }
        }
        _ => {}
    }
}

fn load_setup_backend_contract_state(
    state: &UiState,
    provider_id: &str,
    tenant: &str,
) -> Result<JsonMap<String, Value>> {
    let path = setup_backend_state_path(state, provider_id, tenant)?;
    let mut stored = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str::<JsonMap<String, Value>>(&text)
            .with_context(|| format!("parse setup backend state {}", path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => JsonMap::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    ensure_setup_backend_config_defaults(state, tenant, &mut stored)?;
    Ok(stored)
}

fn save_setup_backend_contract_state(
    state: &UiState,
    provider_id: &str,
    tenant: &str,
    stored: &JsonMap<String, Value>,
) -> Result<()> {
    let path = setup_backend_state_path(state, provider_id, tenant)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create setup backend state dir {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(stored)?)
        .with_context(|| format!("write setup backend state {}", path.display()))
}

fn setup_backend_state_path(state: &UiState, provider_id: &str, tenant: &str) -> Result<PathBuf> {
    let env = validate_log_path_segment(&state.env, "env")?;
    let tenant = validate_log_path_segment(tenant, "tenant")?;
    let team = validate_log_path_segment(state.team.as_deref().unwrap_or("default"), "team")?;
    let provider_id = validate_log_path_segment(provider_id, "provider_id")?;
    Ok(state
        .bundle_path
        .join("state")
        .join("setup-backends")
        .join(env)
        .join(tenant)
        .join(team)
        .join(format!("{provider_id}.json")))
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
    matches!(key, "public_base_url" | "bot_framework_registration_url")
}

fn default_setup_backend_config(state: &UiState, tenant: &str) -> JsonMap<String, Value> {
    default_setup_backend_config_with_runtime_base(
        state,
        tenant,
        configured_runtime_proxy_base_url().as_deref(),
    )
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
    runtime_base: Option<&str>,
    config: &mut JsonMap<String, Value>,
) {
    if setup_backend_config_str(config, "public_base_url").is_empty()
        && let Some(public_base_url) = setup_backend_public_base_url(state, tenant)
    {
        config.insert(
            "public_base_url".to_string(),
            Value::String(public_base_url),
        );
    }
    if setup_backend_config_str(config, "bot_framework_registration_url").is_empty()
        && let Some(url) =
            setup_backend_host_capability_url("bot_framework_registration", runtime_base)
    {
        config.insert(
            "bot_framework_registration_url".to_string(),
            Value::String(url),
        );
    }
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
    if let Ok(guard) = state.setup_tunnel.lock()
        && let Some(tunnel) = guard.as_ref()
    {
        return Some(tunnel.public_base_url.trim_end_matches('/').to_string());
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

fn setup_backend_host_capability_url(
    capability: &str,
    runtime_base: Option<&str>,
) -> Option<String> {
    let env_key = format!(
        "GREENTIC_SETUP_{}_URL",
        capability.to_ascii_uppercase().replace('-', "_")
    );
    if let Some(value) = std::env::var(env_key)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
    {
        return Some(value);
    }
    match capability {
        "bot_framework_registration" => runtime_base.map(|base| {
            format!(
                "{}/v1/setup/bot-framework/registration",
                base.trim_end_matches('/')
            )
        }),
        _ => None,
    }
}

fn setup_backend_executor_host_capability(executor: &Value) -> Option<&str> {
    executor
        .get("host_capability")
        .or_else(|| executor.get("capability"))
        .and_then(Value::as_str)
        .or_else(|| {
            if executor
                .get("registration_url_source")
                .and_then(Value::as_str)
                == Some("host_runtime")
            {
                Some("bot_framework_registration")
            } else {
                None
            }
        })
}

fn setup_backend_template_unresolved(value: &str) -> bool {
    value.contains('{') && value.contains('}')
}

fn setup_backend_missing_host_capability_result(
    action: &Value,
    capability: &str,
    config_key: &str,
) -> Value {
    let message = format!(
        "Setup host does not provide capability `{capability}`. Start a compatible greentic-start runtime or configure GREENTIC_SETUP_RUNTIME_URL."
    );
    setup_backend_step_result(
        action,
        false,
        &message,
        serde_json::json!({
            "ok": false,
            "blocked": true,
            "missing_host_capability": capability,
            "missing_config_key": config_key,
            "error": message,
        }),
    )
}

fn setup_backend_server_owned_keys(
    contract: &ProviderBackendContract,
) -> std::collections::HashSet<&str> {
    contract
        .inline
        .get("server_owned_config_keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
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
    let values = setup_backend_render_values(&config, &stored, setup_result.clone());
    let teams_app = setup_backend_render_teams_app(&stored);
    let items = if contract_blocked.is_some() {
        Vec::new()
    } else {
        setup_backend_contract_items(contract, &stored, &values)
    };
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
    serde_json::json!({
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
        },
    })
}

fn setup_backend_blocked_from_result(setup_result: &Value) -> Option<Value> {
    let result = setup_result.get("result")?;
    if !result
        .get("blocked")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let message = result
        .get("error")
        .or_else(|| setup_result.get("next"))
        .and_then(Value::as_str)
        .unwrap_or("Setup action is blocked.");
    let mut blocked = JsonMap::new();
    blocked.insert(
        "title".to_string(),
        Value::String("Setup action blocked".to_string()),
    );
    blocked.insert("summary".to_string(), Value::String(message.to_string()));
    blocked.insert("next".to_string(), Value::String(message.to_string()));
    if let Some(capability) = result.get("missing_host_capability").cloned() {
        blocked.insert("missing_host_capability".to_string(), capability);
    }
    if let Some(config_key) = result.get("missing_config_key").cloned() {
        blocked.insert("missing_config_key".to_string(), config_key);
    }
    Some(Value::Object(blocked))
}

fn setup_backend_contract_items(
    contract: &ProviderBackendContract,
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
    setup_backend_required_steps(contract)
        .into_iter()
        .map(|step| {
            let done = completed.contains(step)
                || setup_backend_action_by_id(contract, step)
                    .and_then(|action| action.get("completion"))
                    .is_some_and(|completion| setup_backend_completion_met(values, completion));
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
        .or_else(|| publish.and_then(|value| value.get("add_to_teams_url")))
        .cloned()
        .unwrap_or(Value::Null);
    let open_bot_chat_url = install
        .and_then(|value| value.get("open_bot_chat_url"))
        .cloned()
        .unwrap_or(Value::Null);
    serde_json::json!({
        "ok": !add_to_teams_url.is_null() || !open_bot_chat_url.is_null(),
        "add_to_teams_url": add_to_teams_url,
        "open_bot_chat_url": open_bot_chat_url,
    })
}

fn setup_backend_completion_met(values: &Value, completion: &Value) -> bool {
    let Some(path) = completion.get("state_path").and_then(Value::as_str) else {
        return false;
    };
    let Some(value) = setup_backend_value_at_path(values, path) else {
        return false;
    };
    if completion
        .get("exists")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return !value.is_null()
            && value
                .as_str()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(true);
    }
    if let Some(expected) = completion.get("equals") {
        return value == expected;
    }
    value.as_bool().unwrap_or(false)
}

fn setup_backend_value_at_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|part| !part.is_empty())
        .try_fold(root, |current, part| current.get(part))
}

fn setup_backend_required_steps(contract: &ProviderBackendContract) -> Vec<&str> {
    contract
        .inline
        .get("required_order")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
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
    contract: &ProviderBackendContract,
    stored: &JsonMap<String, Value>,
) -> String {
    if contract.load_error.is_some() || setup_backend_required_steps(contract).is_empty() {
        return "contract_blocked".to_string();
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
    setup_backend_contract_items(contract, stored, &values)
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
        match ensure_setup_tunnel(&state, &tunnel_mode).await {
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
                    pending_setup_actions: vec![],
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
        pending_setup_actions: vec![],
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

async fn post_oauth_device_start(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<crate::oauth_device::OAuthDeviceStartInput>,
) -> Json<Value> {
    match crate::oauth_device::start_oauth_device_code(
        &state.bundle_path,
        &req,
        crate::oauth_device::DEFAULT_EXTENSION_KEY,
    ) {
        Ok(report) => Json(serde_json::json!({ "ok": true, "report": report })),
        Err(err) => Json(serde_json::json!({ "ok": false, "error": err.to_string() })),
    }
}

async fn post_oauth_device_poll(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<crate::oauth_device::OAuthDevicePollInput>,
) -> Json<Value> {
    match crate::oauth_device::poll_oauth_device_code(
        &state.bundle_path,
        &state.env,
        &req,
        crate::oauth_device::DEFAULT_EXTENSION_KEY,
    )
    .await
    {
        Ok(report) => Json(serde_json::json!({ "ok": true, "report": report })),
        Err(err) => Json(serde_json::json!({ "ok": false, "error": err.to_string() })),
    }
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

async fn ensure_setup_tunnel(state: &std::sync::Arc<UiState>, mode: &str) -> Result<String> {
    {
        let guard = state
            .setup_tunnel
            .lock()
            .map_err(|_| anyhow!("setup tunnel lock poisoned"))?;
        if let Some(tunnel) = guard.as_ref()
            && tunnel.mode == mode
        {
            return Ok(tunnel.public_base_url.clone());
        }
    }

    let mode = mode.to_string();
    let local_base_url = state.local_base_url.clone();
    let tunnel = tokio::task::spawn_blocking(move || start_setup_tunnel(&mode, &local_base_url))
        .await
        .map_err(|err| anyhow!("setup tunnel task failed: {err}"))??;
    let public_base_url = tunnel.public_base_url.clone();
    let mut guard = state
        .setup_tunnel
        .lock()
        .map_err(|_| anyhow!("setup tunnel lock poisoned"))?;
    *guard = Some(tunnel);
    Ok(public_base_url)
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
                pending_setup_actions: vec![],
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
                pending_setup_actions: vec![],
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
                pending_setup_actions: report.pending_setup_actions,
                provider_setup_status: JsonMap::new(),
            }
        }
        Err(e) => ExecutionResult {
            success: false,
            stdout,
            stderr: format!("Execution failed: {e}"),
            manual_steps: vec![],
            pending_setup_actions: vec![],
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
    use greentic_secrets_lib::SecretsStore;
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
            shutdown_tx,
            result: Mutex::new(None),
        })
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
    fn setup_backend_defaults_include_bot_framework_registration_runtime_capability_url() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = test_ui_state(temp.path());

        let config = super::default_setup_backend_config_with_runtime_base(
            &state,
            "demo",
            Some("http://127.0.0.1:9101/"),
        );

        assert_eq!(
            config["bot_framework_registration_url"],
            "http://127.0.0.1:9101/v1/setup/bot-framework/registration"
        );
    }

    #[tokio::test]
    async fn bot_framework_registration_missing_host_capability_is_blocked() {
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
                "public_base_url": "https://runtime.example.com"
            }),
        );
        let action = json!({
            "id": "register_endpoint",
            "executor": {
                "kind": "bot_framework_registration",
                "bot_app_id_config_key": "bot_app_id",
                "bot_app_password_config_key": "bot_app_password",
                "registration_url_template": "{bot_framework_registration_url}",
                "messaging_endpoint_template": "{public_base_url}/v1/messaging/ingress/{tenant}/{team}",
                "state_store_key": "last_reconcile"
            }
        });

        let result = super::setup_backend_execute_bot_framework_registration(
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
            result["result"]["missing_host_capability"],
            "bot_framework_registration"
        );
        assert!(
            result["next"]
                .as_str()
                .unwrap()
                .contains("GREENTIC_SETUP_RUNTIME_URL")
        );

        stored.insert("last_setup_result".to_string(), result);
        let rendered =
            super::render_setup_backend_contract_state(&state, &contract, "demo", stored);
        assert_eq!(
            rendered["setup_status"]["blocked"]["missing_host_capability"],
            "bot_framework_registration"
        );
        assert!(
            rendered["setup_status"]["blocked"]["summary"]
                .as_str()
                .unwrap()
                .contains("GREENTIC_SETUP_RUNTIME_URL")
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
        assert_eq!(body["ok"], true);
        assert_eq!(body["setup_status"]["ok"], false);
        assert_eq!(body["setup_status"]["items"].as_array().unwrap().len(), 3);
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
