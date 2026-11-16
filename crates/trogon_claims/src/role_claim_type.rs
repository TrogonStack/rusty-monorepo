use crate::claim_type::ClaimType;

/// The single claim type selected for role assertions, without combining groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleClaimType(ClaimType);

impl RoleClaimType {
    pub fn new(claim_type: ClaimType) -> Self {
        Self(claim_type)
    }

    pub fn claim_type(&self) -> &ClaimType {
        &self.0
    }
}
