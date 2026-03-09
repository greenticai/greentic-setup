//! Admin API types and configuration for secure bundle lifecycle management.
//!
//! This module defines the types and configuration for an mTLS-secured admin
//! API that enables runtime bundle deployment, upgrade, and removal without
//! operator restart.
//!
//! The actual HTTP server and routes live in the consuming crate
//! (e.g. greentic-operator), which mounts these handlers on a dedicated port.

pub mod routes;
pub mod tls;

pub use routes::{
    AdminRequest, AdminResponse, BundleDeployRequest, BundleStatus, BundleStatusResponse,
    QaSubmitRequest, QaValidateRequest,
};
pub use tls::AdminTlsConfig;
