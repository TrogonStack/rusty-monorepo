use std::fmt;

use crate::claim_property_key::ClaimPropertyKey;
use crate::claim_text::ClaimText;
use crate::jwk::Jwk;

#[derive(Clone, PartialEq)]
pub struct Confirmation {
    pub(crate) jwk: Option<Jwk>,
    pub(crate) jwe: Option<ClaimText>,
    pub(crate) jku: Option<ClaimText>,
    pub(crate) kid: Option<ClaimText>,
    pub(crate) x5t_s256: Option<ClaimText>,
    pub(crate) jkt: Option<ClaimText>,
    pub(crate) other: Vec<(ClaimPropertyKey, ClaimText)>,
}

impl Confirmation {
    pub fn jwk(&self) -> Option<&Jwk> {
        self.jwk.as_ref()
    }

    pub fn jku(&self) -> Option<&ClaimText> {
        self.jku.as_ref()
    }

    pub fn kid(&self) -> Option<&ClaimText> {
        self.kid.as_ref()
    }

    pub fn x5t_s256(&self) -> Option<&ClaimText> {
        self.x5t_s256.as_ref()
    }

    pub fn jkt(&self) -> Option<&ClaimText> {
        self.jkt.as_ref()
    }

    pub fn has_jwe(&self) -> bool {
        self.jwe.is_some()
    }

    pub fn other_members(&self) -> &[(ClaimPropertyKey, ClaimText)] {
        &self.other
    }
}

impl fmt::Debug for Confirmation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Confirmation")
            .field("jwk", &self.jwk)
            .field("jwe", &self.jwe.as_ref().map(|_| "<redacted>"))
            .field("jku", &self.jku)
            .field("kid", &self.kid)
            .field("x5t#S256", &self.x5t_s256)
            .field("jkt", &self.jkt)
            .field("other", &format_args!("<{} members>", self.other.len()))
            .finish()
    }
}
