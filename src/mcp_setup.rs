//! Collect an MCP server's deployment inputs at setup time.
//!
//! A pack's `mcp` flow node names only an opaque `server` id. greentic-designer
//! writes the non-secret half of the route into `assets/mcp-routes.json`, and
//! greentic-runner reads it — but the credential is deliberately NOT in the
//! artefact. It is resolved at dispatch time from
//! `secrets://default/<tenant>/<team>/mcp/<server_id>`, a URI
//! greentic-designer-admin writes.
//!
//! That leaves a bundle booted by `gtc start` with no way to get a credential
//! at all: there is no admin in a local boot. This module closes that by asking
//! the operator during setup and writing to the URI the runner already reads.
//!
//! # Why this does not reuse the pack-secret-requirement path
//!
//! `qa::persist` writes `secrets://<env>/<tenant>/<team>/<provider>/<key>` via
//! `canonical_secret_uri`, which runs every segment through
//! [`crate::secret_name::canonical_secret_name`] — lowercase, and `-` → `_`.
//! An MCP server id is a hyphenated UUID, so that would write
//! `…/mcp/ff308b9c_951a_…` while the runner reads `…/mcp/ff308b9c-951a-…`.
//! It would look like it worked and resolve nothing. The env segment differs
//! too: the wizard's env is `local`, the MCP convention pins `default`.
//!
//! So this module builds the URI itself, verbatim, and
//! `greentic_aw_runtime::mcp_secrets` on the runner side remains the normative
//! statement of the shape.

use std::path::Path;

use serde::Deserialize;

/// Pack entry the designer writes and the runner reads.
pub const MCP_ROUTES_ENTRY: &str = "assets/mcp-routes.json";

/// Env segment for MCP secrets. Pinned to `default` regardless of the wizard's
/// environment, matching admin's writer and the runner's reader.
pub const MCP_ENV_SEGMENT: &str = "default";

/// Team segment used when the deployment is not team-scoped.
pub const MCP_DEFAULT_TEAM: &str = "_";

/// Question-id prefix for an MCP server's credential. The server id follows
/// VERBATIM — never canonicalized.
pub const MCP_TOKEN_PREFIX: &str = "mcp_token__";

/// Question-id prefix for an MCP server's host override.
pub const MCP_URL_PREFIX: &str = "mcp_url__";

/// One route record from the pack sidecar. Only the fields setup needs.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PackMcpRoute {
    pub server_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub transport: String,
    #[serde(default)]
    pub transport_url: Option<String>,
    #[serde(default)]
    pub auth_header_name: Option<String>,
}

impl PackMcpRoute {
    /// Whether this route reaches a server over HTTP and therefore needs a
    /// credential. A `local-wasm` route has no HTTP endpoint and no token.
    #[must_use]
    pub fn is_http(&self) -> bool {
        self.transport != "local-wasm"
    }
}

/// Read the MCP route sidecar out of a `.gtpack`.
///
/// Every failure yields an empty list: a pack without the sidecar, or with a
/// damaged one, must still set up. A missing route surfaces later as a runner
/// error naming the server, which is far better than aborting the wizard.
#[must_use]
pub fn routes_from_pack(pack_path: &Path) -> Vec<PackMcpRoute> {
    let Ok(file) = std::fs::File::open(pack_path) else {
        return Vec::new();
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    let Ok(entry) = archive.by_name(MCP_ROUTES_ENTRY) else {
        return Vec::new();
    };
    match serde_json::from_reader::<_, Vec<PackMcpRoute>>(entry) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::warn!(
                error = %error,
                entry = MCP_ROUTES_ENTRY,
                "ignoring malformed MCP route sidecar"
            );
            Vec::new()
        }
    }
}

/// The secret URI an MCP credential must be written to, byte-for-byte what
/// `greentic_aw_runtime::mcp_secrets` reads.
#[must_use]
pub fn mcp_secret_uri(tenant: &str, team: Option<&str>, server_id: &str) -> String {
    // Every segment is emitted verbatim. Do NOT route any of them through
    // `canonical_secret_name`: see the module doc.
    let team = team.filter(|t| !t.is_empty()).unwrap_or(MCP_DEFAULT_TEAM);
    format!("secrets://{MCP_ENV_SEGMENT}/{tenant}/{team}/mcp/{server_id}")
}

/// Question id carrying `server_id` verbatim.
#[must_use]
pub fn token_question_id(server_id: &str) -> String {
    format!("{MCP_TOKEN_PREFIX}{server_id}")
}

/// Recover the server id from a token question id.
#[must_use]
pub fn server_id_from_token_question(question_id: &str) -> Option<&str> {
    question_id.strip_prefix(MCP_TOKEN_PREFIX)
}

/// Append one question per HTTP MCP server the pack declares, so every wizard
/// path — interactive prompt, `--answers`, `--emit-answers`,
/// `--non-interactive` and the UI — collects the credential the same way it
/// collects any other pack secret.
///
/// Both questions are OPTIONAL and the URL defaults to the sidecar's own value.
/// A bundle with several MCP servers must not become a mandatory
/// interrogation, and a tenant whose credential is already written by admin
/// must keep working untouched.
///
/// `local-wasm` routes are skipped: they have no HTTP endpoint and admin writes
/// no credential for one.
pub fn augment_with_mcp_routes(mut form: qa_spec::FormSpec, pack_path: &Path) -> qa_spec::FormSpec {
    for route in routes_from_pack(pack_path)
        .into_iter()
        .filter(PackMcpRoute::is_http)
    {
        let label = route
            .name
            .clone()
            .unwrap_or_else(|| route.server_id.clone());

        if !form
            .questions
            .iter()
            .any(|q| q.id == url_question_id(&route.server_id))
        {
            form.questions.push(qa_spec::QuestionSpec {
                id: url_question_id(&route.server_id),
                kind: qa_spec::QuestionType::String,
                title: format!("MCP server '{label}' URL"),
                title_i18n: None,
                description: Some(
                    "Leave blank to use the URL the pack was built with.".to_string(),
                ),
                description_i18n: None,
                required: false,
                choices: None,
                default_value: route.transport_url.clone(),
                secret: false,
                visible_if: None,
                constraint: None,
                list: None,
                computed: None,
                policy: Default::default(),
                computed_overridable: false,
            });
        }

        if !form
            .questions
            .iter()
            .any(|q| q.id == token_question_id(&route.server_id))
        {
            form.questions.push(qa_spec::QuestionSpec {
                id: token_question_id(&route.server_id),
                kind: qa_spec::QuestionType::String,
                title: format!("MCP server '{label}' credential"),
                title_i18n: None,
                description: Some(format!(
                    "Sent as the `{}` header. Leave blank if the credential is \
                     already provisioned for this tenant.",
                    route.auth_header_name.as_deref().unwrap_or("Authorization")
                )),
                description_i18n: None,
                required: false,
                choices: None,
                default_value: None,
                secret: true,
                visible_if: None,
                constraint: None,
                list: None,
                computed: None,
                policy: Default::default(),
                computed_overridable: false,
            });
        }
    }
    form
}

/// Question id for an MCP server's host override, carrying `server_id`
/// verbatim.
#[must_use]
pub fn url_question_id(server_id: &str) -> String {
    format!("{MCP_URL_PREFIX}{server_id}")
}

/// Recover the server id from a URL question id.
#[must_use]
pub fn server_id_from_url_question(question_id: &str) -> Option<&str> {
    question_id.strip_prefix(MCP_URL_PREFIX)
}

/// Write every MCP credential the wizard collected to the URI the runner reads.
///
/// Separate from `qa::persist::persist_qa_secrets` on purpose: that writes
/// `secrets://<wizard-env>/<tenant>/<team>/<provider>/<canonical-key>`, and for
/// MCP all three of the env, the provider segment, and the key canonicalization
/// are wrong. See the module doc.
///
/// Returns the server ids written. An empty or whitespace-only answer is
/// skipped, so leaving the prompt blank keeps an admin-provisioned credential
/// untouched rather than clobbering it with an empty secret.
pub async fn persist_mcp_secrets(
    store: &greentic_secrets_lib::DevStore,
    tenant: &str,
    team: Option<&str>,
    config: &serde_json::Value,
) -> anyhow::Result<Vec<String>> {
    let Some(map) = config.as_object() else {
        return Ok(Vec::new());
    };

    let mut entries = Vec::new();
    let mut written = Vec::new();

    for (key, value) in map {
        let Some(server_id) = server_id_from_token_question(key) else {
            continue;
        };
        let text = value.as_str().unwrap_or_default().trim();
        if text.is_empty() {
            continue;
        }

        let uri = mcp_secret_uri(tenant, team, server_id);
        tracing::info!(
            uri = %uri,
            value_len = text.len(),
            server_id,
            "setup secret WRITE (mcp)"
        );
        entries.push(greentic_secrets_lib::SeedEntry {
            uri,
            format: greentic_secrets_lib::SecretFormat::Text,
            value: greentic_secrets_lib::SeedValue::Text {
                text: text.to_string(),
            },
            description: Some(format!("MCP credential for server {server_id}")),
        });
        written.push(server_id.to_string());
    }

    if entries.is_empty() {
        return Ok(written);
    }

    let report = greentic_secrets_lib::apply_seed(
        store,
        &greentic_secrets_lib::SeedDoc { entries },
        greentic_secrets_lib::ApplyOptions::default(),
    )
    .await;
    if !report.failed.is_empty() {
        anyhow::bail!(
            "failed to persist {} MCP credential(s): {:?}",
            report.failed.len(),
            report.failed
        );
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn pack_with_routes(dir: &std::path::Path, routes_json: &str) -> std::path::PathBuf {
        let path = dir.join("t.gtpack");
        let file = std::fs::File::create(&path).expect("create pack");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(MCP_ROUTES_ENTRY, zip::write::SimpleFileOptions::default())
            .expect("entry");
        zip.write_all(routes_json.as_bytes()).expect("write");
        zip.finish().expect("finish");
        path
    }

    /// The URI must be byte-for-byte what the runner reads. Every segment here
    /// is load-bearing: the env is pinned to `default` and NOT the wizard env,
    /// the absent team becomes `_`, and the hyphenated UUID survives verbatim.
    #[test]
    fn the_secret_uri_matches_what_the_runner_reads() {
        assert_eq!(
            mcp_secret_uri("acme", None, "ff308b9c-951a-40b8-acea-f62cdd19c8f3"),
            "secrets://default/acme/_/mcp/ff308b9c-951a-40b8-acea-f62cdd19c8f3"
        );
    }

    /// A team-scoped deployment writes under that team, not `_`.
    #[test]
    fn a_team_scoped_uri_carries_the_team() {
        assert_eq!(
            mcp_secret_uri("acme", Some("payments"), "srv-1"),
            "secrets://default/acme/payments/mcp/srv-1"
        );
    }

    /// The failure this module exists to prevent: canonicalizing the server id
    /// would write a URI the runner never reads, silently.
    #[test]
    fn the_server_id_is_never_canonicalized() {
        let uri = mcp_secret_uri("acme", None, "FF308B9C-951a-40b8");
        assert!(
            uri.ends_with("/mcp/FF308B9C-951a-40b8"),
            "server id must survive verbatim — case and hyphens included; got {uri}"
        );
        assert!(
            !uri.contains("ff308b9c_951a"),
            "canonicalization would corrupt the id and resolve nothing; got {uri}"
        );
    }

    #[test]
    fn routes_are_read_from_the_pack_sidecar() {
        let dir = tempfile::tempdir().expect("tmp");
        let pack = pack_with_routes(
            dir.path(),
            r#"[{"server_id":"srv-1","name":"demo","transport":"http",
                 "transport_url":"https://example.test/mcp",
                 "auth_header_name":"Authorization"}]"#,
        );

        let routes = routes_from_pack(&pack);

        assert_eq!(routes.len(), 1, "one route expected, got {routes:?}");
        assert_eq!(routes[0].server_id, "srv-1");
        assert_eq!(
            routes[0].transport_url.as_deref(),
            Some("https://example.test/mcp")
        );
        assert!(routes[0].is_http());
    }

    /// Fail-soft: a pack with no sidecar, or a damaged one, must still set up.
    #[test]
    fn a_pack_without_a_sidecar_yields_no_routes() {
        let dir = tempfile::tempdir().expect("tmp");
        let pack = pack_with_routes(dir.path(), "not json at all");
        assert!(routes_from_pack(&pack).is_empty());
        assert!(routes_from_pack(&dir.path().join("missing.gtpack")).is_empty());
    }

    /// Regression: a pack whose ONLY setup surface is its MCP servers — no
    /// `setup.yaml`, no `qa/*.json`, no `secret-requirements.json` — must still
    /// produce a form. The meridian demo bundle is exactly that pack, and
    /// augmenting before the empty-form fallback silently dropped its
    /// questions.
    #[test]
    fn a_pack_whose_only_surface_is_mcp_still_gets_questions() {
        let dir = tempfile::tempdir().expect("tmp");
        let pack = pack_with_routes(
            dir.path(),
            r#"[{"server_id":"ff308b9c-951a-40b8","name":"insurance demo",
                 "transport":"http","transport_url":"https://example.test/mcp",
                 "auth_header_name":"Authorization"}]"#,
        );

        let form = crate::setup_to_formspec::pack_to_form_spec(&pack, "meridian-insurance")
            .expect("a pack carrying MCP routes must yield a form");

        let ids: Vec<&str> = form.questions.iter().map(|q| q.id.as_str()).collect();
        assert!(
            ids.contains(&"mcp_token__ff308b9c-951a-40b8"),
            "credential question missing; got {ids:?}"
        );
        assert!(
            ids.contains(&"mcp_url__ff308b9c-951a-40b8"),
            "url question missing; got {ids:?}"
        );

        let token = form
            .questions
            .iter()
            .find(|q| q.id == "mcp_token__ff308b9c-951a-40b8")
            .expect("token question");
        assert!(token.secret, "the credential must be marked secret");
        assert!(
            !token.required,
            "a bundle with several MCP servers must not become a mandatory \
             interrogation, and an admin-provisioned tenant must still pass"
        );

        let url = form
            .questions
            .iter()
            .find(|q| q.id == "mcp_url__ff308b9c-951a-40b8")
            .expect("url question");
        assert_eq!(
            url.default_value.as_deref(),
            Some("https://example.test/mcp"),
            "the sidecar's own URL must pre-fill the answer"
        );
    }

    #[test]
    fn a_local_wasm_route_needs_no_http_credential() {
        let route = PackMcpRoute {
            server_id: "s".into(),
            name: None,
            transport: "local-wasm".into(),
            transport_url: None,
            auth_header_name: None,
        };
        assert!(!route.is_http());
    }

    /// The write must land at the URI the runner reads, with the hyphenated
    /// UUID intact. This is the whole point of the module: a canonicalized or
    /// wizard-env URI would look successful and resolve nothing.
    #[tokio::test]
    async fn a_collected_credential_lands_where_the_runner_reads_it() {
        let iso = tempfile::tempdir().expect("iso");
        let _override = crate::secrets::test_support::StoreOverride::in_dir(iso.path());
        let temp = tempfile::tempdir().expect("tempdir");
        let store =
            crate::secrets::open_dev_store_for_env(temp.path(), "local").expect("open store");

        let server = "ff308b9c-951a-40b8-acea-f62cdd19c8f3";
        let config = serde_json::json!({
            token_question_id(server): "s3cret",
            token_question_id("blank-server"): "   ",
            "unrelated": "ignored",
        });

        let written = persist_mcp_secrets(&store, "default", None, &config)
            .await
            .expect("persist");

        assert_eq!(
            written,
            vec![server.to_string()],
            "a blank answer must be skipped, not written as an empty secret"
        );

        let uri = mcp_secret_uri("default", None, server);
        assert_eq!(uri, format!("secrets://default/default/_/mcp/{server}"));
        let stored = greentic_secrets_lib::SecretsStore::get(&store, &uri)
            .await
            .expect("secret readable at the runner's URI");
        assert_eq!(String::from_utf8_lossy(&stored), "s3cret");
    }

    #[test]
    fn a_token_question_id_round_trips_the_server_id() {
        let id = token_question_id("ff308b9c-951a-40b8");
        assert_eq!(
            server_id_from_token_question(&id),
            Some("ff308b9c-951a-40b8")
        );
        assert_eq!(server_id_from_token_question("unrelated"), None);
    }
}
