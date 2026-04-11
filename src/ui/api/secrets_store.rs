//! Low-level secrets store operations for the dashboard API.
//!
//! All secret values that pass through this module are held in
//! `zeroize::Zeroizing<String>` so they are scrubbed from memory on drop.
//! No raw value is ever logged — audit log entries record only
//! `provider_id`, `key`, `scope`, and `action`.

use std::path::Path;

use anyhow::{Context, Result};
use greentic_secrets_lib::{SecretFormat, SecretsStore};
use tracing::info;
use zeroize::Zeroizing;

use crate::ui::state::SecretEntry;

// ── Masking ───────────────────────────────────────────────────────────────────

/// Produce a masked representation: "••••" + last 4 chars, or just "••••"
/// if the value is shorter than 8 characters.
pub fn mask_value(raw: &[u8]) -> String {
    // Work only with the UTF-8 view; fall back to "••••" for non-UTF-8.
    let text = match std::str::from_utf8(raw) {
        Ok(s) => s,
        Err(_) => return "\u{2022}\u{2022}\u{2022}\u{2022}".to_string(),
    };
    if text.len() < 8 {
        "\u{2022}\u{2022}\u{2022}\u{2022}".to_string()
    } else {
        let tail: String = text.chars().rev().take(4).collect::<String>().chars().rev().collect();
        format!("\u{2022}\u{2022}\u{2022}\u{2022}{tail}")
    }
}

// ── List secrets ──────────────────────────────────────────────────────────────

/// List all secrets for a given scope, reading each provider's questions
/// from the FormSpec and probing the store for their values.
///
/// Values are returned masked — never in raw form.
pub async fn list_secrets(
    bundle_root: &Path,
    provider_forms: &[crate::ui::state::ProviderFormData],
    scope: &crate::ui::state::ScopeKey,
) -> Result<Vec<SecretEntry>> {
    let store = crate::secrets::open_dev_store(bundle_root)
        .context("ui.error.secrets_list_failed")?;

    let mut entries = Vec::new();

    for pf in provider_forms {
        for q in &pf.form_spec.questions {
            let uri = crate::canonical_secret_uri(
                &scope.env,
                &scope.tenant,
                Some(scope.team.as_str()),
                &pf.provider_id,
                &q.id,
            );

            let (has_value, masked_value) = match store.get(&uri).await {
                Ok(bytes) if !bytes.is_empty() => (true, mask_value(&bytes)),
                _ => (false, "\u{2022}\u{2022}\u{2022}\u{2022}".to_string()),
            };

            entries.push(SecretEntry {
                provider_id: pf.provider_id.clone(),
                key: q.id.clone(),
                uri: uri.clone(),
                masked_value,
                has_value,
            });
        }
    }

    Ok(entries)
}

// ── Reveal secret ─────────────────────────────────────────────────────────────

/// Read the raw secret value for a single URI.
///
/// The returned value is wrapped in `Zeroizing<String>` so it is scrubbed
/// from memory when the caller drops it. The caller must not log the value.
///
/// Audit log entry is written here (key + scope, no value).
pub async fn reveal_secret(
    bundle_root: &Path,
    uri: &str,
    provider_id: &str,
    key: &str,
    scope: &crate::ui::state::ScopeKey,
) -> Result<Zeroizing<String>> {
    // Audit trail — no value, only identity fields.
    info!(
        action = "reveal",
        provider_id = %provider_id,
        key = %key,
        tenant = %scope.tenant,
        env = %scope.env,
        team = %scope.team,
        "secret reveal requested"
    );

    let store = crate::secrets::open_dev_store(bundle_root)
        .context("ui.error.secrets_reveal_failed")?;

    let bytes = store
        .get(uri)
        .await
        .with_context(|| format!("failed to read secret {uri}"))?;

    let text = String::from_utf8(bytes).context("secret value is not valid UTF-8")?;
    Ok(Zeroizing::new(text))
}

// ── Write secret ──────────────────────────────────────────────────────────────

/// Write a secret value to the store.
///
/// Audit log: writes action + identity, never the value.
pub async fn write_secret(
    bundle_root: &Path,
    uri: &str,
    value: Zeroizing<String>,
    provider_id: &str,
    key: &str,
    scope: &crate::ui::state::ScopeKey,
) -> Result<()> {
    info!(
        action = "write",
        provider_id = %provider_id,
        key = %key,
        tenant = %scope.tenant,
        env = %scope.env,
        team = %scope.team,
        "secret write requested"
    );

    let store = crate::secrets::open_dev_store(bundle_root)
        .context("ui.error.secrets_update_failed")?;

    store
        .put(uri, SecretFormat::Text, value.as_bytes())
        .await
        .with_context(|| format!("failed to write secret {uri}"))?;

    Ok(())
}

// ── Delete secret ─────────────────────────────────────────────────────────────

/// Delete a secret from the dev store by removing it from the `.env` file.
///
/// The dev store persists as a base64-encoded JSON blob in a `.env` file.
/// Since `DevStore` does not expose a delete method, this function opens
/// the underlying `.env` persistence file, decodes the JSON state, removes
/// the entry matching `uri`, and rewrites the file.
///
/// Audit log: writes action + identity, never the value.
pub fn delete_secret(
    bundle_root: &Path,
    uri: &str,
    provider_id: &str,
    key: &str,
    scope: &crate::ui::state::ScopeKey,
) -> Result<()> {
    info!(
        action = "delete",
        provider_id = %provider_id,
        key = %key,
        tenant = %scope.tenant,
        env = %scope.env,
        team = %scope.team,
        "secret delete requested"
    );

    let store_path = crate::secrets::ensure_path(bundle_root)
        .context("ui.error.secrets_delete_failed")?;

    if !store_path.exists() {
        // Nothing to delete.
        return Ok(());
    }

    delete_from_env_file(&store_path, uri)
        .with_context(|| format!("failed to delete secret {uri} from store"))
}

// ── `.env` file manipulation ──────────────────────────────────────────────────

/// Remove the entry with the given URI from the `.env` persistence file.
///
/// The file format written by `greentic-secrets-provider-dev` is:
/// ```text
/// SECRETS_BACKEND_STATE=<base64-encoded-json>
/// ```
/// The JSON contains `{ "secrets": [{ "key": "<uri>", "versions": [...] }] }`.
///
/// We decode, remove the matching key, re-encode, and write back.
fn delete_from_env_file(env_path: &Path, uri: &str) -> Result<()> {
    use base64::Engine;

    let content = std::fs::read_to_string(env_path)
        .with_context(|| format!("failed to read {}", env_path.display()))?;

    // Parse: find the SECRETS_BACKEND_STATE= line.
    let state_b64 = content
        .lines()
        .find_map(|line| line.strip_prefix("SECRETS_BACKEND_STATE="))
        .context("SECRETS_BACKEND_STATE key not found in store file")?
        .trim()
        .to_string();

    let state_json = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(&state_b64)
        .context("failed to base64-decode store state")?;

    let mut state: serde_json::Value =
        serde_json::from_slice(&state_json).context("failed to parse store JSON")?;

    // Remove the matching secret from the "secrets" array.
    if let Some(secrets) = state.get_mut("secrets").and_then(|v| v.as_array_mut()) {
        secrets.retain(|entry| {
            entry
                .get("key")
                .and_then(|k| k.as_str())
                .map(|k| k != uri)
                .unwrap_or(true)
        });
    }

    // Re-encode and write back.
    let updated_json = serde_json::to_vec(&state).context("failed to re-serialize store JSON")?;
    let updated_b64 =
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(&updated_json);

    let new_content = format!("SECRETS_BACKEND_STATE={updated_b64}\n");
    std::fs::write(env_path, &new_content)
        .with_context(|| format!("failed to write updated store to {}", env_path.display()))?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_value_masks_short_values() {
        assert_eq!(mask_value(b"abc"), "\u{2022}\u{2022}\u{2022}\u{2022}");
        assert_eq!(mask_value(b"1234567"), "\u{2022}\u{2022}\u{2022}\u{2022}");
    }

    #[test]
    fn mask_value_shows_last_four_for_long_values() {
        let masked = mask_value(b"abcdefghij");
        assert!(masked.ends_with("ghij"), "got: {masked}");
        assert!(masked.starts_with("\u{2022}\u{2022}\u{2022}\u{2022}"));
    }

    #[test]
    fn mask_value_handles_non_utf8() {
        assert_eq!(mask_value(&[0xFF, 0xFE]), "\u{2022}\u{2022}\u{2022}\u{2022}");
    }

    #[test]
    fn mask_value_exactly_eight_chars_shows_last_four() {
        // "abcdefgh" — 8 chars: should show last 4 = "efgh"
        let masked = mask_value(b"abcdefgh");
        assert!(masked.ends_with("efgh"), "got: {masked}");
    }
}
