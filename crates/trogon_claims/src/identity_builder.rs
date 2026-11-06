use thiserror::Error;

use crate::authentication_context::AuthenticationContext;
use crate::authentication_type::AuthenticationType;
use crate::authorized_actor::AuthorizedActor;
use crate::binding::Binding;
use crate::claim::Claim;
use crate::claim_type::ClaimType;
use crate::claim_value::ClaimValue;
use crate::current_actor::CurrentActor;
use crate::delegation::Delegation;
use crate::dynamic_claims::DynamicClaims;
use crate::errors::ValueObjectError;
use crate::identity::{Identity, IdentityParts};
use crate::issuer::Issuer;
use crate::name_claim_type::NameClaimType;
use crate::principal_key::PrincipalKey;
use crate::role_claim_type::RoleClaimType;
use crate::subject_id::SubjectId;
use crate::well_known_claim::WellKnownClaim;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum IdentityBuildError {
    #[error("identity has no sub claim")]
    MissingSubject,
    #[error("identity sub claim is not textual")]
    InvalidSubjectType,
    #[error("identity has duplicate sub claims")]
    DuplicateSubject,
    #[error("identity has no iss claim or explicitly configured issuer")]
    MissingIssuer,
    #[error("identity iss claim is not textual")]
    InvalidIssuerType,
    #[error("identity has duplicate iss claims")]
    DuplicateIssuer,
    #[error("identity claim is invalid: {0}")]
    Invalid(#[from] ValueObjectError),
}

/// Builds an identity with explicit name and role claim selections.
/// Each selection names one claim type, without defaults or fallback chains.
#[derive(Clone, Debug)]
pub struct IdentityBuilder {
    authentication_type: AuthenticationType,
    claims: Vec<Claim>,
    name_claim_type: NameClaimType,
    role_claim_type: RoleClaimType,
    actor: Option<CurrentActor>,
    binding: Binding,
    may_act: Option<AuthorizedActor>,
    context: AuthenticationContext,
    default_issuer: Option<Issuer>,
}

impl IdentityBuilder {
    pub fn new(
        authentication_type: AuthenticationType,
        name_claim_type: NameClaimType,
        role_claim_type: RoleClaimType,
    ) -> Self {
        Self {
            authentication_type,
            claims: Vec::new(),
            name_claim_type,
            role_claim_type,
            actor: None,
            binding: Binding::Bearer,
            may_act: None,
            context: AuthenticationContext::default(),
            default_issuer: None,
        }
    }

    pub fn push(mut self, claim: Claim) -> Self {
        self.claims.push(claim);
        self
    }

    /// Supplies an issuer only when no `iss` claim is present.
    /// Malformed or duplicate explicit issuer claims are always rejected.
    pub fn with_default_issuer(mut self, issuer: Issuer) -> Self {
        self.default_issuer = Some(issuer);
        self
    }

    pub fn actor(mut self, actor: CurrentActor) -> Self {
        self.actor = Some(actor);
        self
    }

    pub fn binding(mut self, binding: Binding) -> Self {
        self.binding = binding;
        self
    }

    pub fn may_act(mut self, may_act: AuthorizedActor) -> Self {
        self.may_act = Some(may_act);
        self
    }

    pub fn context(mut self, context: AuthenticationContext) -> Self {
        self.context = context;
        self
    }

    pub fn try_build(self) -> Result<Identity<DynamicClaims>, IdentityBuildError> {
        let sub_type: ClaimType = WellKnownClaim::Sub.into();
        let iss_type: ClaimType = WellKnownClaim::Iss.into();
        let mut subjects = self.claims.iter().filter(|claim| claim.claim_type() == &sub_type);
        let sub_claim = subjects.next().ok_or(IdentityBuildError::MissingSubject)?;
        if subjects.next().is_some() {
            return Err(IdentityBuildError::DuplicateSubject);
        }
        let ClaimValue::Text(sub_text) = sub_claim.value() else {
            return Err(IdentityBuildError::InvalidSubjectType);
        };
        let sub = SubjectId::try_new(sub_text.as_str().to_owned())?;
        let mut issuers = self.claims.iter().filter(|claim| claim.claim_type() == &iss_type);
        let explicit_issuer = issuers.next();
        if issuers.next().is_some() {
            return Err(IdentityBuildError::DuplicateIssuer);
        }
        let issuer = match explicit_issuer {
            Some(claim) => match claim.value() {
                ClaimValue::Text(text) => Issuer::try_new(text.as_str().to_owned())?,
                _ => return Err(IdentityBuildError::InvalidIssuerType),
            },
            None => self.default_issuer.ok_or(IdentityBuildError::MissingIssuer)?,
        };
        let delegation = match self.actor {
            Some(actor) => Delegation::Delegated(std::sync::Arc::new(actor)),
            None => Delegation::Direct,
        };
        Ok(Identity::authenticated(IdentityParts {
            authentication_type: self.authentication_type,
            claims: DynamicClaims::from_claims(self.claims),
            name_claim_type: self.name_claim_type,
            role_claim_type: self.role_claim_type,
            key: PrincipalKey::new(issuer, sub),
            delegation,
            binding: self.binding,
            may_act: self.may_act,
            context: self.context,
        }))
    }
}
