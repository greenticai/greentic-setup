//! Admin API types and configuration for secure bundle lifecycle management.
//!
//! This module defines the types and configuration for an mTLS-secured admin
//! API that enables runtime bundle deployment, update, removal, and lifecycle
//! control.
//!
//! This crate owns the shared request/response contract and TLS configuration.
//! The actual HTTP server and route ownership live in the consuming runtime host
//! (for example `greentic-start` for runtime lifecycle control).

pub mod routes;
pub mod tls;

pub use routes::{
    AdminClientAddRequest, AdminClientEntry, AdminClientListResponse, AdminClientRemoveRequest,
    AdminRequest, AdminResponse, BundleDeployRequest, BundleListResponse, BundleStartRequest,
    BundleStatus, BundleStatusResponse, BundleStopRequest, BundleUpdateRequest, QaSubmitRequest,
    QaValidateRequest,
};
pub use tls::AdminTlsConfig;
