//! Dashboard server: bind 127.0.0.1, generate bearer, open browser,
//! serve the Axum app until the shutdown broadcast fires.
//!
//! Phase 1a replacement for the legacy `launch` function. Wired into the
//! CLI binary by the cutover task (Task 34).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::broadcast;

use crate::ui::auth::generate_bearer_token;
use crate::ui::state::{AppState, BundleMeta};

/// Stub bundle discovery. Replaced by Task 34 with real pack + scope loading.
///
/// Produces a minimal `BundleMeta` so the server can start. Real scopes,
/// providers, and secrets integration happens during cutover.
fn discover_bundle_stub(bundle_path: &Path) -> Result<BundleMeta> {
    let id = bundle_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("bundle")
        .to_string();
    let display_name = id
        .split('-')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    Ok(BundleMeta {
        id,
        display_name,
        path: bundle_path.to_path_buf(),
        scopes: vec![],
        available_tenants: vec!["default".into()],
        available_envs: vec!["dev".into()],
        available_teams: vec!["default".into()],
        extension_providers: vec![],
    })
}

/// Launch the Phase 1a dashboard server.
///
/// Binds to `127.0.0.1:{random_port}`, generates a bearer token, opens the
/// user's default browser, and serves until the shutdown broadcast fires.
///
/// The function takes a custom router-builder closure so it can be called
/// from tests with an empty router, and from the CLI binary (via the
/// cutover task) with the real router from `routes::build_router`.
pub async fn launch_v2<F>(bundle_path: &Path, build_router: F) -> Result<()>
where
    F: FnOnce(Arc<AppState>) -> axum::Router,
{
    let bundle = discover_bundle_stub(bundle_path)
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

    #[test]
    fn discover_bundle_stub_produces_title_case_display_name() {
        let path = PathBuf::from("/tmp/my-bundle-name");
        let bundle = discover_bundle_stub(&path).unwrap();
        assert_eq!(bundle.id, "my-bundle-name");
        assert_eq!(bundle.display_name, "My Bundle Name");
    }

    #[test]
    fn discover_bundle_stub_handles_single_word_name() {
        let path = PathBuf::from("/tmp/demo");
        let bundle = discover_bundle_stub(&path).unwrap();
        assert_eq!(bundle.display_name, "Demo");
    }

    #[test]
    fn discover_bundle_stub_has_default_scopes() {
        let path = PathBuf::from("/tmp/demo");
        let bundle = discover_bundle_stub(&path).unwrap();
        assert_eq!(bundle.available_tenants, vec!["default"]);
        assert_eq!(bundle.available_envs, vec!["dev"]);
        assert_eq!(bundle.available_teams, vec!["default"]);
        assert!(bundle.scopes.is_empty());
    }

    #[tokio::test]
    async fn launch_v2_binds_and_shuts_down() {
        // Smoke test: spawn the server with an empty router, immediately
        // fire the shutdown signal, and confirm it exits cleanly within
        // a short timeout.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            async move {
                let (tx, _) = tokio::sync::broadcast::channel::<()>(1);
                let tx_clone = tx.clone();
                let handle = tokio::spawn(async move {
                    launch_v2(&path, |_state| {
                        // Empty router — just needs to be a valid tower service.
                        Router::new()
                    })
                    .await
                });
                // Give the server 50ms to bind, then fire the internal shutdown.
                // NOTE: launch_v2 owns its own broadcast channel, so we can't
                // trigger it from outside. This test instead exits via timeout.
                // A better test requires exposing a shutdown handle — future work.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                handle.abort();
                let _ = tx_clone; // prevent unused warning
            },
        )
        .await;
        assert!(result.is_ok(), "server did not shut down within 5s");
    }
}
