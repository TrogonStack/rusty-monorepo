use std::borrow::Cow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use trogon_claims_syntax::validate_non_empty;

use crate::errors::{ValueKind, ValueObjectError};
use crate::well_known_claim::WellKnownClaim;

#[derive(Clone, Debug)]
enum Repr {
    WellKnown(WellKnownClaim),
    Custom(Cow<'static, str>),
}

#[derive(Clone, Debug)]
pub struct ClaimType(Repr);

impl ClaimType {
    pub fn try_new(name: impl Into<Cow<'static, str>>) -> Result<Self, ValueObjectError> {
        let name = name.into();
        validate_non_empty(&name).map_err(|violation| ValueObjectError::new(ValueKind::ClaimType, violation))?;
        Ok(match WellKnownClaim::from_name(&name) {
            Some(well_known) => Self(Repr::WellKnown(well_known)),
            None => Self(Repr::Custom(name)),
        })
    }

    #[cfg(feature = "json")]
    pub(crate) const fn from_static_unchecked(name: &'static str) -> Self {
        Self(Repr::Custom(Cow::Borrowed(name)))
    }

    #[cfg(feature = "json")]
    pub(crate) fn try_from_member_name(name: &str) -> Result<Self, ValueObjectError> {
        match WellKnownClaim::from_name(name) {
            Some(well_known) => Ok(Self(Repr::WellKnown(well_known))),
            None => Self::try_new(name.to_owned()),
        }
    }

    pub fn as_str(&self) -> &str {
        match &self.0 {
            Repr::WellKnown(well_known) => well_known.as_str(),
            Repr::Custom(name) => name,
        }
    }

    pub fn well_known(&self) -> Option<WellKnownClaim> {
        match &self.0 {
            Repr::WellKnown(well_known) => Some(*well_known),
            Repr::Custom(name) => WellKnownClaim::from_name(name),
        }
    }
}

impl PartialEq for ClaimType {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::WellKnown(left), Repr::WellKnown(right)) => left == right,
            _ => self.as_str() == other.as_str(),
        }
    }
}

impl Eq for ClaimType {}

impl Hash for ClaimType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl From<WellKnownClaim> for ClaimType {
    fn from(well_known: WellKnownClaim) -> Self {
        Self(Repr::WellKnown(well_known))
    }
}

impl fmt::Display for ClaimType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ClaimType {
    type Err = ValueObjectError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::try_new(name.to_owned())
    }
}

impl TryFrom<&str> for ClaimType {
    type Error = ValueObjectError;

    fn try_from(name: &str) -> Result<Self, Self::Error> {
        Self::try_new(name.to_owned())
    }
}

impl TryFrom<String> for ClaimType {
    type Error = ValueObjectError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::try_new(name)
    }
}

#[cfg(test)]
mod tests;
