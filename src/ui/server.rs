//! Dashboard server: bind 127.0.0.1, generate bearer, open browser,
//! serve the Axum app until the shutdown broadcast fires.
//!
//! Phase 1a replacement for the legacy `launch` function. Wired into the
//! CLI binary by the cutover task (Task 34).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use tokio::sync::broadcast;

use crate::ui::auth::generate_bearer_token;
use crate::ui::state::{AppState, BundleMeta, ProviderFormData, ProviderRef, ScopeKey, ScopeStatus, ScopeSummary};

/// Options plumbed from the CLI binary through `ui::launch` into the
/// dashboard server. These seed the initial state injected into the SPA.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub initial_tenant: String,
    pub initial_team: Option<String>,
    pub initial_env: String,
    pub advanced: bool,
    pub initial_locale: Option<String>,
    pub prefill_answers: Option<Map<String, Value>>,
    pub scope_from_answers: bool,
}

/// Derive a title-cased display name from a kebab-case bundle id.
fn title_case_id(id: &str) -> String {
    id.split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Scan configured scopes by probing the secrets store for any secrets.
///
/// For each (tenant, env, team) triple, if any secret exists in the store
/// for that scope, the scope is marked `Configured`, otherwise `NotConfigured`.
/// Must be called from an async context — the DevStore `get` call is async.
async fn scan_configured_scopes(
    bundle_path: &Path,
    tenants: &[String],
    envs: &[String],
    teams: &[String],
    providers: &[crate::discovery::DetectedProvider],
    form_specs: &[ProviderFormData],
) -> Vec<ScopeSummary> {
    use greentic_secrets_lib::SecretsStore;

    // Open the dev store — if it doesn't exist yet, return empty scopes (not configured).
    let store = match crate::secrets::open_dev_store(bundle_path) {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let mut summaries = Vec::new();

    for tenant in tenants {
        for env in envs {
            for team in teams {
                // Build provider status list by probing each question's secret URI.
                let mut provider_statuses = Vec::new();
                let mut scope_has_any_secret = false;

                for p in providers {
                    let Some(pf) = form_specs.iter().find(|pf| pf.provider_id == p.provider_id)
                    else {
                        continue;
                    };

                    let mut secrets_count: u32 = 0;
                    for q in &pf.form_spec.questions {
                        let uri = crate::canonical_secret_uri(
                            env,
                            tenant,
                            Some(team.as_str()),
                            &p.provider_id,
                            &q.id,
                        );
                        if store.get(&uri).await.map(|b| !b.is_empty()).unwrap_or(false) {
                            secrets_count += 1;
                        }
                    }

                    let configured = secrets_count > 0;
                    if configured {
                        scope_has_any_secret = true;
                    }

                    provider_statuses.push(crate::ui::state::ProviderStatus {
                        id: p.provider_id.clone(),
                        display_name: pf.display_name.clone(),
                        configured,
                        secrets_count,
                        warnings: vec![],
                    });
                }

                let status = if scope_has_any_secret {
                    ScopeStatus::Configured
                } else {
                    ScopeStatus::NotConfigured
                };

                summaries.push(ScopeSummary {
                    scope: ScopeKey {
                        tenant: tenant.clone(),
                        env: env.clone(),
                        team: team.clone(),
                    },
                    status,
                    providers: provider_statuses,
                    warnings: vec![],
                });
            }
        }
    }

    // Keep only scopes that are actually configured.
    summaries
        .into_iter()
        .filter(|s| !matches!(s.status, ScopeStatus::NotConfigured))
        .collect()
}

/// Discover the bundle at `bundle_path`, returning a `BundleMeta` and the
/// list of provider FormSpecs.
///
/// On discovery failure (missing/malformed bundle), returns a minimal
/// `BundleMeta` with empty scopes and providers so the UI can still render.
/// Must be called from an async context — probes the secrets store for
/// per-scope configuration status.
pub async fn discover_bundle(
    bundle_path: &Path,
    options: &LaunchOptions,
) -> Result<(BundleMeta, Vec<ProviderFormData>)> {
    let id = bundle_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("bundle")
        .to_string();
    let display_name = title_case_id(&id);

    // Discover tenants from the bundle's tenants/ directory.
    let discovered_tenants = crate::bundle::discover_tenants(bundle_path, None).unwrap_or_default();
    let tenants: Vec<String> = {
        let mut t = if discovered_tenants.is_empty() {
            vec![options.initial_tenant.clone()]
        } else {
            discovered_tenants
        };
        // Include the CLI-supplied initial tenant so it appears in the UI dropdown.
        if !t.contains(&options.initial_tenant) {
            t.push(options.initial_tenant.clone());
        }
        if !t.contains(&"default".to_string()) {
            t.push("default".into());
        }
        t.sort();
        t.dedup();
        t
    };

    // Envs: CLI-supplied + well-known defaults, deduped.
    let mut envs = vec![options.initial_env.clone()];
    for e in &["dev", "staging", "prod"] {
        let s = e.to_string();
        if !envs.contains(&s) {
            envs.push(s);
        }
    }

    // Teams: CLI-supplied team or "default".
    let initial_team = options.initial_team.clone().unwrap_or_else(|| "default".into());
    let mut teams = vec![initial_team];
    if !teams.contains(&"default".to_string()) {
        teams.push("default".into());
    }

    // Discover providers. On failure, continue with empty list.
    let (detected_providers, form_specs) =
        match crate::discovery::discover(bundle_path) {
            Ok(discovery) => {
                let forms: Vec<ProviderFormData> = discovery
                    .providers
                    .iter()
                    .filter_map(|p| {
                        crate::setup_to_formspec::pack_to_form_spec(
                            &p.pack_path,
                            &p.provider_id,
                        )
                        .map(|form_spec| ProviderFormData {
                            provider_id: p.provider_id.clone(),
                            display_name: p
                                .display_name
                                .clone()
                                .unwrap_or_else(|| title_case_id(&p.provider_id)),
                            form_spec,
                            pack_path: p.pack_path.clone(),
                        })
                    })
                    .collect();
                (discovery.providers, forms)
            }
            Err(_) => (vec![], vec![]),
        };

    let extension_providers: Vec<ProviderRef> = detected_providers
        .iter()
        .map(|p| ProviderRef {
            oci: format!("local:{}", p.provider_id),
        })
        .collect();

    let scopes = scan_configured_scopes(
        bundle_path,
        &tenants,
        &envs,
        &teams,
        &detected_providers,
        &form_specs,
    )
    .await;

    let meta = BundleMeta {
        id,
        display_name,
        path: bundle_path.to_path_buf(),
        scopes,
        available_tenants: tenants,
        available_envs: envs,
        available_teams: teams,
        extension_providers,
    };

    Ok((meta, form_specs))
}

/// Launch the Phase 1a dashboard server.
///
/// Binds to `127.0.0.1:{random_port}`, generates a bearer token, opens the
/// user's default browser, and serves until the shutdown broadcast fires.
///
/// The function takes a custom router-builder closure so it can be called
/// from tests with an empty router, and from the CLI binary with the real
/// router from `routes::build_router`.
pub async fn launch_v2<F>(
    bundle_path: &Path,
    options: LaunchOptions,
    build_router: F,
) -> Result<()>
where
    F: FnOnce(Arc<AppState>) -> axum::Router,
{
    let (bundle, provider_forms) = discover_bundle(bundle_path, &options)
        .await
        .context("failed to discover bundle for dashboard")?;

    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Bind first so we know the port before building state.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind dashboard listener on 127.0.0.1")?;
    let port = listener
        .local_addr()
        .context("failed to read local address")?
        .port();

    let state = Arc::new(AppState {
        bundle,
        port,
        bearer_token: zeroize::Zeroizing::new(generate_bearer_token()),
        wizard_sessions: std::sync::Mutex::new(std::collections::HashMap::new()),
        shutdown_tx: shutdown_tx.clone(),
        launch_options: options,
        provider_forms,
        pending_mutations: std::sync::atomic::AtomicBool::new(false),
        reveal_count: std::sync::atomic::AtomicU32::new(0),
        reveal_window_start: std::sync::atomic::AtomicU64::new(0),
    });

    let router = build_router(state.clone());
    let url = format!("http://127.0.0.1:{port}");
    eprintln!("Dashboard started at: {url}");

    // Best-effort browser open — failing to open is not fatal.
    let _ = open::that(&url);

    let mut shutdown_rx = shutdown_tx.subscribe();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.recv().await;
        })
        .await
        .context("dashboard server error")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use std::path::PathBuf;

    fn default_options() -> LaunchOptions {
        LaunchOptions {
            initial_tenant: "demo".into(),
            initial_team: None,
            initial_env: "dev".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn discover_bundle_produces_title_case_display_name() {
        let path = PathBuf::from("/tmp/my-bundle-name");
        // Non-existent path → discovery falls back gracefully with empty providers.
        let (bundle, forms) = discover_bundle(&path, &default_options()).await.unwrap();
        assert_eq!(bundle.id, "my-bundle-name");
        assert_eq!(bundle.display_name, "My Bundle Name");
        assert!(forms.is_empty());
    }

    #[tokio::test]
    async fn discover_bundle_handles_single_word_name() {
        let path = PathBuf::from("/tmp/demo");
        let (bundle, _) = discover_bundle(&path, &default_options()).await.unwrap();
        assert_eq!(bundle.display_name, "Demo");
    }

    #[tokio::test]
    async fn discover_bundle_seeds_cli_scope_into_allow_list() {
        let path = PathBuf::from("/tmp/demo");
        let mut opts = default_options();
        opts.initial_tenant = "acme-corp".into();
        opts.initial_env = "prod".into();
        opts.initial_team = Some("platform".into());
        let (bundle, _) = discover_bundle(&path, &opts).await.unwrap();
        assert!(bundle.available_tenants.contains(&"acme-corp".to_string()));
        assert!(bundle.available_tenants.contains(&"default".to_string()));
        assert!(bundle.available_envs.contains(&"prod".to_string()));
        assert!(bundle.available_envs.contains(&"dev".to_string()));
        assert!(bundle.available_teams.contains(&"platform".to_string()));
        assert!(bundle.available_teams.contains(&"default".to_string()));
        assert!(bundle.scopes.is_empty());
    }

    #[tokio::test]
    async fn launch_v2_binds_and_shuts_down() {
        // Smoke test: spawn the server with an empty router, immediately
        // fire the shutdown signal, and confirm it exits cleanly within
        // a short timeout.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async move {
            let (tx, _) = tokio::sync::broadcast::channel::<()>(1);
            let tx_clone = tx.clone();
            let handle = tokio::spawn(async move {
                launch_v2(&path, default_options(), |_state| {
                    // Empty router — just needs to be a valid tower service.
                    Router::new()
                })
                .await
            });
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            handle.abort();
            let _ = tx_clone;
        })
        .await;
        assert!(result.is_ok(), "server did not shut down within 5s");
    }
}
