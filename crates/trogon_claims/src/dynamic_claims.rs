use crate::claim::Claim;
use crate::claim_set::ClaimSet;
use crate::claim_type::ClaimType;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DynamicClaims {
    pub(crate) claims: Vec<Claim>,
    pub(crate) unprojected: Vec<ClaimType>,
}

impl DynamicClaims {
    pub fn from_claims(claims: Vec<Claim>) -> Self {
        Self {
            claims,
            unprojected: Vec::new(),
        }
    }

    pub fn claims(&self) -> &[Claim] {
        &self.claims
    }

    pub fn find_first(&self, claim_type: &ClaimType) -> Option<&Claim> {
        self.claims.iter().find(|claim| claim.claim_type() == claim_type)
    }

    pub fn find_all<'a>(&'a self, claim_type: &'a ClaimType) -> impl Iterator<Item = &'a Claim> + 'a {
        self.claims.iter().filter(move |claim| claim.claim_type() == claim_type)
    }

    pub fn has_claim(&self, claim_type: &ClaimType) -> bool {
        self.find_first(claim_type).is_some()
    }

    pub fn unprojected(&self) -> &[ClaimType] {
        &self.unprojected
    }
}

impl ClaimSet for DynamicClaims {}
