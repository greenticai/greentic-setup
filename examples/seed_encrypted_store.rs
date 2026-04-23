//! Test helper: seeds an encrypted v1 dev secrets store at a given
//! bundle root using a passphrase read from `stdin`. Used by the
//! end-to-end smoke test to create encrypted bundles without needing
//! a TTY.
//!
//! Usage:
//!
//! ```bash
//! echo "my-passphrase-12chars" | \
//!   cargo run --example seed_encrypted_store -- <bundle_root>
//! ```
//!
//! After running, `<bundle_root>/.greentic/dev/.dev.secrets.env` is
//! a real encrypted v1 file plus the `.encrypted-marker` sidecar, ready
//! for unlock / wrong-passphrase / downgrade-guard tests.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use greentic_secrets_lib::SecretsStore;
use greentic_secrets_passphrase::{SecretString, derive_master_key, random_salt};
use greentic_setup::secrets::ensure_path;
use secrets_provider_dev::PassphraseKeyProvider;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let bundle_root: PathBuf = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("usage: seed_encrypted_store <bundle_root>"))?
        .into();

    let mut line = String::new();
    BufReader::new(std::io::stdin())
        .read_line(&mut line)
        .context("reading passphrase from stdin")?;
    let passphrase_str = line.trim_end_matches(['\r', '\n']).to_string();
    if passphrase_str.is_empty() {
        bail!("empty passphrase on stdin");
    }
    let passphrase = SecretString::from(passphrase_str);

    let salt = random_salt();
    let master_key =
        derive_master_key(&passphrase, &salt).map_err(|e| anyhow!("derive_master_key: {e}"))?;
    drop(passphrase);

    let provider = Arc::new(PassphraseKeyProvider::new(master_key, salt));
    greentic_setup::secrets::set_global_key_provider(provider, false);

    let store_path = ensure_path(&bundle_root)?;
    let store = greentic_setup::secrets::open_dev_store(&bundle_root)?;

    // Write a single sentinel secret so the encrypted file body has
    // real content (not just an empty state). Use the same canonical
    // URI shape the runtime uses.
    let uri = greentic_setup::canonical_secret_uri(
        "dev",
        "demo",
        Some("default"),
        "smoke-test",
        "SENTINEL_KEY",
    );
    store
        .put(
            &uri,
            greentic_secrets_lib::SecretFormat::Text,
            b"sentinel-value-not-secret",
        )
        .await
        .map_err(|e| anyhow!("put: {e}"))?;

    println!("OK: wrote encrypted store at {}", store_path.display());
    Ok(())
}
