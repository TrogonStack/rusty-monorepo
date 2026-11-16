//! The slice of the Transit engine that signing needs.
//!
//! Transit never hands out a private key, so signing is a request to the
//! engine and the answer is a signature over exactly the bytes that were sent.
//! Only the one read and one write below are spoken, which is why they sit
//! beside the KV v2 calls rather than behind a separate client.

use serde::Serialize;
use serde_json::Value;

use super::constants::{TRANSIT_KEY_ROUTE, TRANSIT_SIGN_ROUTE};
use super::{Absence, OpenBaoBackend};
use crate::secrets::{SecretPath, SecretsError};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

/// How a key type wants to be asked to sign, where its default is not enough.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignParameters {
    pub hash_algorithm: &'static str,
    pub signature_algorithm: &'static str,
}

#[derive(Serialize)]
struct SignBody {
    input: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash_algorithm: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signature_algorithm: Option<&'static str>,
}

/// What `GET <mount>/keys/<name>` says about the version that signs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitPublicKey {
    pub key_type: String,
    pub public_key: String,
}

impl OpenBaoBackend {
    /// The public half of the version of a Transit key that signs, or `None`
    /// when the key does not exist.
    pub async fn transit_public_key(&self, mount: &str, name: &str) -> Result<Option<TransitPublicKey>, SecretsError> {
        let anchor = anchor(mount, name, "keys");
        let url = format!("{}/v1/{mount}/keys/{name}", self.addr);
        self.exchange(
            reqwest::Method::GET,
            TRANSIT_KEY_ROUTE,
            &url,
            &anchor,
            None,
            Absence::IsAMiss,
            |body| parse_public_key(&body, &anchor),
        )
        .await
    }

    /// The raw signature the engine produced over `message`.
    pub async fn transit_sign(
        &self,
        mount: &str,
        name: &str,
        message: &[u8],
        parameters: Option<SignParameters>,
    ) -> Result<Vec<u8>, SecretsError> {
        let anchor = anchor(mount, name, "sign");
        let url = format!("{}/v1/{mount}/sign/{name}", self.addr);
        let body = serde_json::to_value(SignBody {
            input: BASE64.encode(message),
            hash_algorithm: parameters.map(|p| p.hash_algorithm),
            signature_algorithm: parameters.map(|p| p.signature_algorithm),
        })
        .map_err(|e| malformed(&anchor, &e.to_string()))?;
        let answered = self
            .exchange(
                reqwest::Method::POST,
                TRANSIT_SIGN_ROUTE,
                &url,
                &anchor,
                Some(body),
                Absence::IsAFailure,
                |body| parse_signature(&body, &anchor),
            )
            .await?;
        answered.ok_or_else(|| malformed(&anchor, "the engine answered with no body"))
    }
}

pub(super) fn anchor(mount: &str, name: &str, verb: &str) -> SecretPath {
    SecretPath::parse(&format!("{mount}/{verb}/{name}"))
        .unwrap_or_else(|_| SecretPath::parse("transit").expect("a static path is valid"))
}

pub(super) fn malformed(anchor: &SecretPath, cause: &str) -> SecretsError {
    SecretsError::Malformed {
        path: anchor.clone(),
        cause: cause.to_string(),
        raw: None,
    }
}

fn parse_public_key(body: &Value, anchor: &SecretPath) -> Result<TransitPublicKey, SecretsError> {
    let data = body
        .get("data")
        .ok_or_else(|| malformed(anchor, "response has no `data`"))?;
    let key_type = data
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(anchor, "response names no key `type`"))?
        .to_string();
    let latest = data
        .get("latest_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| malformed(anchor, "response names no `latest_version`"))?;
    let encoded = data
        .get("keys")
        .and_then(|keys| keys.get(latest.to_string()))
        .and_then(|version| version.get("public_key"))
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(anchor, "response carries no public key for the latest version"))?;
    Ok(TransitPublicKey {
        key_type,
        public_key: encoded.to_string(),
    })
}

fn parse_signature(body: &Value, anchor: &SecretPath) -> Result<Vec<u8>, SecretsError> {
    let text = body
        .get("data")
        .and_then(|data| data.get("signature"))
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(anchor, "response carries no `signature`"))?;
    let shape = "the signature is not in the `vault:vN:<base64>` form";
    let mut parts = text.splitn(3, ':');
    let (Some("vault"), Some(version), Some(encoded)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(malformed(anchor, shape));
    };
    let digits = version.strip_prefix('v').unwrap_or("");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed(anchor, shape));
    }
    BASE64
        .decode(encoded)
        .or(Err(malformed(anchor, "the signature is not base64")))
}
