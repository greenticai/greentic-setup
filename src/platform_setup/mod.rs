//! Bundle-level platform setup types and static routes policy handling.

mod persistence;
mod prompts;
mod types;
mod url;

// Re-export public types
pub use persistence::{
    load_effective_static_routes_defaults, load_runtime_public_base_url,
    load_static_routes_artifact, persist_static_routes_artifact, static_routes_artifact_path,
};
pub use prompts::{prompt_static_routes_policy, prompt_static_routes_policy_with_answers};
pub use types::{PlatformSetupAnswers, StaticRoutesAnswers, StaticRoutesPolicy};

#[cfg(test)]
mod tests {
    use super::prompts::merge_prompt_seed;
    use super::types::{
        PACK_DECLARED_POLICY, STATIC_ROUTES_VERSION, SURFACE_DISABLED, SURFACE_ENABLED,
        StaticRoutesAnswers, StaticRoutesPolicy,
    };
    use super::{load_effective_static_routes_defaults, persist_static_routes_artifact};

    #[test]
    fn disabled_is_default() {
        let policy = StaticRoutesPolicy::normalize(None, "dev").unwrap();
        assert_eq!(policy, StaticRoutesPolicy::disabled());
    }

    #[test]
    fn enabled_requires_base_url() {
        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                ..Default::default()
            }),
            "dev",
        )
        .unwrap_err();
        assert!(err.to_string().contains("public_base_url is required"));
    }

    #[test]
    fn normalizes_public_base_url() {
        let policy = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com/base/".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap();
        assert_eq!(
            policy.public_base_url.as_deref(),
            Some("https://example.com/base")
        );
        assert_eq!(policy.public_surface_policy, SURFACE_ENABLED);
        assert_eq!(policy.default_route_prefix_policy, PACK_DECLARED_POLICY);
        assert_eq!(policy.tenant_path_policy, PACK_DECLARED_POLICY);
    }

    #[test]
    fn rejects_query_and_fragment() {
        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com?x=1".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("query string"));

        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com#frag".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("fragment"));
    }

    #[test]
    fn allows_http_loopback_in_dev_only() {
        let policy = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("http://127.0.0.1:3000/".into()),
                ..Default::default()
            }),
            "dev",
        )
        .unwrap();
        assert_eq!(
            policy.public_base_url.as_deref(),
            Some("http://127.0.0.1:3000")
        );

        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("http://127.0.0.1:3000".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("dev"));
    }

    #[test]
    fn rejects_enabled_with_disabled_surface_policy() {
        let err = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com".into()),
                public_surface_policy: Some("disabled".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap_err();
        assert!(err.to_string().contains("incompatible"));
    }

    #[test]
    fn persists_and_loads_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let policy = StaticRoutesPolicy::normalize(
            Some(&StaticRoutesAnswers {
                public_web_enabled: Some(true),
                public_base_url: Some("https://example.com".into()),
                ..Default::default()
            }),
            "prod",
        )
        .unwrap();
        let path = persist_static_routes_artifact(temp.path(), &policy).unwrap();
        assert!(path.exists());
        let loaded = super::load_static_routes_artifact(temp.path())
            .unwrap()
            .unwrap();
        assert_eq!(loaded, policy);
    }

    #[test]
    fn effective_defaults_fall_back_to_runtime_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let runtime_dir = temp
            .path()
            .join("state")
            .join("runtime")
            .join("demo.default");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        std::fs::write(
            runtime_dir.join("endpoints.json"),
            r#"{"tenant":"demo","team":"default","public_base_url":"https://runtime.example.com"}"#,
        )
        .unwrap();

        let loaded =
            load_effective_static_routes_defaults(temp.path(), "demo", Some("default")).unwrap();
        assert_eq!(
            loaded.and_then(|policy| policy.public_base_url),
            Some("https://runtime.example.com".to_string())
        );
    }

    #[test]
    fn merge_prompt_seed_overlays_partial_answers_on_existing_policy() {
        let existing = StaticRoutesPolicy {
            version: STATIC_ROUTES_VERSION,
            public_web_enabled: false,
            public_base_url: Some("https://existing.example.com".into()),
            public_surface_policy: SURFACE_DISABLED.into(),
            default_route_prefix_policy: PACK_DECLARED_POLICY.into(),
            tenant_path_policy: PACK_DECLARED_POLICY.into(),
        };
        let answers = StaticRoutesAnswers {
            public_web_enabled: Some(true),
            public_base_url: None,
            public_surface_policy: Some(SURFACE_ENABLED.into()),
            default_route_prefix_policy: None,
            tenant_path_policy: None,
        };

        let merged = merge_prompt_seed(Some(&answers), Some(&existing));
        assert!(merged.public_web_enabled);
        assert_eq!(
            merged.public_base_url.as_deref(),
            Some("https://existing.example.com")
        );
        assert_eq!(merged.public_surface_policy, SURFACE_ENABLED);
    }
}
