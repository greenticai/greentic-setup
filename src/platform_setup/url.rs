//! URL normalization and validation for public base URLs.

use std::net::IpAddr;

use anyhow::{Result, anyhow, bail};
use url::Url;

/// Normalize and validate a public base URL.
///
/// Accepts environment variable placeholders (e.g., "${PUBLIC_BASE_URL}") as-is.
/// For actual URLs, validates:
/// - Must use https (or http for localhost/loopback in dev)
/// - Must include a host
/// - Must not include query string or fragment
/// - Trailing slashes are removed
pub fn normalize_public_base_url(value: &str, env: &str) -> Result<String> {
    // Accept environment variable placeholders as-is (e.g., "${PUBLIC_BASE_URL}")
    // These will be resolved at runtime
    if value.starts_with("${") && value.ends_with('}') {
        return Ok(value.to_string());
    }

    let url = Url::parse(value).map_err(|err| anyhow!("invalid public_base_url: {err}"))?;
    match url.scheme() {
        "https" => {}
        "http" if is_local_http_origin(&url) => {}
        "http" => bail!("public_base_url must use https unless it targets localhost/loopback"),
        _ => bail!("public_base_url must use http or https"),
    }

    if url.host_str().is_none() {
        bail!("public_base_url must include a host");
    }
    if url.query().is_some() {
        bail!("public_base_url must not include a query string");
    }
    if url.fragment().is_some() {
        bail!("public_base_url must not include a fragment");
    }
    if env != "dev" && url.scheme() == "http" {
        bail!("public_base_url may only use http for localhost/loopback origins in dev");
    }

    let mut normalized = url.to_string();
    while normalized.ends_with('/') && normalized.len() > scheme_host_floor(&url) {
        normalized.pop();
    }
    if normalized.ends_with('/') && url.path() == "/" {
        normalized.pop();
    }
    Ok(normalized)
}

/// Calculate the minimum length of a URL (scheme + host + optional port).
fn scheme_host_floor(url: &Url) -> usize {
    let host = url.host_str().unwrap_or_default();
    let mut floor = url.scheme().len() + 3 + host.len();
    if let Some(port) = url.port() {
        floor += 1 + port.to_string().len();
    }
    floor
}

/// Check if a URL targets localhost or a loopback address.
fn is_local_http_origin(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|addr| addr.is_loopback())
        .unwrap_or(false)
}
