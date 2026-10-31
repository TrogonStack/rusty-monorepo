use crate::authentication_type::AuthenticationType;
use crate::claim_type::ClaimType;
use crate::issuer::Issuer;
use crate::well_known_claim::WellKnownClaim;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestProfile {
    authentication_type: AuthenticationType,
    name_claim_type: ClaimType,
    role_claim_type: ClaimType,
    default_issuer: Option<Issuer>,
}

impl IngestProfile {
    pub fn new(authentication_type: AuthenticationType) -> Self {
        Self {
            authentication_type,
            name_claim_type: WellKnownClaim::WsName.into(),
            role_claim_type: WellKnownClaim::WsRole.into(),
            default_issuer: None,
        }
    }

    pub fn with_name_claim_type(mut self, claim_type: ClaimType) -> Self {
        self.name_claim_type = claim_type;
        self
    }

    pub fn with_role_claim_type(mut self, claim_type: ClaimType) -> Self {
        self.role_claim_type = claim_type;
        self
    }

    pub fn with_default_issuer(mut self, issuer: Issuer) -> Self {
        self.default_issuer = Some(issuer);
        self
    }

    pub fn authentication_type(&self) -> &AuthenticationType {
        &self.authentication_type
    }

    pub fn name_claim_type(&self) -> &ClaimType {
        &self.name_claim_type
    }

    pub fn role_claim_type(&self) -> &ClaimType {
        &self.role_claim_type
    }

    pub fn default_issuer(&self) -> Option<&Issuer> {
        self.default_issuer.as_ref()
    }
}
