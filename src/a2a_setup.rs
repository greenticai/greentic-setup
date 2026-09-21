//! Collect an A2A agent's credential at setup time.
//!
//! The twin of [`crate::mcp_setup`]. greentic-designer writes the non-secret
//! half of every A2A agent a worker binds into `assets/a2a-routes.json`; the
//! credential is deliberately NOT in the artefact. greentic-runner reads it at
//! call time from `secrets://default/<tenant>/<auth_team or _>/a2a/<agent_id>`.
//!
//! A bundle booted by `gtc start` has no admin to have written that secret, so
//! this module asks the operator for it and writes it where the runner reads.
//! Contract: greentic-designer
//! `docs/superpowers/specs/2026-09-21-a2a-agent-registry-contract.md` §4 and §6.
//!
//! Two deliberate differences from the MCP module:
//!
//! - **`requires_auth` is the trigger, not `auth_header_name`.** A null
//!   header name means `Authorization: Bearer`, which still needs a token.
//! - **The team segment goes through
//!   [`greentic_secrets_lib::normalize_team`].** `mcp_secret_uri` writes the
//!   team verbatim, so a literal `default` team lands under `…/default/…`
//!   while the runtime reads `_`. That gap is not copied here.
//!
//! As for MCP, the agent id (a hyphenated UUID) is written VERBATIM — never
//! through `canonical_secret_name`, which would turn `-` into `_` and produce
//! a URI that looks written and resolves nothing. There is no `a2a_url__`
//! question in v1: nothing would consume it.

use std::path::Path;

use serde::Deserialize;

/// Pack entry the designer writes and the runner reads.
pub const A2A_ROUTES_ENTRY: &str = "assets/a2a-routes.json";

/// Env segment for A2A secrets, pinned to `default` regardless of the wizard's
/// environment, matching the runner's reader.
pub const A2A_ENV_SEGMENT: &str = "default";

/// Question-id prefix for an A2A agent's credential. The agent id follows
/// VERBATIM — never canonicalized.
pub const A2A_TOKEN_PREFIX: &str = "a2a_token__";

/// One route record from the pack sidecar. Only the fields setup needs;
/// unknown fields are ignored.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct PackA2aRoute {
    pub agent_id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub auth_header_name: Option<String>,
    #[serde(default)]
    pub auth_team: Option<String>,
    /// Whether a token is stored for this agent. A missing field reads as
    /// `false`. Named without `credential`/`secret`/`token` because the
    /// designer's sidecar ratchet rejects those substrings.
    #[serde(default)]
    pub requires_auth: bool,
}

/// Read the A2A route sidecar out of a `.gtpack`.
///
/// Every failure yields an empty list: a pack built before the feature, or
/// with a damaged sidecar, must still set up. A malformed sidecar is warned.
#[must_use]
pub fn routes_from_pack(pack_path: &Path) -> Vec<PackA2aRoute> {
    let Ok(file) = std::fs::File::open(pack_path) else {
        return Vec::new();
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return Vec::new();
    };
    let Ok(entry) = archive.by_name(A2A_ROUTES_ENTRY) else {
        return Vec::new();
    };
    match serde_json::from_reader::<_, Vec<PackA2aRoute>>(entry) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::warn!(
                error = %error,
                entry = A2A_ROUTES_ENTRY,
                "ignoring malformed A2A route sidecar"
            );
            Vec::new()
        }
    }
}

/// The secret URI an A2A credential must be written to.
///
/// The team is normalised (`None`, blank, `_` and `default` all become `_`);
/// tenant and agent id are emitted verbatim.
#[must_use]
pub fn a2a_secret_uri(tenant: &str, team: Option<&str>, agent_id: &str) -> String {
    let team = greentic_secrets_lib::normalize_team(team)
        .unwrap_or_else(|| greentic_secrets_lib::TEAM_PLACEHOLDER.to_string());
    format!("secrets://{A2A_ENV_SEGMENT}/{tenant}/{team}/a2a/{agent_id}")
}

/// The team segment a route's credential is written under: the row's
/// `auth_team` when set, else the configured team, else `_` — normalised.
#[must_use]
pub fn secret_team(route: &PackA2aRoute, configured_team: Option<&str>) -> String {
    let chosen = route
        .auth_team
        .as_deref()
        .filter(|team| !team.trim().is_empty())
        .or(configured_team);
    greentic_secrets_lib::normalize_team(chosen)
        .unwrap_or_else(|| greentic_secrets_lib::TEAM_PLACEHOLDER.to_string())
}

/// Question id carrying `agent_id` verbatim.
#[must_use]
pub fn token_question_id(agent_id: &str) -> String {
    format!("{A2A_TOKEN_PREFIX}{agent_id}")
}

/// Recover the agent id from a token question id.
#[must_use]
pub fn agent_id_from_token_question(question_id: &str) -> Option<&str> {
    question_id.strip_prefix(A2A_TOKEN_PREFIX)
}

/// Append one OPTIONAL secret question per agent whose route carries a
/// credential, so every wizard path — prompt, `--answers`, `--emit-answers`,
/// `--non-interactive` and the UI — collects it like any other pack secret.
///
/// Optional, because a tenant whose credential is already provisioned must
/// keep working untouched, and several agents must not become a mandatory
/// interrogation.
pub fn augment_with_a2a_routes(mut form: qa_spec::FormSpec, pack_path: &Path) -> qa_spec::FormSpec {
    for route in routes_from_pack(pack_path)
        .into_iter()
        .filter(|route| route.requires_auth)
    {
        let id = token_question_id(&route.agent_id);
        if form.questions.iter().any(|q| q.id == id) {
            continue;
        }
        let label = route.name.clone().unwrap_or_else(|| route.agent_id.clone());
        let header = match route.auth_header_name.as_deref() {
            Some(name) if !name.trim().is_empty() => format!("`{name}` header"),
            _ => "`Authorization: Bearer` header".to_string(),
        };
        form.questions.push(qa_spec::QuestionSpec {
            id,
            kind: qa_spec::QuestionType::String,
            title: format!("A2A agent '{label}' credential"),
            title_i18n: None,
            description: Some(format!(
                "Sent in the {header}. Leave blank if the credential is already \
                 provisioned for this tenant."
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
    form
}

/// Write every A2A credential the wizard collected to the URI the runner reads.
///
/// Separate from the universal `qa::persist` write for the same reason as MCP:
/// that one canonicalizes the key and uses the wizard's env. The pack (when
/// given) supplies each agent's `auth_team`; an answer for an agent the
/// sidecar does not list is written under the configured team.
///
/// Returns the agent ids written. A blank answer is skipped, so leaving the
/// prompt empty never clobbers a provisioned credential.
pub async fn persist_a2a_secrets(
    store: &greentic_secrets_lib::DevStore,
    tenant: &str,
    team: Option<&str>,
    config: &serde_json::Value,
    pack_path: Option<&Path>,
) -> anyhow::Result<Vec<String>> {
    let Some(map) = config.as_object() else {
        return Ok(Vec::new());
    };
    let routes = pack_path.map(routes_from_pack).unwrap_or_default();

    let mut entries = Vec::new();
    let mut written = Vec::new();

    for (key, value) in map {
        let Some(agent_id) = agent_id_from_token_question(key) else {
            continue;
        };
        let text = value.as_str().unwrap_or_default().trim();
        if text.is_empty() {
            continue;
        }

        let team_segment = match routes.iter().find(|route| route.agent_id == agent_id) {
            Some(route) => secret_team(route, team),
            None => greentic_secrets_lib::normalize_team(team)
                .unwrap_or_else(|| greentic_secrets_lib::TEAM_PLACEHOLDER.to_string()),
        };
        let uri = a2a_secret_uri(tenant, Some(&team_segment), agent_id);
        tracing::info!(
            uri = %uri,
            value_len = text.len(),
            agent_id,
            "setup secret WRITE (a2a)"
        );
        entries.push(greentic_secrets_lib::SeedEntry {
            uri,
            format: greentic_secrets_lib::SecretFormat::Text,
            value: greentic_secrets_lib::SeedValue::Text {
                text: text.to_string(),
            },
            description: Some(format!("A2A credential for agent {agent_id}")),
        });
        written.push(agent_id.to_string());
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
            "failed to persist {} A2A credential(s): {:?}",
            report.failed.len(),
            report.failed
        );
    }
    Ok(written)
}

#[cfg(test)]
mod tests;
