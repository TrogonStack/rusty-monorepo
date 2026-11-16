//! What a Transit key of each type looks like to git.
//!
//! Transit names its key types by algorithm and hands the public key out in a
//! different encoding for each, so the type decides how the key is read and how
//! a signature is asked for.

use super::constants::{TRANSIT_ED25519, TRANSIT_RSA_HASH, TRANSIT_RSA_PREFIX, TRANSIT_RSA_SCHEME};
use super::der::{self, DerError};
use crate::config::signers::{PublicKey, PublicKeyError};
use crate::secrets::openbao::{SignParameters, TransitPublicKey};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitKeyError {
    #[error(
        "the Transit key is of type `{0}`, but only ed25519 and rsa-2048, rsa-3072 and rsa-4096 can sign for git here"
    )]
    UnsupportedType(String),

    #[error("the public key in the response is not base64")]
    Encoding,

    #[error("{0}")]
    Key(#[from] PublicKeyError),

    #[error("{0}")]
    Der(#[from] DerError),
}

/// The public key a Transit key signs as.
pub fn public_key(found: &TransitPublicKey) -> Result<PublicKey, TransitKeyError> {
    if found.key_type == TRANSIT_ED25519 {
        let bytes = BASE64.decode(&found.public_key).or(Err(TransitKeyError::Encoding))?;
        return Ok(PublicKey::from_ed25519_bytes(&bytes)?);
    }
    if found.key_type.starts_with(TRANSIT_RSA_PREFIX) {
        return Ok(PublicKey::Rsa(der::rsa_from_spki_pem(&found.public_key)?));
    }
    Err(TransitKeyError::UnsupportedType(found.key_type.clone()))
}

/// RSA is signed over a SHA-512 digest with PKCS#1 v1.5 padding, which is what
/// an `rsa-sha2-512` SSH signature is; Ed25519 needs nothing beyond the data.
pub fn sign_parameters(key: &PublicKey) -> Option<SignParameters> {
    match key {
        PublicKey::Ed25519(_) => None,
        PublicKey::Rsa(_) => Some(SignParameters {
            hash_algorithm: TRANSIT_RSA_HASH,
            signature_algorithm: TRANSIT_RSA_SCHEME,
        }),
    }
}
