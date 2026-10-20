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
use crate::principal_key::PrincipalKey;
use crate::subject_id::SubjectId;
use crate::well_known_claim::WellKnownClaim;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum IdentityBuildError {
    #[error("identity has no textual sub claim")]
    MissingSubject,
    #[error("identity claim is invalid: {0}")]
    Invalid(#[from] ValueObjectError),
}

#[derive(Clone, Debug)]
pub struct IdentityBuilder {
    authentication_type: AuthenticationType,
    claims: Vec<Claim>,
    name_claim_type: ClaimType,
    role_claim_type: ClaimType,
    actor: Option<CurrentActor>,
    binding: Binding,
    may_act: Option<AuthorizedActor>,
    context: AuthenticationContext,
}

impl IdentityBuilder {
    pub fn new(authentication_type: AuthenticationType) -> Self {
        Self {
            authentication_type,
            claims: Vec::new(),
            name_claim_type: WellKnownClaim::WsName.into(),
            role_claim_type: WellKnownClaim::WsRole.into(),
            actor: None,
            binding: Binding::Bearer,
            may_act: None,
            context: AuthenticationContext::default(),
        }
    }

    pub fn push(mut self, claim: Claim) -> Self {
        self.claims.push(claim);
        self
    }

    pub fn name_claim_type(mut self, claim_type: ClaimType) -> Self {
        self.name_claim_type = claim_type;
        self
    }

    pub fn role_claim_type(mut self, claim_type: ClaimType) -> Self {
        self.role_claim_type = claim_type;
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
        let sub_claim = self
            .claims
            .iter()
            .find(|claim| claim.claim_type() == &sub_type)
            .ok_or(IdentityBuildError::MissingSubject)?;
        let ClaimValue::Text(sub_text) = sub_claim.value() else {
            return Err(IdentityBuildError::MissingSubject);
        };
        let sub = SubjectId::try_new(sub_text.as_str().to_owned())?;
        let issuer = match self.claims.iter().find(|claim| claim.claim_type() == &iss_type) {
            Some(claim) => match claim.value() {
                ClaimValue::Text(text) => Issuer::try_new(text.as_str().to_owned())?,
                _ => sub_claim.issuer().clone(),
            },
            None => sub_claim.issuer().clone(),
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
