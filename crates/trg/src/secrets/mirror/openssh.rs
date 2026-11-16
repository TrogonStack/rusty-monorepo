//! An unencrypted OpenSSH private key, read into the form Transit imports.
//!
//! 1Password hands an SSH key out as `op://...?ssh-format=openssh`, which is
//! the `openssh-key-v1` container. Transit imports a PKCS#8 document instead,
//! so the container is opened, the key material is checked against itself, and
//! it is rewritten as PKCS#8. The private half only ever lives in zeroizing
//! buffers, and no error here carries any of it.

use aws_lc_rs::rsa::{KeyPair as RsaKeyPair, KeyPairComponents, PublicKeyComponents};
use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretSlice};

use crate::config::signers::{read_string, PublicKey, RsaPublicKey};

use super::constants::{
    ED25519_PKCS8_PREFIX, ED25519_PRIVATE_LEN, ED25519_SEED_LEN, OPENSSH_CIPHER_NONE, OPENSSH_FENCE_BEGIN,
    OPENSSH_FENCE_END, OPENSSH_MAGIC, SSH_TYPE_ED25519, SSH_TYPE_RSA,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceKeyError {
    #[error("the source value is not an OpenSSH private key (`-----BEGIN OPENSSH PRIVATE KEY-----`); in 1Password, read the key with `?ssh-format=openssh`")]
    NotOpenssh,

    #[error("the OpenSSH private key is malformed: {0}")]
    Malformed(&'static str),

    #[error("the OpenSSH private key is passphrase protected; mirror an unencrypted export of it")]
    Encrypted,

    #[error("`{0}` private keys cannot be imported into Transit here: only ssh-ed25519 and ssh-rsa are supported")]
    Unsupported(String),

    #[error("RSA keys of {0} bits cannot be imported into Transit here: only 2048, 3072 and 4096 are supported")]
    RsaSize(usize),

    #[error("the private key does not match itself, so it was not imported")]
    Inconsistent,
}

/// A private key ready to import, with the public half it was checked against.
pub struct SourceKey {
    public: PublicKey,
    transit_type: &'static str,
    pkcs8: SecretSlice<u8>,
}

impl SourceKey {
    pub fn public(&self) -> &PublicKey {
        &self.public
    }

    /// The `type` Transit names this key by.
    pub fn transit_type(&self) -> &'static str {
        self.transit_type
    }

    /// The PKCS#8 document, which is what gets wrapped.
    pub fn pkcs8(&self) -> &[u8] {
        self.pkcs8.expose_secret()
    }
}

impl std::fmt::Debug for SourceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SourceKey({}, {})", self.transit_type, self.public.type_name())
    }
}

pub fn parse(text: &str) -> Result<SourceKey, SourceKeyError> {
    let mut body = String::new();
    let mut inside = false;
    let mut closed = false;
    for line in text.lines().map(str::trim) {
        if line == OPENSSH_FENCE_BEGIN {
            inside = true;
        } else if line == OPENSSH_FENCE_END {
            closed = inside;
            break;
        } else if inside {
            body.push_str(line);
        }
    }
    if !closed {
        return Err(SourceKeyError::NotOpenssh);
    }
    let raw = Zeroizing::new(
        BASE64
            .decode(body.as_bytes())
            .or(Err(SourceKeyError::Malformed("the body is not base64")))?,
    );

    let mut rest = raw.strip_prefix(OPENSSH_MAGIC).ok_or(SourceKeyError::NotOpenssh)?;
    let cipher = next_string(&mut rest)?;
    let _kdf = next_string(&mut rest)?;
    let _kdf_options = next_string(&mut rest)?;
    if cipher != OPENSSH_CIPHER_NONE {
        return Err(SourceKeyError::Encrypted);
    }
    let count = next_u32(&mut rest)?;
    if count != 1 {
        return Err(SourceKeyError::Malformed("it must hold exactly one key"));
    }
    let _public_blob = next_string(&mut rest)?;
    let mut private = next_string(&mut rest)?;

    let (check_a, check_b) = (next_u32(&mut private)?, next_u32(&mut private)?);
    if check_a != check_b {
        return Err(SourceKeyError::Malformed("the check integers differ"));
    }
    let key_type = next_string(&mut private)?;
    match key_type {
        SSH_TYPE_ED25519 => ed25519(&mut private),
        SSH_TYPE_RSA => rsa(&mut private),
        other => Err(SourceKeyError::Unsupported(String::from_utf8_lossy(other).into_owned())),
    }
}

fn next_string<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], SourceKeyError> {
    read_string(input).ok_or(SourceKeyError::Malformed("truncated"))
}

fn next_u32(input: &mut &[u8]) -> Result<u32, SourceKeyError> {
    let (head, rest) = input
        .split_at_checked(4)
        .ok_or(SourceKeyError::Malformed("truncated"))?;
    *input = rest;
    Ok(u32::from_be_bytes([head[0], head[1], head[2], head[3]]))
}

fn ed25519(private: &mut &[u8]) -> Result<SourceKey, SourceKeyError> {
    let public = next_string(private)?;
    let pair = next_string(private)?;
    if pair.len() != ED25519_PRIVATE_LEN {
        return Err(SourceKeyError::Malformed("an Ed25519 private key is 64 bytes"));
    }
    let seed = &pair[..ED25519_SEED_LEN];
    let derived = Ed25519KeyPair::from_seed_unchecked(seed).or(Err(SourceKeyError::Inconsistent))?;
    if derived.public_key().as_ref() != public || &pair[ED25519_SEED_LEN..] != public {
        return Err(SourceKeyError::Inconsistent);
    }
    let mut pkcs8 = Zeroizing::new(Vec::with_capacity(ED25519_PKCS8_PREFIX.len() + ED25519_SEED_LEN));
    pkcs8.extend_from_slice(&ED25519_PKCS8_PREFIX);
    pkcs8.extend_from_slice(seed);
    Ok(SourceKey {
        public: PublicKey::from_ed25519_bytes(public).or(Err(SourceKeyError::Inconsistent))?,
        transit_type: "ed25519",
        pkcs8: SecretSlice::from(std::mem::take(&mut *pkcs8)),
    })
}

fn rsa(private: &mut &[u8]) -> Result<SourceKey, SourceKeyError> {
    let n = strip(next_string(private)?);
    let e = strip(next_string(private)?);
    let d = next_string(private)?;
    let iqmp = next_string(private)?;
    let p = next_string(private)?;
    let q = next_string(private)?;

    let transit_type = match n.len() * 8 {
        2048 => "rsa-2048",
        3072 => "rsa-3072",
        4096 => "rsa-4096",
        bits => return Err(SourceKeyError::RsaSize(bits)),
    };
    let dp = Zeroizing::new(modulo(d, &minus_one(p)));
    let dq = Zeroizing::new(modulo(d, &minus_one(q)));
    let components = KeyPairComponents {
        public_key: PublicKeyComponents { n, e },
        d,
        p,
        q,
        dP: dp.as_slice(),
        dQ: dq.as_slice(),
        qInv: iqmp,
    };
    let pair = RsaKeyPair::from_components(&components).or(Err(SourceKeyError::Inconsistent))?;
    let document = aws_lc_rs::encoding::AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&pair)
        .or(Err(SourceKeyError::Inconsistent))?;
    let public = RsaPublicKey::new(e, n).or(Err(SourceKeyError::Inconsistent))?;
    Ok(SourceKey {
        public: PublicKey::Rsa(public),
        transit_type,
        pkcs8: SecretSlice::from(document.as_ref().to_vec()),
    })
}

fn strip(bytes: &[u8]) -> &[u8] {
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    &bytes[zeros..]
}

/// `value - 1` for an odd value, as big-endian bytes.
fn minus_one(value: &[u8]) -> Vec<u8> {
    let mut out = strip(value).to_vec();
    for byte in out.iter_mut().rev() {
        if *byte == 0 {
            *byte = 0xff;
        } else {
            *byte -= 1;
            break;
        }
    }
    out
}

/// `value mod modulus` over big-endian bytes, one bit at a time. The CRT
/// exponents are the only arithmetic the container leaves undone, and a key
/// is imported rarely enough that a few thousand byte-wide steps cost nothing.
fn modulo(value: &[u8], modulus: &[u8]) -> Vec<u8> {
    let modulus = strip(modulus);
    if modulus.is_empty() {
        return Vec::new();
    }
    let width = modulus.len() + 1;
    let mut padded = vec![0u8; width];
    padded[width - modulus.len()..].copy_from_slice(modulus);
    let mut rem = Zeroizing::new(vec![0u8; width]);
    for byte in value {
        for shift in (0..8).rev() {
            let mut carry = (byte >> shift) & 1;
            for cell in rem.iter_mut().rev() {
                let next = *cell >> 7;
                *cell = (*cell << 1) | carry;
                carry = next;
            }
            if rem.as_slice() >= padded.as_slice() {
                let mut borrow = 0i16;
                for (cell, sub) in rem.iter_mut().zip(&padded).rev() {
                    let mut diff = i16::from(*cell) - i16::from(*sub) - borrow;
                    borrow = i16::from(diff < 0);
                    if diff < 0 {
                        diff += 256;
                    }
                    *cell = u8::try_from(diff).unwrap_or(0);
                }
            }
        }
    }
    strip(&rem).to_vec()
}

#[cfg(test)]
#[path = "openssh_tests.rs"]
mod tests;
