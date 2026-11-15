//! The little of DER and PEM that an RSA public key needs.
//!
//! Transit hands an RSA public key out as a PEM `SubjectPublicKeyInfo`, while
//! SSH writes the same key as two integers, and the signature check wants it as
//! a bare `RSAPublicKey`. Only those three shapes are handled, and anything
//! that does not match them exactly is refused.

use super::constants::{
    DER_BIT_STRING, DER_INTEGER, DER_LONG_FORM, DER_NULL, DER_OID, DER_SEQUENCE, PEM_FENCE, RSA_ENCRYPTION_OID,
};
use crate::config::signers::{base64, PublicKeyError, RsaPublicKey};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DerError {
    #[error("the PEM is not valid base64")]
    Pem,

    #[error("the key is not a DER-encoded RSA public key: {0}")]
    Shape(&'static str),

    #[error("{0}")]
    Key(#[from] PublicKeyError),
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn element(&mut self, tag: u8) -> Result<&'a [u8], DerError> {
        let (&found, rest) = self.0.split_first().ok_or(DerError::Shape("truncated"))?;
        if found != tag {
            return Err(DerError::Shape("unexpected element"));
        }
        let (&first, mut rest) = rest.split_first().ok_or(DerError::Shape("truncated"))?;
        let length = if first & DER_LONG_FORM == 0 {
            usize::from(first)
        } else {
            let count = usize::from(first & !DER_LONG_FORM);
            if count == 0 || count > 4 || rest.len() < count {
                return Err(DerError::Shape("bad length"));
            }
            let (digits, after) = rest.split_at(count);
            rest = after;
            digits.iter().fold(0usize, |acc, &b| (acc << 8) | usize::from(b))
        };
        if rest.len() < length {
            return Err(DerError::Shape("truncated"));
        }
        let (body, after) = rest.split_at(length);
        self.0 = after;
        Ok(body)
    }

    fn finished(&self) -> Result<(), DerError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(DerError::Shape("trailing bytes"))
        }
    }
}

/// The key of a PEM `SubjectPublicKeyInfo` for `rsaEncryption`.
pub fn rsa_from_spki_pem(pem: &str) -> Result<RsaPublicKey, DerError> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with(PEM_FENCE))
        .collect();
    let der = base64::decode(&body).or(Err(DerError::Pem))?;

    let mut spki = Reader(&der);
    let info = spki.element(DER_SEQUENCE)?;
    spki.finished()?;

    let mut info = Reader(info);
    let algorithm = info.element(DER_SEQUENCE)?;
    let key_bits = info.element(DER_BIT_STRING)?;
    info.finished()?;

    let mut algorithm = Reader(algorithm);
    if algorithm.element(DER_OID)? != RSA_ENCRYPTION_OID {
        return Err(DerError::Shape("not an rsaEncryption key"));
    }
    let parameters = algorithm.element(DER_NULL)?;
    if !parameters.is_empty() {
        return Err(DerError::Shape("unexpected algorithm parameters"));
    }
    algorithm.finished()?;

    let (&unused_bits, key) = key_bits.split_first().ok_or(DerError::Shape("truncated"))?;
    if unused_bits != 0 {
        return Err(DerError::Shape("unexpected padding bits"));
    }
    let mut key = Reader(key);
    let numbers = key.element(DER_SEQUENCE)?;
    key.finished()?;

    let mut numbers = Reader(numbers);
    let modulus = numbers.element(DER_INTEGER)?;
    let exponent = numbers.element(DER_INTEGER)?;
    numbers.finished()?;
    Ok(RsaPublicKey::new(exponent, modulus)?)
}

fn put_length(out: &mut Vec<u8>, length: usize) {
    if length < usize::from(DER_LONG_FORM) {
        out.push(length as u8);
    } else {
        let bytes = length.to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(DER_LONG_FORM | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
}

fn put_element(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(tag);
    put_length(out, body.len());
    out.extend_from_slice(body);
}

fn put_unsigned(out: &mut Vec<u8>, magnitude: &[u8]) {
    let mut body = Vec::with_capacity(magnitude.len() + 1);
    if magnitude.first().is_some_and(|b| b & 0x80 != 0) {
        body.push(0);
    }
    body.extend_from_slice(magnitude);
    put_element(out, DER_INTEGER, &body);
}

/// A bare `RSAPublicKey`, the form signature verification reads.
pub fn rsa_public_key(key: &RsaPublicKey) -> Vec<u8> {
    let mut numbers = Vec::new();
    put_unsigned(&mut numbers, key.modulus());
    put_unsigned(&mut numbers, key.exponent());
    let mut out = Vec::new();
    put_element(&mut out, DER_SEQUENCE, &numbers);
    out
}
