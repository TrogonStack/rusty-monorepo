use std::fmt;

use crate::claim_property_key::ClaimPropertyKey;
use crate::claim_text::ClaimText;

#[derive(Clone, PartialEq)]
pub struct Jwk {
    pub(crate) kty: ClaimText,
    pub(crate) kid: Option<ClaimText>,
    pub(crate) alg: Option<ClaimText>,
    pub(crate) key_use: Option<ClaimText>,
    pub(crate) crv: Option<ClaimText>,
    pub(crate) members: Vec<(ClaimPropertyKey, ClaimText)>,
}

impl Jwk {
    pub fn kty(&self) -> &ClaimText {
        &self.kty
    }

    pub fn kid(&self) -> Option<&ClaimText> {
        self.kid.as_ref()
    }

    pub fn alg(&self) -> Option<&ClaimText> {
        self.alg.as_ref()
    }

    pub fn key_use(&self) -> Option<&ClaimText> {
        self.key_use.as_ref()
    }

    pub fn crv(&self) -> Option<&ClaimText> {
        self.crv.as_ref()
    }

    pub fn members(&self) -> &[(ClaimPropertyKey, ClaimText)] {
        &self.members
    }
}

impl fmt::Debug for Jwk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Jwk")
            .field("kty", &self.kty)
            .field("kid", &self.kid)
            .field("alg", &self.alg)
            .field("use", &self.key_use)
            .field("crv", &self.crv)
            .field("members", &format_args!("<{} redacted>", self.members.len()))
            .finish()
    }
}
