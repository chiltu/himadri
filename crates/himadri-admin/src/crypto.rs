//! Encryption at rest for provider API keys.
//!
//! Values are encrypted with AES-256-GCM under a key supplied via the
//! `PROVIDER_ENCRYPTION_KEY` env var (32 raw bytes, base64-encoded — e.g.
//! `openssl rand -base64 32`). Ciphertext is stored as `enc:v1:<base64(nonce || ciphertext)>`
//! so plaintext rows written before encryption was enabled remain readable
//! (`decrypt` passes through any value without the prefix unchanged), allowing
//! in-place adoption without a data migration: existing rows are transparently
//! re-encrypted the next time they're written.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, Key};
use base64::{engine::general_purpose::STANDARD, Engine as _};

const PREFIX: &str = "enc:v1:";

#[derive(Clone)]
pub struct CipherKey(Key<Aes256Gcm>);

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("PROVIDER_ENCRYPTION_KEY must decode to exactly 32 bytes")]
    InvalidKeyLength,
    #[error("PROVIDER_ENCRYPTION_KEY is not valid base64: {0}")]
    InvalidBase64(#[from] base64::DecodeError),
    #[error("failed to decrypt value (corrupt data or wrong key)")]
    DecryptFailed,
}

impl CipherKey {
    pub fn from_base64(encoded: &str) -> Result<Self, CryptoError> {
        let bytes = STANDARD.decode(encoded.trim())?;
        if bytes.len() != 32 {
            return Err(CryptoError::InvalidKeyLength);
        }
        Ok(Self(*Key::<Aes256Gcm>::from_slice(&bytes)))
    }

    /// Loads the key from `PROVIDER_ENCRYPTION_KEY`. Returns `None` if unset,
    /// in which case callers should fall back to storing values in plaintext.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("PROVIDER_ENCRYPTION_KEY").ok()?;
        match Self::from_base64(&raw) {
            Ok(key) => Some(key),
            Err(e) => {
                tracing::error!("PROVIDER_ENCRYPTION_KEY is set but invalid: {e}");
                None
            }
        }
    }

    pub fn encrypt(&self, plaintext: &str) -> String {
        let cipher = Aes256Gcm::new(&self.0);
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .expect("AES-GCM encryption is infallible for valid keys/nonces");
        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ciphertext);
        format!("{PREFIX}{}", STANDARD.encode(combined))
    }

    /// Decrypts a value produced by `encrypt`. Values without the `enc:v1:`
    /// prefix are assumed to be legacy plaintext and returned unchanged.
    pub fn decrypt(&self, value: &str) -> Result<String, CryptoError> {
        let Some(encoded) = value.strip_prefix(PREFIX) else {
            return Ok(value.to_string());
        };
        let combined = STANDARD.decode(encoded)?;
        if combined.len() < 12 {
            return Err(CryptoError::DecryptFailed);
        }
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let cipher = Aes256Gcm::new(&self.0);
        let plaintext = cipher
            .decrypt(nonce_bytes.into(), ciphertext)
            .map_err(|_| CryptoError::DecryptFailed)?;
        String::from_utf8(plaintext).map_err(|_| CryptoError::DecryptFailed)
    }
}

/// Encryption-at-rest policy for `ModelEndpoint.api_key`, shared by the
/// SQLite and Postgres stores so the encrypt-if-nonempty rule and the
/// decrypt-failure handling can never diverge between backends.
///
/// Encrypts a key about to be stored. Without a cipher (or for `None`/empty
/// values) the input is passed through unchanged.
pub(crate) fn encrypt_endpoint_api_key(
    cipher: Option<&CipherKey>,
    api_key: Option<String>,
) -> Option<String> {
    match (cipher, api_key) {
        (Some(cipher), Some(plaintext)) if !plaintext.is_empty() => {
            Some(cipher.encrypt(&plaintext))
        }
        (_, other) => other,
    }
}

/// Decrypts `endpoint.api_key` in place for reads. On failure the field is
/// cleared (and the failure logged) rather than handing ciphertext to
/// callers. Writers must treat `api_key: None` as "leave the stored column
/// alone", so a decrypt failure can never overwrite the stored ciphertext.
pub(crate) fn decrypt_endpoint(
    cipher: Option<&CipherKey>,
    mut endpoint: crate::models::ModelEndpoint,
) -> crate::models::ModelEndpoint {
    if let (Some(cipher), Some(value)) = (cipher, &endpoint.api_key) {
        match cipher.decrypt(value) {
            Ok(plaintext) => endpoint.api_key = Some(plaintext),
            Err(e) => {
                tracing::error!(endpoint = %endpoint.id, "failed to decrypt endpoint api_key: {e}");
                endpoint.api_key = None;
            }
        }
    }
    endpoint
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key() -> CipherKey {
        CipherKey::from_base64(&STANDARD.encode([7u8; 32])).unwrap()
    }

    #[test]
    fn round_trips() {
        let key = test_key();
        let ciphertext = key.encrypt("sk-super-secret");
        assert!(ciphertext.starts_with(PREFIX));
        assert_eq!(key.decrypt(&ciphertext).unwrap(), "sk-super-secret");
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        let key = test_key();
        assert_eq!(
            key.decrypt("sk-legacy-plaintext").unwrap(),
            "sk-legacy-plaintext"
        );
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let key = test_key();
        let other = CipherKey::from_base64(&STANDARD.encode([9u8; 32])).unwrap();
        let ciphertext = key.encrypt("sk-super-secret");
        assert!(other.decrypt(&ciphertext).is_err());
    }

    #[test]
    fn rejects_wrong_length_key() {
        assert!(matches!(
            CipherKey::from_base64(&STANDARD.encode([1u8; 16])),
            Err(CryptoError::InvalidKeyLength)
        ));
    }
}
