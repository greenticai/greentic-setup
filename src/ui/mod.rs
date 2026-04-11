//! Greentic Setup web dashboard — Phase 1a.
//!
//! Serves an Alpine.js SPA from an Axum server bound to 127.0.0.1, with
//! bearer-token + origin authentication, security headers, and an embedded
//! static asset manifest.

#![allow(dead_code)]

mod assets;
pub mod auth;
pub mod bundle_yaml;
mod locales;
pub mod routes;
pub mod server;
mod sse;

pub mod api;
pub mod state;

// Re-export server pieces that integration tests exercise directly.
pub use routes::build_router;

use std::path::Path;

use anyhow::Result;
use serde_json::{Map, Value};

/// Public launch entry point used by `src/bin/greentic_setup.rs`.
///
/// Signature preserved from the legacy module so the CLI binary compiles
/// unchanged. `tenant`, `team`, `env`, and `locale` seed the initial scope
/// selection shown in the dashboard. `prefill_answers` populates the
/// wizard form when the user provided a `--answers` file, and
/// `scope_from_answers` tells the UI to lock the scope dropdowns to the
/// pre-selected values.
#[allow(clippy::too_many_arguments)]
pub async fn launch(
    bundle_path: &Path,
    tenant: &str,
    team: Option<&str>,
    env: &str,
    advanced: bool,
    locale: Option<&str>,
    prefill_answers: Option<Map<String, Value>>,
    scope_from_answers: bool,
) -> Result<()> {
    let options = server::LaunchOptions {
        initial_tenant: tenant.to_string(),
        initial_team: team.map(String::from),
        initial_env: env.to_string(),
        advanced,
        initial_locale: locale.map(String::from),
        prefill_answers,
        scope_from_answers,
    };
    server::launch_v2(bundle_path, options, routes::build_router).await
}
