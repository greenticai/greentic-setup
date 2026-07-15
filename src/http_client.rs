//! Crate-wide outbound HTTP agent constructors.
//!
//! Every outbound call must carry a hard deadline: a stalled remote must
//! FAIL, never hang the wizard. Bare `ureq::get/post(...)` calls have no
//! default timeout in ureq 3 — do not add new ones; construct an agent here
//! instead so the deadline policy stays in one place.

use std::time::Duration;

/// Hard deadline for interactive API calls (OAuth token/device-code
/// exchanges, Microsoft Graph, provider setup steps). Generous enough for
/// slow provider backends; small enough that a wedged connection surfaces
/// as an error while the operator is still watching.
pub const API_TIMEOUT: Duration = Duration::from_secs(30);

/// Hard deadline for potentially large downloads (bundle archives, tunnel
/// binaries). Caps the entire transfer, not just connect.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// Agent for API calls with ureq's default status handling (non-2xx becomes
/// `Err(ureq::Error::StatusCode(..))`) — drop-in for bare `ureq::` calls.
pub fn api_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(API_TIMEOUT))
        .build()
        .new_agent()
}

/// Agent for API calls where non-2xx statuses are handled as regular
/// responses (`http_status_as_error(false)`) — drop-in for call sites that
/// inspect status codes themselves (OAuth polling, Graph error bodies).
pub fn api_agent_any_status() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(API_TIMEOUT))
        .build()
        .new_agent()
}

/// Agent for downloads: same failure semantics, longer transfer budget.
pub fn download_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .new_agent()
}
