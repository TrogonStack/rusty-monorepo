use crate::authentication_type::AuthenticationType;
use crate::issuer::Issuer;
use crate::name_claim_type::NameClaimType;
use crate::role_claim_type::RoleClaimType;

/// Explicit claim selection for ingest. Each selector names one claim type;
/// no name or role default, username fallback or group-to-role mapping is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestProfile {
    authentication_type: AuthenticationType,
    name_claim_type: NameClaimType,
    role_claim_type: RoleClaimType,
    default_issuer: Option<Issuer>,
}

impl IngestProfile {
    pub fn new(
        authentication_type: AuthenticationType,
        name_claim_type: NameClaimType,
        role_claim_type: RoleClaimType,
    ) -> Self {
        Self {
            authentication_type,
            name_claim_type,
            role_claim_type,
            default_issuer: None,
        }
    }

    pub fn with_default_issuer(mut self, issuer: Issuer) -> Self {
        self.default_issuer = Some(issuer);
        self
    }

    pub fn authentication_type(&self) -> &AuthenticationType {
        &self.authentication_type
    }

    pub fn name_claim_type(&self) -> &NameClaimType {
        &self.name_claim_type
    }

    pub fn role_claim_type(&self) -> &RoleClaimType {
        &self.role_claim_type
    }

    pub fn default_issuer(&self) -> Option<&Issuer> {
        self.default_issuer.as_ref()
    }
}
