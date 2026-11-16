use crate::claim_type::ClaimType;

/// The single claim type selected for principal names, without fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameClaimType(ClaimType);

impl NameClaimType {
    pub fn new(claim_type: ClaimType) -> Self {
        Self(claim_type)
    }

    pub fn claim_type(&self) -> &ClaimType {
        &self.0
    }
}
