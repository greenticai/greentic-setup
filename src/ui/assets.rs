//! Embedded static assets for the setup UI.

pub const INDEX_HTML: &str = include_str!("../../assets/setup-ui/index.html");
pub const APP_JS: &str = include_str!("../../assets/setup-ui/app.js");
pub const STYLE_CSS: &str = include_str!("../../assets/setup-ui/style.css");

/// Catalog of additional providers a user can add to the bundle, referencing
/// their GHCR/OCI pack assets. Surfaced by `GET /api/available-providers` and
/// rendered as the scrollable "add providers" list on the provider page.
pub const PROVIDERS_CATALOG: &str = include_str!("../../assets/setup-ui/providers-catalog.json");
