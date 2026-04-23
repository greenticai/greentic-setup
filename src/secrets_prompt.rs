//! Interactive prompts for missing pack secrets.
//!
//! Used by the setup CLI to fill values that are required by the
//! discovered packs but absent from both the dev store and `seeds.yaml`.
//! Secret-shaped keys (token, password, api_key, ...) prompt with
//! no-echo via `rpassword`; plain configuration keys prompt with
//! `dialoguer` (echoed).
//!
//! All prompted values are written via
//! [`crate::secrets::SecretsSetup::set_secret_text`], which persists
//! through the AES-256-GCM-encrypted backend when a passphrase has
//! been resolved (see `crate::secrets::set_global_key_provider`).

use anyhow::{Context, Result};
use dialoguer::Input;
use dialoguer::theme::ColorfulTheme;

use crate::secrets::MissingKey;

/// Prompt the user for a single missing secret value.
///
/// Returns the entered string. Empty input is rejected for required
/// keys; for non-required keys, an empty string is returned and the
/// caller may decide to skip persisting it.
pub fn prompt_value(missing: &MissingKey) -> Result<String> {
    let label = match (&missing.description, missing.required) {
        (Some(d), true) => format!("{} ({}) [required]", missing.key, d),
        (Some(d), false) => format!("{} ({})", missing.key, d),
        (None, true) => format!("{} [required]", missing.key),
        (None, false) => missing.key.clone(),
    };

    if missing.looks_secret {
        let value = rpassword::prompt_password(format!("{label}: "))
            .context("reading secret value from terminal")?;
        Ok(value)
    } else {
        let prompt = Input::<String>::with_theme(&ColorfulTheme::default())
            .with_prompt(label)
            .allow_empty(!missing.required)
            .interact_text()
            .context("reading config value from terminal")?;
        Ok(prompt)
    }
}

/// Prompt for every missing key in order. Stops at the first error
/// (e.g. user Ctrl+C). Skips non-required keys whose user-entered
/// value is empty.
pub fn prompt_missing_keys(missing: &[MissingKey]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(missing.len());
    for key in missing {
        let value = prompt_value(key)?;
        if value.is_empty() && !key.required {
            continue;
        }
        out.push((key.uri.clone(), value));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    // The interactive prompts are covered by integration tests that
    // pipe stdin into the real binary. Pure-Rust unit tests here are
    // limited to compile-time guarantees (e.g. that the public API
    // signatures don't drift). The real contract — "no passphrase
    // appears in argv or logs" — is enforced by tests/clap_help_check.rs
    // and tests/passphrase_setup.rs.
}
