//! Bringing your own key into Transit.
//!
//! Transit will not take a private key in the clear. It publishes a wrapping
//! key, and the caller sends the key encrypted for it: a fresh AES-256 key
//! wraps the PKCS#8 document with AES-KWP, and the wrapping key wraps that AES
//! key with RSA-OAEP. The two ciphertexts travel concatenated, which is how
//! the engine tells them apart again.

use aws_lc_rs::key_wrap::{AesKek, KeyWrapPadded, AES_256};
use aws_lc_rs::rand;
use aws_lc_rs::rsa::{OaepPublicEncryptingKey, PublicEncryptingKey, OAEP_SHA256_MGF1SHA256};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use secrecy::zeroize::Zeroizing;
use serde::Serialize;
use serde_json::Value;

use super::constants::{
    EPHEMERAL_KEY_LEN, KWP_BLOCK, OAEP_HASH_FUNCTION, PEM_FENCE, TRANSIT_IMPORT_ROUTE, TRANSIT_IMPORT_VERSION_ROUTE,
    TRANSIT_WRAPPING_KEY_ROUTE,
};
use super::transit::{anchor, malformed};
use super::{Absence, OpenBaoBackend};
use crate::secrets::{SecretPath, SecretsError};

/// Why a key could not be wrapped. Never carries key bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WrapError {
    #[error("the Transit wrapping key is not a PEM public key")]
    WrappingKeyEncoding,

    #[error("the Transit wrapping key is not an RSA public key this build can encrypt with")]
    WrappingKey,

    #[error("the system could not produce randomness for the ephemeral key")]
    Randomness,

    #[error("the key could not be wrapped")]
    Wrap,
}

/// The `ciphertext` Transit's import endpoints take for `pkcs8`, wrapped for
/// the PEM `wrapping_key` the engine published.
pub fn wrap(wrapping_key_pem: &str, pkcs8: &[u8]) -> Result<String, WrapError> {
    let body: String = wrapping_key_pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(PEM_FENCE))
        .collect();
    let der = BASE64.decode(body).or(Err(WrapError::WrappingKeyEncoding))?;
    let wrapping = OaepPublicEncryptingKey::new(PublicEncryptingKey::from_der(&der).or(Err(WrapError::WrappingKey))?)
        .or(Err(WrapError::WrappingKey))?;

    let mut ephemeral = Zeroizing::new([0u8; EPHEMERAL_KEY_LEN]);
    rand::fill(ephemeral.as_mut_slice()).or(Err(WrapError::Randomness))?;

    let mut wrapped_ephemeral = vec![0u8; wrapping.ciphertext_size()];
    let wrapped_ephemeral_len = wrapping
        .encrypt(
            &OAEP_SHA256_MGF1SHA256,
            ephemeral.as_slice(),
            &mut wrapped_ephemeral,
            None,
        )
        .or(Err(WrapError::Wrap))?
        .len();
    wrapped_ephemeral.truncate(wrapped_ephemeral_len);

    let kek = AesKek::new(&AES_256, ephemeral.as_slice()).or(Err(WrapError::Wrap))?;
    let mut wrapped_key = vec![0u8; pkcs8.len().div_ceil(KWP_BLOCK) * KWP_BLOCK + KWP_BLOCK];
    let wrapped_key_len = kek
        .wrap_with_padding(pkcs8, &mut wrapped_key)
        .or(Err(WrapError::Wrap))?
        .len();
    wrapped_key.truncate(wrapped_key_len);

    wrapped_ephemeral.extend_from_slice(&wrapped_key);
    Ok(BASE64.encode(wrapped_ephemeral))
}

#[derive(Serialize)]
struct ImportBody<'a> {
    ciphertext: &'a str,
    hash_function: &'static str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    key_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exportable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_plaintext_backup: Option<bool>,
}

impl OpenBaoBackend {
    /// The PEM public key Transit asks imported keys to be wrapped for.
    pub async fn transit_wrapping_key(&self, mount: &str) -> Result<String, SecretsError> {
        let anchor = anchor(mount, "", "wrapping_key");
        let url = format!("{}/v1/{mount}/wrapping_key", self.addr);
        let answered = self
            .exchange(
                reqwest::Method::GET,
                TRANSIT_WRAPPING_KEY_ROUTE,
                &url,
                &anchor,
                None,
                Absence::IsAFailure,
                |body| {
                    body.get("data")
                        .and_then(|data| data.get("public_key"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .ok_or_else(|| malformed(&anchor, "response carries no wrapping `public_key`"))
                },
            )
            .await?;
        answered.ok_or_else(|| malformed(&anchor, "the engine answered with no body"))
    }

    /// Create a Transit key of `key_type` from an already wrapped private key.
    ///
    /// The key is made neither exportable nor backed up in the clear, since
    /// the point of importing it is that it stays inside the engine.
    pub async fn transit_import(
        &self,
        mount: &str,
        name: &str,
        key_type: &str,
        ciphertext: &str,
    ) -> Result<(), SecretsError> {
        let anchor = anchor(mount, name, "keys");
        let url = format!("{}/v1/{mount}/keys/{name}/import", self.addr);
        let body = import_body(
            &anchor,
            &ImportBody {
                ciphertext,
                hash_function: OAEP_HASH_FUNCTION,
                key_type: Some(key_type),
                exportable: Some(false),
                allow_plaintext_backup: Some(false),
            },
        )?;
        self.exchange(
            reqwest::Method::POST,
            TRANSIT_IMPORT_ROUTE,
            &url,
            &anchor,
            Some(body),
            Absence::IsAFailure,
            |_| Ok(()),
        )
        .await?;
        Ok(())
    }

    /// Add a version to a Transit key that was itself imported.
    pub async fn transit_import_version(&self, mount: &str, name: &str, ciphertext: &str) -> Result<(), SecretsError> {
        let anchor = anchor(mount, name, "keys");
        let url = format!("{}/v1/{mount}/keys/{name}/import_version", self.addr);
        let body = import_body(
            &anchor,
            &ImportBody {
                ciphertext,
                hash_function: OAEP_HASH_FUNCTION,
                key_type: None,
                exportable: None,
                allow_plaintext_backup: None,
            },
        )?;
        self.exchange(
            reqwest::Method::POST,
            TRANSIT_IMPORT_VERSION_ROUTE,
            &url,
            &anchor,
            Some(body),
            Absence::IsAFailure,
            |_| Ok(()),
        )
        .await?;
        Ok(())
    }
}

fn import_body(anchor: &SecretPath, body: &ImportBody<'_>) -> Result<Value, SecretsError> {
    serde_json::to_value(body).map_err(|e| malformed(anchor, &e.to_string()))
}
