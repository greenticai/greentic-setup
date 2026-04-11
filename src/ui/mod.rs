//! Greentic Setup web dashboard — Phase 1a.
//!
//! Serves an Alpine.js SPA from an Axum server bound to 127.0.0.1, with
//! bearer-token + origin authentication, security headers, and an embedded
//! static asset manifest.

#![allow(dead_code)]

mod assets;
pub mod auth;
pub mod routes;
mod server;
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
/// unchanged. The extra arguments (tenant, team, env, advanced, locale,
/// prefill_answers, scope_from_answers) are currently unused in Phase 1a
/// — they'll be passed through to the wizard context in Phase 1b.
#[allow(clippy::too_many_arguments)]
pub async fn launch(
    bundle_path: &Path,
    _tenant: &str,
    _team: Option<&str>,
    _env: &str,
    _advanced: bool,
    _locale: Option<&str>,
    _prefill_answers: Option<Map<String, Value>>,
    _scope_from_answers: bool,
) -> Result<()> {
    server::launch_v2(bundle_path, routes::build_router).await
}
