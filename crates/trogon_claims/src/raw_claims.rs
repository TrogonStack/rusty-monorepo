use std::borrow::Cow;
use std::collections::HashSet;

use serde_json::value::RawValue;

use crate::claim_type::ClaimType;
use crate::raw_claims_error::RawClaimsError;
use crate::raw_json::{RawJson, RawKind};
use crate::raw_scan::object_members;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawClaims(Vec<(ClaimType, RawJson)>);

impl RawClaims {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, RawClaimsError> {
        let document: &RawValue = serde_json::from_slice(bytes).map_err(|error| RawClaimsError::InvalidJson {
            line: error.line(),
            column: error.column(),
        })?;
        Self::from_validated(document.get())
    }

    pub fn from_object(value: &RawJson) -> Result<Self, RawClaimsError> {
        Self::from_validated(value.as_str())
    }

    pub(crate) fn from_validated(text: &str) -> Result<Self, RawClaimsError> {
        let text = text.trim();
        if !text.starts_with('{') {
            return Err(RawClaimsError::NotObject);
        }
        let members = object_members(text).ok_or(RawClaimsError::InvalidJson { line: 0, column: 0 })?;
        let mut claims = Vec::with_capacity(members.len());
        for (key, value) in members {
            let name = member_name(key)?;
            if name.is_empty() {
                return Err(RawClaimsError::EmptyName);
            }
            let claim_type = ClaimType::try_from_member_name(&name).map_err(|_| RawClaimsError::EmptyName)?;
            claims.push((claim_type, RawJson::from_validated(value)));
        }
        let mut seen = HashSet::with_capacity(claims.len());
        for (claim_type, _) in &claims {
            if !seen.insert(claim_type.as_str()) {
                return Err(RawClaimsError::Duplicate {
                    name: claim_type.as_str().to_owned(),
                });
            }
        }
        Ok(Self(claims))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ClaimType, &RawJson)> {
        self.0.iter().map(|(claim_type, value)| (claim_type, value))
    }

    pub fn get(&self, claim_type: &ClaimType) -> Option<&RawJson> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == claim_type)
            .map(|(_, value)| value)
    }

    pub fn get_named(&self, name: &str) -> Option<&RawJson> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate.as_str() == name)
            .map(|(_, value)| value)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn kind_of(&self, name: &str) -> Option<RawKind> {
        self.get_named(name).map(RawJson::kind)
    }
}

fn member_name(key: &str) -> Result<Cow<'_, str>, RawClaimsError> {
    if key.contains('\\') {
        return serde_json::from_str::<String>(key)
            .map(Cow::Owned)
            .map_err(|_| RawClaimsError::InvalidJson { line: 0, column: 0 });
    }
    key.get(1..key.len().saturating_sub(1))
        .map(Cow::Borrowed)
        .ok_or(RawClaimsError::InvalidJson { line: 0, column: 0 })
}
