//! Greentic Setup web dashboard.
//!
//! During Phase 1a migration this module re-exports the legacy `launch`
//! function so the CLI binary keeps working while the new dashboard is built
//! out piece by piece in sibling modules. The legacy module will be removed
//! in the cutover task at the end of Phase 1a.

#![allow(dead_code)] // skeleton modules may have unused items during migration

mod assets;
mod legacy;

// New modules — currently empty stubs, filled in by subsequent tasks.
// Each stays private until the cutover task rewires `launch`.
mod auth;
mod server;
mod routes;
pub mod state;
mod sse;
mod api;

pub use legacy::launch;
