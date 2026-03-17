use aes_gcm_siv::aead::{Aead, KeyInit};
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use anyhow::{Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rand::RngCore;
use rpassword::prompt_password;
use serde_json::{Map as JsonMap, Value};
use sha2::{Digest, Sha256};

const ENCRYPTED_KIND: &str = "aes-256-gcm-siv-v1";
const MARKER_FIELD: &str = "__greentic_encrypted__";
const NONCE_FIELD: &str = "nonce";
const CIPHERTEXT_FIELD: &str = "ciphertext";

pub fn prompt_for_key(action: &str) -> Result<String> {
    let prompt = format!("Answer file key for {action}: ");
    let key = prompt_password(prompt).map_err(|err| anyhow!("read key: {err}"))?;
    if key.is_empty() {
        return Err(anyhow!("key cannot be empty"));
    }
    Ok(key)
}

pub fn has_encrypted_values(value: &Value) -> bool {
    match value {
        Value::Object(map) => is_encrypted_value(map) || map.values().any(has_encrypted_values),
        Value::Array(items) => items.iter().any(has_encrypted_values),
        _ => false,
    }
}

pub fn encrypt_value(value: &Value, key: &str) -> Result<Value> {
    let plaintext = serde_json::to_vec(value)?;
    let derived_key = derive_key(key);
    let cipher =
        Aes256GcmSiv::new_from_slice(&derived_key).map_err(|err| anyhow!("init cipher: {err}"))?;
    let mut nonce_bytes = [0_u8; 12];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|err| anyhow!("encrypt answer value: {err}"))?;
    Ok(serde_json::json!({
        MARKER_FIELD: ENCRYPTED_KIND,
        NONCE_FIELD: STANDARD.encode(nonce_bytes),
        CIPHERTEXT_FIELD: STANDARD.encode(ciphertext),
    }))
}

pub fn decrypt_value(value: &Value, key: &str) -> Result<Value> {
    let map = value
        .as_object()
        .ok_or_else(|| anyhow!("encrypted answer payload must be an object"))?;
    if !is_encrypted_value(map) {
        return Ok(value.clone());
    }
    let nonce_b64 = map
        .get(NONCE_FIELD)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("encrypted answer payload missing nonce"))?;
    let ciphertext_b64 = map
        .get(CIPHERTEXT_FIELD)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("encrypted answer payload missing ciphertext"))?;
    let nonce_bytes = STANDARD
        .decode(nonce_b64)
        .map_err(|err| anyhow!("decode nonce: {err}"))?;
    let ciphertext = STANDARD
        .decode(ciphertext_b64)
        .map_err(|err| anyhow!("decode ciphertext: {err}"))?;
    let derived_key = derive_key(key);
    let cipher =
        Aes256GcmSiv::new_from_slice(&derived_key).map_err(|err| anyhow!("init cipher: {err}"))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext.as_ref())
        .map_err(|err| anyhow!("decrypt answer value: {err}"))?;
    serde_json::from_slice(&plaintext).map_err(|err| anyhow!("decode decrypted JSON: {err}"))
}

pub fn decrypt_tree(value: &Value, key: &str) -> Result<Value> {
    match value {
        Value::Object(map) => {
            if is_encrypted_value(map) {
                return decrypt_value(value, key);
            }
            let mut out = JsonMap::new();
            for (k, v) in map {
                out.insert(k.clone(), decrypt_tree(v, key)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(decrypt_tree(item, key)?);
            }
            Ok(Value::Array(out))
        }
        _ => Ok(value.clone()),
    }
}

fn derive_key(key: &str) -> [u8; 32] {
    let digest = Sha256::digest(key.as_bytes());
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}

fn is_encrypted_value(map: &JsonMap<String, Value>) -> bool {
    map.get(MARKER_FIELD).and_then(Value::as_str) == Some(ENCRYPTED_KIND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let value = json!({"secret": "abc", "enabled": true});
        let encrypted = encrypt_value(&value, "demo-key").expect("encrypt");
        assert!(has_encrypted_values(&encrypted));
        let decrypted = decrypt_value(&encrypted, "demo-key").expect("decrypt");
        assert_eq!(decrypted, value);
    }

    #[test]
    fn decrypt_tree_walks_nested_values() {
        let inner = encrypt_value(&json!("abc"), "demo-key").expect("encrypt");
        let doc = json!({"setup_answers": {"provider": {"token": inner}}});
        let decrypted = decrypt_tree(&doc, "demo-key").expect("decrypt tree");
        assert_eq!(
            decrypted["setup_answers"]["provider"]["token"],
            json!("abc")
        );
    }
}
