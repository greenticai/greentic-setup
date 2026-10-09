use super::*;
use std::io::Write as _;

const AGENT: &str = "8d2f4c1e-7a0b-4f3e-9c55-1b2a3c4d5e6f";

fn pack_with_routes(dir: &std::path::Path, routes_json: &str) -> std::path::PathBuf {
    let path = dir.join("t.gtpack");
    let file = std::fs::File::create(&path).expect("create pack");
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file(A2A_ROUTES_ENTRY, zip::write::SimpleFileOptions::default())
        .expect("entry");
    zip.write_all(routes_json.as_bytes()).expect("write");
    zip.finish().expect("finish");
    path
}

fn empty_form() -> qa_spec::FormSpec {
    qa_spec::FormSpec {
        id: "t-setup".to_string(),
        title: "T setup".to_string(),
        version: "1.0.0".to_string(),
        description: None,
        presentation: None,
        progress_policy: None,
        secrets_policy: None,
        store: vec![],
        validations: vec![],
        includes: vec![],
        questions: vec![],
    }
}

fn route(agent_id: &str, auth_team: Option<&str>, requires_auth: bool) -> PackA2aRoute {
    PackA2aRoute {
        agent_id: agent_id.to_string(),
        name: None,
        base_url: "https://agent.example.test".to_string(),
        auth_header_name: None,
        auth_team: auth_team.map(str::to_string),
        requires_auth,
    }
}

/// The URI must be byte-for-byte what the runner reads: env pinned to
/// `default`, category `a2a`, an absent team as `_`, the id verbatim.
#[test]
fn the_secret_uri_matches_what_the_runner_reads() {
    assert_eq!(
        a2a_secret_uri("acme", None, AGENT),
        format!("secrets://default/acme/_/a2a/{AGENT}")
    );
}

#[test]
fn a_team_scoped_uri_carries_the_team() {
    assert_eq!(
        a2a_secret_uri("acme", Some("payments"), "agent-1"),
        "secrets://default/acme/payments/a2a/agent-1"
    );
}

/// The gap `mcp_secret_uri` still has: a literal `default` team (and a blank
/// one) is the wildcard, and the runtime reads it as `_`.
#[test]
fn a_literal_default_team_is_normalised_to_the_wildcard() {
    assert_eq!(
        a2a_secret_uri("acme", Some("default"), "agent-1"),
        "secrets://default/acme/_/a2a/agent-1"
    );
    assert_eq!(
        a2a_secret_uri("acme", Some("  "), "agent-1"),
        "secrets://default/acme/_/a2a/agent-1"
    );
}

#[test]
fn the_agent_id_is_never_canonicalized() {
    let uri = a2a_secret_uri("acme", None, "8D2F4C1E-7a0b-4f3e");
    assert!(
        uri.ends_with("/a2a/8D2F4C1E-7a0b-4f3e"),
        "agent id must survive verbatim — case and hyphens included; got {uri}"
    );
    assert!(!uri.contains("8d2f4c1e_7a0b"), "canonicalized id: {uri}");
}

/// The row's `auth_team` wins over the deployment's configured team, because
/// that is the scope the credential was provisioned under.
#[test]
fn the_rows_auth_team_is_honoured_over_the_configured_team() {
    assert_eq!(
        secret_team(&route(AGENT, Some("research"), true), Some("payments")),
        "research"
    );
    assert_eq!(
        secret_team(&route(AGENT, None, true), Some("payments")),
        "payments"
    );
    assert_eq!(secret_team(&route(AGENT, Some(""), true), None), "_");
    assert_eq!(secret_team(&route(AGENT, Some("default"), true), None), "_");
}

#[test]
fn routes_are_read_from_the_pack_sidecar_ignoring_unknown_fields() {
    let dir = tempfile::tempdir().expect("tmp");
    let pack = pack_with_routes(
        dir.path(),
        &format!(
            r#"[{{"agent_id":"{AGENT}","name":"Recipe agent",
                 "base_url":"https://agent.example.test","auth_header_name":null,
                 "auth_team":"research","requires_auth":true,"future_field":42}}]"#
        ),
    );

    let routes = routes_from_pack(&pack);

    assert_eq!(routes.len(), 1, "one route expected, got {routes:?}");
    assert_eq!(routes[0].agent_id, AGENT);
    assert_eq!(routes[0].auth_team.as_deref(), Some("research"));
    assert!(routes[0].requires_auth);
}

#[test]
fn a_missing_or_malformed_sidecar_yields_no_routes_and_no_questions() {
    let dir = tempfile::tempdir().expect("tmp");
    let pack = pack_with_routes(dir.path(), "not json at all");
    assert!(routes_from_pack(&pack).is_empty());
    assert!(routes_from_pack(&dir.path().join("missing.gtpack")).is_empty());

    let form = augment_with_a2a_routes(empty_form(), &pack);
    assert!(form.questions.is_empty(), "got {:?}", form.questions);
}

/// `requires_auth` — not `auth_header_name` — is the trigger.
#[test]
fn only_rows_that_carry_a_credential_get_a_question() {
    let dir = tempfile::tempdir().expect("tmp");
    let pack = pack_with_routes(
        dir.path(),
        r#"[{"agent_id":"public-agent","base_url":"https://a.test",
             "auth_header_name":"X-Api-Key","requires_auth":false},
            {"agent_id":"bearer-agent","base_url":"https://b.test",
             "auth_header_name":null,"requires_auth":true}]"#,
    );

    let form = augment_with_a2a_routes(empty_form(), &pack);
    let ids: Vec<&str> = form.questions.iter().map(|q| q.id.as_str()).collect();

    assert_eq!(ids, vec!["a2a_token__bearer-agent"], "got {ids:?}");
    let token = &form.questions[0];
    assert!(token.secret, "the credential must be marked secret");
    assert!(!token.required, "the credential question must be optional");
    assert!(
        token
            .description
            .as_deref()
            .is_some_and(|d| d.contains("Authorization: Bearer")),
        "a null header name means Bearer; got {:?}",
        token.description
    );
    assert!(
        !ids.iter().any(|id| id.starts_with("a2a_url__")),
        "no URL question in v1"
    );
}

#[test]
fn a_named_header_is_what_the_description_names() {
    let dir = tempfile::tempdir().expect("tmp");
    let pack = pack_with_routes(
        dir.path(),
        r#"[{"agent_id":"a","base_url":"https://a.test",
             "auth_header_name":"X-Api-Key","requires_auth":true}]"#,
    );
    let form = augment_with_a2a_routes(empty_form(), &pack);
    let description = form.questions[0].description.as_deref().unwrap_or_default();
    assert!(description.contains("`X-Api-Key`"), "got {description}");
    assert!(!description.contains("Bearer"), "got {description}");
}

#[test]
fn an_existing_question_is_not_duplicated() {
    let dir = tempfile::tempdir().expect("tmp");
    let pack = pack_with_routes(
        dir.path(),
        r#"[{"agent_id":"a","base_url":"https://a.test","requires_auth":true}]"#,
    );
    let once = augment_with_a2a_routes(empty_form(), &pack);
    let twice = augment_with_a2a_routes(once, &pack);
    assert_eq!(twice.questions.len(), 1);
}

/// A pack whose ONLY setup surface is its A2A agents must still produce a form.
#[test]
fn a_pack_whose_only_surface_is_a2a_still_gets_its_question() {
    let dir = tempfile::tempdir().expect("tmp");
    let pack = pack_with_routes(
        dir.path(),
        &format!(r#"[{{"agent_id":"{AGENT}","base_url":"https://a.test","requires_auth":true}}]"#),
    );

    let form = crate::setup_to_formspec::pack_to_form_spec(&pack, "recipes")
        .expect("a pack carrying A2A routes must yield a form");

    let ids: Vec<&str> = form.questions.iter().map(|q| q.id.as_str()).collect();
    assert!(
        ids.contains(&format!("a2a_token__{AGENT}").as_str()),
        "credential question missing; got {ids:?}"
    );
}

/// The write lands at the verbatim URI under the row's team, and a blank
/// answer is skipped rather than clobbering a provisioned credential.
#[tokio::test]
async fn a_collected_credential_lands_where_the_runner_reads_it() {
    let iso = tempfile::tempdir().expect("iso");
    let _override = crate::secrets::test_support::StoreOverride::in_dir(iso.path());
    let temp = tempfile::tempdir().expect("tempdir");
    let store = crate::secrets::open_dev_store_for_env(temp.path(), "local").expect("open store");
    let pack = pack_with_routes(
        temp.path(),
        &format!(
            r#"[{{"agent_id":"{AGENT}","base_url":"https://a.test",
                 "auth_team":"research","requires_auth":true}}]"#
        ),
    );

    let config = serde_json::json!({
        token_question_id(AGENT): "s3cret",
        token_question_id("unlisted-agent"): "other",
        token_question_id("blank-agent"): "   ",
        "mcp_token__srv": "not ours",
    });

    let mut written = persist_a2a_secrets(&store, "acme", Some("default"), &config, Some(&pack))
        .await
        .expect("persist");
    written.sort();
    assert_eq!(
        written,
        vec![AGENT.to_string(), "unlisted-agent".to_string()]
    );

    let uri = format!("secrets://default/acme/research/a2a/{AGENT}");
    let stored = greentic_secrets_lib::SecretsStore::get(&store, &uri)
        .await
        .expect("secret readable at the runner's URI");
    assert_eq!(String::from_utf8_lossy(&stored), "s3cret");

    // No sidecar row: the configured team, here a literal `default`, normalised.
    let stored = greentic_secrets_lib::SecretsStore::get(
        &store,
        "secrets://default/acme/_/a2a/unlisted-agent",
    )
    .await
    .expect("unlisted agent under the normalised configured team");
    assert_eq!(String::from_utf8_lossy(&stored), "other");
}

#[test]
fn a_token_question_id_round_trips_the_agent_id() {
    let id = token_question_id(AGENT);
    assert_eq!(id, format!("a2a_token__{AGENT}"));
    assert_eq!(agent_id_from_token_question(&id), Some(AGENT));
    assert_eq!(agent_id_from_token_question("mcp_token__x"), None);
}

/// The path every wizard takes (`engine::executors`) must reach the A2A write,
/// and must report the answer as saved.
#[tokio::test]
async fn the_wizard_persist_path_writes_the_a2a_credential() {
    let iso = tempfile::tempdir().expect("iso");
    let _override = crate::secrets::test_support::StoreOverride::in_dir(iso.path());
    let temp = tempfile::tempdir().expect("tempdir");
    let pack = pack_with_routes(
        temp.path(),
        &format!(r#"[{{"agent_id":"{AGENT}","base_url":"https://a.test","requires_auth":true}}]"#),
    );
    let config = serde_json::json!({ token_question_id(AGENT): "s3cret" });

    let saved = crate::qa::persist::persist_all_config_as_secrets(
        temp.path(),
        "local",
        "acme",
        None,
        "recipes",
        &config,
        Some(&pack),
    )
    .await
    .expect("persist all");
    assert!(
        saved.contains(&token_question_id(AGENT)),
        "a2a answer not reported as saved; got {saved:?}"
    );

    let store = crate::secrets::open_dev_store_for_env(temp.path(), "local").expect("open store");
    let stored = greentic_secrets_lib::SecretsStore::get(
        &store,
        &format!("secrets://default/acme/_/a2a/{AGENT}"),
    )
    .await
    .expect("secret readable at the runner's URI");
    assert_eq!(String::from_utf8_lossy(&stored), "s3cret");
}

/// A row with no `requires_auth` reads as `false` — including one carrying the
/// first-draft name `has_credential`, which is just an unknown field now.
#[test]
fn a_row_without_requires_auth_asks_for_nothing() {
    let dir = tempfile::tempdir().expect("tmp");
    let pack = pack_with_routes(
        dir.path(),
        r#"[{"agent_id":"a","base_url":"https://a.test"},
            {"agent_id":"b","base_url":"https://b.test","has_credential":true}]"#,
    );
    let routes = routes_from_pack(&pack);
    assert_eq!(
        routes.len(),
        2,
        "both rows must still parse; got {routes:?}"
    );
    assert!(routes.iter().all(|route| !route.requires_auth));
    assert!(
        augment_with_a2a_routes(empty_form(), &pack)
            .questions
            .is_empty()
    );
}
