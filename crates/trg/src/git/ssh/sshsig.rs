//! The SSHSIG signature format, from OpenSSH's PROTOCOL.sshsig.
//!
//! What is signed is not the file but a small structure naming the namespace
//! and the digest of the file, so a signature made for one purpose cannot be
//! replayed for another. The signature is then wrapped with the public key and
//! those same fields and armored, which is the form `ssh-keygen -Y verify`
//! and git read.

use sha2::{Digest, Sha256, Sha512};

use crate::config::signers::{base64, put_string, PublicKey};

use super::constants::{
    ARMOR_BEGIN, ARMOR_COLUMNS, ARMOR_END, ED25519_SIGNATURE_LEN, SIGNATURE_TYPE, SSHSIG_MAGIC as MAGIC,
    SSHSIG_VERSION as VERSION,
};

/// How the message is reduced before it is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HashAlgorithm {
    Sha256,
    #[default]
    Sha512,
}

impl HashAlgorithm {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "sha256" => Some(Self::Sha256),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }

    fn digest(self, message: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => Sha256::digest(message).to_vec(),
            Self::Sha512 => Sha512::digest(message).to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a namespace must not be empty")]
pub struct EmptyNamespaceError;

/// What a signature is for, such as `git`. A verifier names the namespace it
/// expects, so this is what stops a signature from being accepted elsewhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace(String);

impl Namespace {
    pub fn new(name: impl Into<String>) -> Result<Self, EmptyNamespaceError> {
        let name = name.into();
        if name.is_empty() {
            return Err(EmptyNamespaceError);
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a signature must be {ED25519_SIGNATURE_LEN} bytes, got {0}")]
pub struct SignatureLengthError(usize);

/// An Ed25519 signature, which is always the same length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ed25519Signature([u8; ED25519_SIGNATURE_LEN]);

impl Ed25519Signature {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SignatureLengthError> {
        <[u8; ED25519_SIGNATURE_LEN]>::try_from(bytes)
            .map(Self)
            .or(Err(SignatureLengthError(bytes.len())))
    }
}

/// The bytes the signer must sign for `message`.
///
/// Ed25519 signs these directly, with no further hashing, which is why a
/// Transit key is asked to sign them as they are.
pub fn signed_data(namespace: &Namespace, hash: HashAlgorithm, message: &[u8]) -> Vec<u8> {
    let mut out = Vec::from(&MAGIC[..]);
    put_string(&mut out, namespace.as_str().as_bytes());
    put_string(&mut out, b"");
    put_string(&mut out, hash.name().as_bytes());
    put_string(&mut out, &hash.digest(message));
    out
}

/// The armored signature `ssh-keygen -Y sign` writes to `<file>.sig`.
pub fn armor(key: &PublicKey, namespace: &Namespace, hash: HashAlgorithm, signature: &Ed25519Signature) -> String {
    let mut inner = Vec::new();
    put_string(&mut inner, SIGNATURE_TYPE.as_bytes());
    put_string(&mut inner, &signature.0);

    let mut blob = Vec::from(&MAGIC[..]);
    blob.extend_from_slice(&VERSION.to_be_bytes());
    put_string(&mut blob, &key.blob());
    put_string(&mut blob, namespace.as_str().as_bytes());
    put_string(&mut blob, b"");
    put_string(&mut blob, hash.name().as_bytes());
    put_string(&mut blob, &inner);

    let encoded = base64::encode(&blob);
    let mut out = String::with_capacity(encoded.len() + encoded.len() / ARMOR_COLUMNS + 80);
    out.push_str(ARMOR_BEGIN);
    out.push('\n');
    for line in encoded.as_bytes().chunks(ARMOR_COLUMNS) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str(ARMOR_END);
    out.push('\n');
    out
}
