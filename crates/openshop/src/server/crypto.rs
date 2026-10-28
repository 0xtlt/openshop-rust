//! AES-256-GCM encryption for provider credentials and Shopify access tokens.
//!
//! The stored formats are identical to the TypeScript implementation, so both
//! runtimes can read each other's rows.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Result, anyhow, bail};
use rand::Rng;
use serde_json::Value;

use crate::runtime::logger;
use crate::types::JsonMap;

const IV_LENGTH: usize = 12;
const TAG_LENGTH: usize = 16;
const ENCRYPTED_MARKER: &str = "aes-256-gcm";
const ENCRYPTED_STRING_PREFIX: &str = "enc:aes-256-gcm:";

/// What a missing key says. It names both ways out: a key, or an environment
/// that is explicitly a development one.
const MISSING_KEY: &str = "ENCRYPTION_KEY must be set (64 hex characters, generate one with `openssl rand -hex 32`); \
     to store secrets in plaintext on a development machine, run `openshop dev` or set OPENSHOP_ENV=development";

struct Inner {
    encryption_key: Option<String>,
    allow_plaintext: bool,
    key: OnceLock<Result<[u8; 32], String>>,
    warned: AtomicBool,
}

/// Encrypts with `ENCRYPTION_KEY` (64 hex characters). Without a key, values are
/// refused, unless plaintext is allowed: only a development environment does,
/// see [`RuntimeEnv::allows_plaintext_secrets`](crate::runtime::RuntimeEnv::allows_plaintext_secrets).
#[derive(Clone)]
pub struct Crypto {
    inner: Arc<Inner>,
}

impl Crypto {
    pub fn new(encryption_key: Option<String>, allow_plaintext: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                encryption_key: encryption_key.filter(|key| !key.is_empty()),
                allow_plaintext,
                key: OnceLock::new(),
                warned: AtomicBool::new(false),
            }),
        }
    }

    fn key(&self) -> Result<Option<[u8; 32]>> {
        let Some(hex_key) = &self.inner.encryption_key else {
            if !self.inner.allow_plaintext {
                bail!(MISSING_KEY);
            }
            if !self.inner.warned.swap(true, Ordering::Relaxed) {
                logger::warn("[openshop] ENCRYPTION_KEY not set — provider credentials stored in plaintext");
            }
            return Ok(None);
        };

        self.inner
            .key
            .get_or_init(|| {
                const INVALID: &str = "ENCRYPTION_KEY must be 64 hex characters (32 bytes)";
                if hex_key.len() != 64 {
                    return Err(INVALID.to_string());
                }
                let bytes = hex::decode(hex_key).map_err(|_| INVALID.to_string())?;
                <[u8; 32]>::try_from(bytes).map_err(|_| INVALID.to_string())
            })
            .clone()
            .map(Some)
            .map_err(|message| anyhow!(message))
    }

    fn seal(&self, key: &[u8; 32], plaintext: &[u8]) -> Result<String> {
        let mut iv = [0u8; IV_LENGTH];
        rand::rng().fill_bytes(&mut iv);

        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("Invalid encryption key"))?;
        let mut sealed = cipher
            .encrypt(&Nonce::from(iv), plaintext)
            .map_err(|_| anyhow!("Encryption failed"))?;
        let tag = sealed.split_off(sealed.len() - TAG_LENGTH);

        Ok(format!(
            "{}:{}:{}",
            hex::encode(iv),
            hex::encode(tag),
            hex::encode(sealed)
        ))
    }

    fn open(&self, key: &[u8; 32], payload: &str) -> Result<Vec<u8>> {
        let mut parts = payload.split(':');
        let (Some(iv), Some(tag), Some(data)) = (parts.next(), parts.next(), parts.next()) else {
            bail!("Malformed encrypted payload");
        };
        let iv = hex::decode(iv).map_err(|_| anyhow!("Malformed encrypted payload"))?;
        let tag = hex::decode(tag).map_err(|_| anyhow!("Malformed encrypted payload"))?;
        let mut ciphertext = hex::decode(data).map_err(|_| anyhow!("Malformed encrypted payload"))?;
        if iv.len() != IV_LENGTH || tag.len() != TAG_LENGTH {
            bail!("Malformed encrypted payload");
        }
        ciphertext.extend_from_slice(&tag);

        let nonce = Nonce::try_from(iv.as_slice()).map_err(|_| anyhow!("Malformed encrypted payload"))?;
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("Invalid encryption key"))?;
        cipher
            .decrypt(&nonce, ciphertext.as_slice())
            .map_err(|_| anyhow!("Unable to decrypt: data is corrupted or the key is wrong"))
    }

    /// Encrypt a config object. Returns the encrypted envelope or the raw config if no key.
    pub fn encrypt_config(&self, config: &JsonMap) -> Result<Value> {
        let Some(key) = self.key()? else {
            return Ok(Value::Object(config.clone()));
        };

        let plaintext = serde_json::to_vec(config)?;
        let mut envelope = JsonMap::new();
        envelope.insert("__encrypted".into(), Value::from(ENCRYPTED_MARKER));
        envelope.insert("payload".into(), Value::from(self.seal(&key, &plaintext)?));
        Ok(Value::Object(envelope))
    }

    /// Decrypt a config object. Handles both encrypted envelopes and legacy plaintext.
    pub fn decrypt_config(&self, stored: Option<&Value>) -> Result<JsonMap> {
        let Some(Value::Object(object)) = stored else {
            return Ok(JsonMap::new());
        };
        if object.get("__encrypted").and_then(Value::as_str) != Some(ENCRYPTED_MARKER) {
            return Ok(object.clone());
        }

        let Some(key) = self.key()? else {
            bail!("Cannot decrypt provider config: ENCRYPTION_KEY not set");
        };
        let payload = object.get("payload").and_then(Value::as_str).unwrap_or_default();
        let decrypted = self.open(&key, payload)?;
        match serde_json::from_slice(&decrypted)? {
            Value::Object(config) => Ok(config),
            _ => Ok(JsonMap::new()),
        }
    }

    /// Fails when no key is set and plaintext is not allowed, which is checked
    /// when the application starts. A key that is set is only checked when it
    /// is used.
    pub(crate) fn check_configured(&self) -> Result<()> {
        if self.inner.encryption_key.is_none() && !self.inner.allow_plaintext {
            bail!(MISSING_KEY);
        }
        Ok(())
    }

    /// Fails as [`Crypto::encrypt_string`] would, without a value: without a
    /// valid `ENCRYPTION_KEY` outside development.
    pub(crate) fn can_encrypt(&self) -> Result<()> {
        self.key().map(|_| ())
    }

    pub fn encrypt_string(&self, value: &str) -> Result<String> {
        let Some(key) = self.key()? else {
            return Ok(value.to_string());
        };
        Ok(format!(
            "{ENCRYPTED_STRING_PREFIX}{}",
            self.seal(&key, value.as_bytes())?
        ))
    }

    /// Legacy plaintext values are returned as-is; empty values become `None`.
    pub fn decrypt_string(&self, stored: Option<&str>) -> Result<Option<String>> {
        let Some(stored) = stored.filter(|stored| !stored.is_empty()) else {
            return Ok(None);
        };
        let Some(payload) = stored.strip_prefix(ENCRYPTED_STRING_PREFIX) else {
            return Ok(Some(stored.to_string()));
        };

        let Some(key) = self.key()? else {
            bail!("Cannot decrypt secret: ENCRYPTION_KEY not set");
        };
        Ok(Some(String::from_utf8(self.open(&key, payload)?)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn crypto() -> Crypto {
        Crypto::new(Some(KEY.into()), false)
    }

    fn object(value: Value) -> JsonMap {
        value.as_object().cloned().unwrap()
    }

    #[test]
    fn encrypt_returns_encrypted_envelope() {
        let result = crypto().encrypt_config(&object(json!({ "apiKey": "sk-123" }))).unwrap();
        assert_eq!(result["__encrypted"], "aes-256-gcm");
        assert!(result["payload"].is_string());
    }

    #[test]
    fn encrypt_and_decrypt_roundtrip() {
        let original = object(json!({ "apiKey": "sk-123", "secret": "my-secret", "nested": { "foo": "bar" } }));
        let encrypted = crypto().encrypt_config(&original).unwrap();
        assert_eq!(crypto().decrypt_config(Some(&encrypted)).unwrap(), original);
    }

    #[test]
    fn two_encryptions_produce_different_payloads() {
        let config = object(json!({ "key": "value" }));
        let a = crypto().encrypt_config(&config).unwrap();
        let b = crypto().encrypt_config(&config).unwrap();
        assert_ne!(a["payload"], b["payload"]);
    }

    #[test]
    fn tampered_ciphertext_fails_on_decrypt() {
        let mut encrypted = crypto().encrypt_config(&object(json!({ "key": "value" }))).unwrap();
        let payload = encrypted["payload"].as_str().unwrap().to_string();
        let mut parts: Vec<String> = payload.split(':').map(str::to_string).collect();
        let last = parts[2].pop().unwrap();
        parts[2].push(if last == '0' { '1' } else { '0' });
        encrypted["payload"] = Value::from(parts.join(":"));
        assert!(crypto().decrypt_config(Some(&encrypted)).is_err());
    }

    #[test]
    fn non_encrypted_object_passes_through() {
        let plain = json!({ "foo": "bar", "num": 42 });
        assert_eq!(crypto().decrypt_config(Some(&plain)).unwrap(), object(plain));
    }

    #[test]
    fn null_input_returns_empty_object() {
        assert!(crypto().decrypt_config(Some(&Value::Null)).unwrap().is_empty());
        assert!(crypto().decrypt_config(None).unwrap().is_empty());
    }

    #[test]
    fn encrypt_string_and_decrypt_string_roundtrip() {
        let encrypted = crypto().encrypt_string("offline-token").unwrap();
        assert_ne!(encrypted, "offline-token");
        assert!(encrypted.starts_with("enc:aes-256-gcm:"));
        assert_eq!(
            crypto().decrypt_string(Some(&encrypted)).unwrap().as_deref(),
            Some("offline-token")
        );
    }

    #[test]
    fn decrypt_string_keeps_legacy_plaintext_values() {
        assert_eq!(
            crypto().decrypt_string(Some("legacy-token")).unwrap().as_deref(),
            Some("legacy-token")
        );
        assert_eq!(crypto().decrypt_string(None).unwrap(), None);
        assert_eq!(crypto().decrypt_string(Some("")).unwrap(), None);
    }

    #[test]
    fn stores_plaintext_without_a_key_when_it_is_allowed() {
        let crypto = Crypto::new(None, true);
        let config = object(json!({ "apiKey": "sk-123" }));
        assert_eq!(crypto.encrypt_config(&config).unwrap(), Value::Object(config));
        assert_eq!(crypto.encrypt_string("token").unwrap(), "token");
    }

    #[test]
    fn refuses_to_store_without_a_key_unless_plaintext_is_allowed() {
        let crypto = Crypto::new(None, false);
        let error = crypto.encrypt_string("token").unwrap_err().to_string();
        assert_eq!(error, MISSING_KEY);
        assert!(error.contains("openssl rand -hex 32") && error.contains("OPENSHOP_ENV=development"));
        assert_eq!(
            crypto.encrypt_config(&object(json!({ "apiKey": "sk-123" }))).unwrap_err().to_string(),
            MISSING_KEY
        );
        assert_eq!(crypto.check_configured().unwrap_err().to_string(), MISSING_KEY);
        assert!(Crypto::new(None, true).check_configured().is_ok());
        assert!(Crypto::new(Some(KEY.into()), false).check_configured().is_ok());
    }

    #[test]
    fn rejects_keys_with_the_wrong_length() {
        let crypto = Crypto::new(Some("abc".into()), false);
        let error = crypto.encrypt_string("token").unwrap_err().to_string();
        assert_eq!(error, "ENCRYPTION_KEY must be 64 hex characters (32 bytes)");
    }

    #[test]
    fn cannot_decrypt_without_a_key() {
        let encrypted_config = crypto().encrypt_config(&object(json!({ "a": 1 }))).unwrap();
        let encrypted_string = crypto().encrypt_string("token").unwrap();
        let keyless = Crypto::new(None, true);
        assert_eq!(
            keyless.decrypt_config(Some(&encrypted_config)).unwrap_err().to_string(),
            "Cannot decrypt provider config: ENCRYPTION_KEY not set"
        );
        assert_eq!(
            keyless.decrypt_string(Some(&encrypted_string)).unwrap_err().to_string(),
            "Cannot decrypt secret: ENCRYPTION_KEY not set"
        );
    }

    #[test]
    fn reads_values_written_by_the_typescript_implementation() {
        // Produced with node:crypto (aes-256-gcm, 12-byte IV, 16-byte tag) and KEY above.
        let iv = [7u8; IV_LENGTH];
        let cipher = Aes256Gcm::new_from_slice(&hex::decode(KEY).unwrap()).unwrap();
        let mut sealed = cipher.encrypt(&Nonce::from(iv), b"shpat_123".as_ref()).unwrap();
        let tag = sealed.split_off(sealed.len() - TAG_LENGTH);
        let stored = format!(
            "enc:aes-256-gcm:{}:{}:{}",
            hex::encode(iv),
            hex::encode(tag),
            hex::encode(sealed)
        );
        assert_eq!(
            crypto().decrypt_string(Some(&stored)).unwrap().as_deref(),
            Some("shpat_123")
        );
    }
}
