//! Embedded static assets for the Phase 1a dashboard UI.
//!
//! Assets are bundled into the binary at compile time via `include_bytes!`
//! so the setup server works offline and in airgapped environments.

pub struct Asset {
    /// URL path as served by Axum — always starts with `/`.
    pub path: &'static str,
    /// MIME type for the `Content-Type` response header.
    pub mime: &'static str,
    /// Raw bytes of the asset.
    pub body: &'static [u8],
}

/// Full list of embedded assets.
pub const ASSETS: &[Asset] = &[
    // Alpine.js vendor bundle
    Asset {
        path: "/vendor/alpine/alpine.min.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/vendor/alpine/alpine.min.js"),
    },
    // Poppins font (Latin subset, 4 weights)
    Asset {
        path: "/vendor/poppins/poppins-400.woff2",
        mime: "font/woff2",
        body: include_bytes!("../../assets/setup-ui/vendor/poppins/poppins-400.woff2"),
    },
    Asset {
        path: "/vendor/poppins/poppins-500.woff2",
        mime: "font/woff2",
        body: include_bytes!("../../assets/setup-ui/vendor/poppins/poppins-500.woff2"),
    },
    Asset {
        path: "/vendor/poppins/poppins-600.woff2",
        mime: "font/woff2",
        body: include_bytes!("../../assets/setup-ui/vendor/poppins/poppins-600.woff2"),
    },
    Asset {
        path: "/vendor/poppins/poppins-700.woff2",
        mime: "font/woff2",
        body: include_bytes!("../../assets/setup-ui/vendor/poppins/poppins-700.woff2"),
    },
    // Stylesheets
    Asset {
        path: "/styles/tokens.css",
        mime: "text/css; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/styles/tokens.css"),
    },
    Asset {
        path: "/styles/base.css",
        mime: "text/css; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/styles/base.css"),
    },
    Asset {
        path: "/styles/layout.css",
        mime: "text/css; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/styles/layout.css"),
    },
    Asset {
        path: "/styles/components.css",
        mime: "text/css; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/styles/components.css"),
    },
    Asset {
        path: "/styles/animations.css",
        mime: "text/css; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/styles/animations.css"),
    },
    // Icons
    Asset {
        path: "/icons/greentic-mascot.png",
        mime: "image/png",
        body: include_bytes!("../../assets/setup-ui/icons/greentic-mascot.png"),
    },
    // Phase 1a SPA shell + JS
    Asset {
        path: "/index.html",
        mime: "text/html; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/index.html"),
    },
    Asset {
        path: "/js/app.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/app.js"),
    },
    Asset {
        path: "/js/api.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/api.js"),
    },
    Asset {
        path: "/js/router.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/router.js"),
    },
    Asset {
        path: "/js/formatters.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/formatters.js"),
    },
    Asset {
        path: "/js/stores/bundle.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/stores/bundle.js"),
    },
    Asset {
        path: "/js/stores/scope.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/stores/scope.js"),
    },
    Asset {
        path: "/js/stores/overview.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/stores/overview.js"),
    },
    Asset {
        path: "/js/stores/wizard.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/stores/wizard.js"),
    },
    Asset {
        path: "/js/stores/locale.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/stores/locale.js"),
    },
    Asset {
        path: "/js/stores/ui.js",
        mime: "application/javascript; charset=utf-8",
        body: include_bytes!("../../assets/setup-ui/js/stores/ui.js"),
    },
];

/// Look up an embedded asset by its request path.
pub fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.path == path)
}

/// Return the MIME type for a file extension, or a reasonable default.
///
/// Used as a fallback when `find()` is not appropriate (e.g. generated
/// content). Keep the list small — each mapping should earn its place.
pub fn mime_for_extension(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_assets_have_nonempty_bodies() {
        for asset in ASSETS {
            assert!(!asset.body.is_empty(), "empty asset: {}", asset.path);
        }
    }

    #[test]
    fn find_returns_some_for_known_path() {
        assert!(find("/styles/tokens.css").is_some());
    }

    #[test]
    fn find_returns_none_for_unknown_path() {
        assert!(find("/nonexistent.txt").is_none());
    }

    #[test]
    fn mime_for_extension_handles_common_types() {
        assert_eq!(mime_for_extension("html"), "text/html; charset=utf-8");
        assert_eq!(
            mime_for_extension("js"),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(mime_for_extension("woff2"), "font/woff2");
        assert_eq!(mime_for_extension("png"), "image/png");
        assert_eq!(mime_for_extension("unknown"), "application/octet-stream");
    }

    #[test]
    fn all_paths_start_with_slash() {
        for asset in ASSETS {
            assert!(asset.path.starts_with('/'), "bad path: {}", asset.path);
        }
    }
}
