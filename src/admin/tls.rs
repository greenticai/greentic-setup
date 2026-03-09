//! mTLS configuration for the admin API endpoint.
//!
//! Defines `AdminTlsConfig` for loading server and client certificates used
//! to authenticate admin API consumers. The actual TLS server setup happens
//! in the consuming crate (e.g. greentic-operator via `axum-server` + `rustls`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// TLS configuration for the admin API endpoint.
///
/// # Example
///
/// ```rust
/// use greentic_setup::admin::AdminTlsConfig;
///
/// let config = AdminTlsConfig {
///     server_cert: "/etc/greentic/admin/server.crt".into(),
///     server_key: "/etc/greentic/admin/server.key".into(),
///     client_ca: "/etc/greentic/admin/ca.crt".into(),
///     allowed_clients: vec!["CN=greentic-admin".to_string()],
///     port: 8443,
/// };
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AdminTlsConfig {
    /// Path to the server TLS certificate (PEM).
    pub server_cert: PathBuf,
    /// Path to the server TLS private key (PEM).
    pub server_key: PathBuf,
    /// Path to the CA certificate for verifying client certificates (PEM).
    pub client_ca: PathBuf,
    /// Optional list of allowed client CN (Common Name) patterns.
    ///
    /// If empty, any client with a certificate signed by the CA is allowed.
    #[serde(default)]
    pub allowed_clients: Vec<String>,
    /// Port to bind the admin API server on.
    #[serde(default = "default_admin_port")]
    pub port: u16,
}

fn default_admin_port() -> u16 {
    8443
}

impl Default for AdminTlsConfig {
    fn default() -> Self {
        Self {
            server_cert: PathBuf::from("admin/server.crt"),
            server_key: PathBuf::from("admin/server.key"),
            client_ca: PathBuf::from("admin/ca.crt"),
            allowed_clients: Vec::new(),
            port: default_admin_port(),
        }
    }
}

impl AdminTlsConfig {
    /// Validate that all referenced certificate files exist.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (label, path) in [
            ("server_cert", &self.server_cert),
            ("server_key", &self.server_key),
            ("client_ca", &self.client_ca),
        ] {
            if !path.exists() {
                return Err(anyhow::anyhow!(
                    "admin TLS {label} not found: {}",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    /// Check whether a client Common Name is allowed by this config.
    ///
    /// Returns `true` if `allowed_clients` is empty (any client allowed)
    /// or if the CN matches one of the patterns.
    pub fn is_client_allowed(&self, cn: &str) -> bool {
        if self.allowed_clients.is_empty() {
            return true;
        }
        self.allowed_clients
            .iter()
            .any(|pattern| pattern == cn || pattern == "*")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_is_8443() {
        let config = AdminTlsConfig::default();
        assert_eq!(config.port, 8443);
    }

    #[test]
    fn empty_allowed_clients_allows_anyone() {
        let config = AdminTlsConfig::default();
        assert!(config.is_client_allowed("anything"));
    }

    #[test]
    fn rejects_unlisted_client() {
        let config = AdminTlsConfig {
            allowed_clients: vec!["CN=admin".into()],
            ..Default::default()
        };
        assert!(config.is_client_allowed("CN=admin"));
        assert!(!config.is_client_allowed("CN=hacker"));
    }

    #[test]
    fn wildcard_allows_all() {
        let config = AdminTlsConfig {
            allowed_clients: vec!["*".into()],
            ..Default::default()
        };
        assert!(config.is_client_allowed("anyone"));
    }

    #[test]
    fn validate_fails_for_missing_certs() {
        let config = AdminTlsConfig {
            server_cert: "/nonexistent/server.crt".into(),
            server_key: "/nonexistent/server.key".into(),
            client_ca: "/nonexistent/ca.crt".into(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}
