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

use crate::engine::{SetupConfig, SetupRequest};
use crate::plan::TenantSelection;
use crate::platform_setup::StaticRoutesPolicy;
use crate::qa::wizard;
use crate::{SetupEngine, SetupMode, discovery, setup_to_formspec};

// ── Types ──

struct UiState {
    bundle_path: PathBuf,
    tenant: String,
    team: Option<String>,
    env: String,
    #[allow(dead_code)]
    advanced: bool,
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
    help: Option<String>,
    choices: Option<Vec<String>>,
    visible_if: Option<VisibleIfInfo>,
}

#[derive(Serialize, Clone)]
struct VisibleIfInfo {
    field: String,
    eq: Option<String>,
}

#[derive(Deserialize)]
struct ExecuteRequest {
    answers: JsonMap<String, Value>,
}

#[derive(Serialize, Clone)]
struct ExecutionResult {
    success: bool,
    stdout: String,
    stderr: String,
}

// ── Public API ──

/// Launch the setup UI server and open in browser.
pub async fn launch(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    advanced: bool,
) -> Result<()> {
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    let state = std::sync::Arc::new(UiState {
        bundle_path: bundle_path.to_path_buf(),
        tenant: tenant.to_string(),
        team: team.map(String::from),
        env: env.to_string(),
        advanced,
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
        .route("/api/providers", get(get_providers))
        .route("/api/execute", post(post_execute))
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

async fn get_providers(State(state): State<std::sync::Arc<UiState>>) -> Json<Value> {
    let bundle_path = &state.bundle_path;

    let discovered = match discovery::discover(bundle_path) {
        Ok(d) => d,
        Err(e) => {
            return Json(serde_json::json!({
                "bundle_path": bundle_path.display().to_string(),
                "providers": [],
                "provider_forms": [],
                "shared_questions": [],
                "error": e.to_string(),
            }));
        }
    };

    let provider_form_specs: Vec<wizard::ProviderFormSpec> = discovered
        .providers
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

    // Detect shared questions
    let shared_questions = if provider_form_specs.len() > 1 {
        let shared = wizard::collect_shared_questions(&provider_form_specs);
        shared
            .shared_questions
            .iter()
            .map(form_question_to_info)
            .collect::<Vec<_>>()
    } else {
        vec![]
    };

    let providers: Vec<ProviderInfo> = discovered
        .providers
        .iter()
        .map(|p| {
            let form = setup_to_formspec::pack_to_form_spec(&p.pack_path, &p.provider_id);
            ProviderInfo {
                provider_id: p.provider_id.clone(),
                domain: p.domain.clone(),
                question_count: form.as_ref().map(|f| f.questions.len()).unwrap_or(0),
            }
        })
        .collect();

    let provider_forms: Vec<ProviderForm> = provider_form_specs
        .iter()
        .map(|pfs| ProviderForm {
            provider_id: pfs.provider_id.clone(),
            title: pfs.form_spec.title.clone(),
            questions: pfs
                .form_spec
                .questions
                .iter()
                .map(form_question_to_info)
                .collect(),
        })
        .collect();

    Json(serde_json::json!({
        "bundle_path": bundle_path.display().to_string(),
        "providers": providers,
        "provider_forms": provider_forms,
        "shared_questions": shared_questions,
    }))
}

async fn post_execute(
    State(state): State<std::sync::Arc<UiState>>,
    Json(req): Json<ExecuteRequest>,
) -> Json<ExecutionResult> {
    let bundle_path = state.bundle_path.clone();
    let tenant = state.tenant.clone();
    let team = state.team.clone();
    let env = state.env.clone();
    let answers = req.answers;

    let result = tokio::task::spawn_blocking(move || {
        execute_setup(&bundle_path, &tenant, team.as_deref(), &env, answers)
    })
    .await
    .unwrap_or_else(|e| ExecutionResult {
        success: false,
        stdout: String::new(),
        stderr: format!("Task panicked: {e}"),
    });

    *state.result.lock().unwrap() = Some(result.clone());
    Json(result)
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
            };
        }
    };

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
            }
        }
        Err(e) => ExecutionResult {
            success: false,
            stdout,
            stderr: format!("Execution failed: {e}"),
        },
    }
}

// ── Helpers ──

fn form_question_to_info(q: &qa_spec::QuestionSpec) -> QuestionInfo {
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

    QuestionInfo {
        id: q.id.clone(),
        title: q.title.clone(),
        kind: format!("{:?}", q.kind),
        required: q.required,
        secret: q.secret,
        default_value: q.default_value.clone(),
        help: q.description.clone(),
        choices: q.choices.clone(),
        visible_if,
    }
}
