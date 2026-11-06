use std::sync::Arc;

use thiserror::Error;

use crate::authentication_context::AuthenticationContext;
use crate::authentication_type::AuthenticationType;
use crate::authorized_actor::AuthorizedActor;
use crate::binding::Binding;
use crate::claim::Claim;
use crate::claim_set::{ClaimSet, HasSubject, SubjectRef};
use crate::claim_type::ClaimType;
use crate::claim_value::ClaimValue;
use crate::current_actor::CurrentActor;
use crate::delegation::Delegation;
use crate::dynamic_claims::DynamicClaims;
use crate::name_claim_type::NameClaimType;
use crate::principal_key::PrincipalKey;
use crate::principal_kind::PrincipalKind;
use crate::role_claim_type::RoleClaimType;
use crate::role_name::RoleName;
use crate::verified_binding::{BindingError, BindingMethod, Dpop, Method, MutualTls, VerifiedBinding};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("an anonymous identity cannot be classified")]
pub struct AnonymousIdentityError;

#[derive(Clone, Debug)]
pub(crate) struct IdentityParts<C: ClaimSet> {
    pub(crate) authentication_type: AuthenticationType,
    pub(crate) claims: C,
    pub(crate) name_claim_type: NameClaimType,
    pub(crate) role_claim_type: RoleClaimType,
    pub(crate) key: PrincipalKey,
    pub(crate) delegation: Delegation,
    pub(crate) binding: Binding,
    pub(crate) may_act: Option<AuthorizedActor>,
    pub(crate) context: AuthenticationContext,
}

#[derive(Clone, Debug)]
pub struct AuthenticatedIdentity<C: ClaimSet = DynamicClaims> {
    authentication_type: AuthenticationType,
    claims: C,
    name_claim_type: NameClaimType,
    role_claim_type: RoleClaimType,
    key: PrincipalKey,
    kind: Option<PrincipalKind>,
    delegation: Delegation,
    binding: Binding,
    may_act: Option<AuthorizedActor>,
    context: AuthenticationContext,
}

impl<C: ClaimSet> AuthenticatedIdentity<C> {
    pub(crate) fn from_parts(parts: IdentityParts<C>) -> Self {
        Self {
            authentication_type: parts.authentication_type,
            claims: parts.claims,
            name_claim_type: parts.name_claim_type,
            role_claim_type: parts.role_claim_type,
            key: parts.key,
            kind: None,
            delegation: parts.delegation,
            binding: parts.binding,
            may_act: parts.may_act,
            context: parts.context,
        }
    }

    pub fn authentication_type(&self) -> &AuthenticationType {
        &self.authentication_type
    }

    pub fn claims(&self) -> &C {
        &self.claims
    }

    pub fn name_claim_type(&self) -> &NameClaimType {
        &self.name_claim_type
    }

    pub fn role_claim_type(&self) -> &RoleClaimType {
        &self.role_claim_type
    }

    pub fn key(&self) -> &PrincipalKey {
        &self.key
    }

    pub fn kind(&self) -> Option<&PrincipalKind> {
        self.kind.as_ref()
    }

    pub fn delegation(&self) -> &Delegation {
        &self.delegation
    }

    pub fn actor(&self) -> Option<&Arc<CurrentActor>> {
        match &self.delegation {
            Delegation::Direct => None,
            Delegation::Delegated(actor) => Some(actor),
        }
    }

    pub fn binding(&self) -> &Binding {
        &self.binding
    }

    pub fn may_act(&self) -> Option<&AuthorizedActor> {
        self.may_act.as_ref()
    }

    pub fn context(&self) -> &AuthenticationContext {
        &self.context
    }

    pub fn verify_binding<M: Method>(&self, presented: &str) -> Result<VerifiedBinding, BindingError> {
        self.binding.verify::<M>(presented)
    }
}

impl AuthenticatedIdentity<DynamicClaims> {
    pub fn find_first(&self, claim_type: &ClaimType) -> Option<&Claim> {
        self.claims.find_first(claim_type)
    }

    pub fn find_all<'a>(&'a self, claim_type: &'a ClaimType) -> impl Iterator<Item = &'a Claim> + 'a {
        self.claims.find_all(claim_type)
    }

    pub fn has_claim(&self, claim_type: &ClaimType) -> bool {
        self.claims.has_claim(claim_type)
    }

    pub fn name(&self) -> Option<&str> {
        match self.claims.find_first(self.name_claim_type.claim_type())?.value() {
            ClaimValue::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// Queries only the selected role claim type. A collection is repeated
    /// non-empty text or one flat list of non-empty text. Malformed or
    /// partially projected collections never match.
    pub fn is_in_role(&self, role: &RoleName) -> bool {
        if self.claims.unprojected().contains(self.role_claim_type.claim_type()) {
            return false;
        }
        let mut values = self
            .claims
            .find_all(self.role_claim_type.claim_type())
            .map(Claim::value);
        let Some(first) = values.next() else {
            return false;
        };
        let match_role = |found: bool, value: &ClaimValue| match value {
            ClaimValue::Text(text) if !text.as_str().is_empty() => Some(found || text.as_str() == role.as_str()),
            _ => None,
        };
        match first {
            ClaimValue::List(items) => {
                if values.next().is_some() {
                    return false;
                }
                items.iter().try_fold(false, match_role).unwrap_or(false)
            }
            _ => std::iter::once(first)
                .chain(values)
                .try_fold(false, match_role)
                .unwrap_or(false),
        }
    }
}

impl<C: HasSubject> AuthenticatedIdentity<C> {
    pub fn subject(&self) -> SubjectRef<'_> {
        self.claims.subject()
    }
}

#[derive(Debug)]
pub enum Identity<C: ClaimSet = DynamicClaims> {
    Anonymous,
    Authenticated(Arc<AuthenticatedIdentity<C>>),
}

impl<C: ClaimSet> Identity<C> {
    pub fn anonymous() -> Self {
        Self::Anonymous
    }

    pub(crate) fn authenticated(parts: IdentityParts<C>) -> Self {
        Self::Authenticated(Arc::new(AuthenticatedIdentity::from_parts(parts)))
    }

    pub fn is_authenticated(&self) -> bool {
        matches!(self, Self::Authenticated(_))
    }

    pub fn as_authenticated(&self) -> Option<&AuthenticatedIdentity<C>> {
        match self {
            Self::Anonymous => None,
            Self::Authenticated(identity) => Some(identity),
        }
    }
}

impl<C: ClaimSet> Clone for Identity<C> {
    fn clone(&self) -> Self {
        match self {
            Self::Anonymous => Self::Anonymous,
            Self::Authenticated(identity) => Self::Authenticated(Arc::clone(identity)),
        }
    }
}

impl<C: ClaimSet + Clone> Identity<C> {
    /// Records supplied evidence only when it matches this identity's binding.
    /// The caller remains responsible for validating the underlying proof.
    pub fn try_with_verified_binding(mut self, verified: VerifiedBinding) -> Result<Self, BindingError> {
        let Self::Authenticated(identity) = &mut self else {
            return Err(BindingError::NotAsserted);
        };
        let matched = match verified.method() {
            BindingMethod::Dpop => identity.binding.verify::<Dpop>(verified.thumbprint())?,
            BindingMethod::MutualTls => identity.binding.verify::<MutualTls>(verified.thumbprint())?,
        };
        Arc::make_mut(identity).binding = Binding::Verified(Box::new(matched));
        Ok(self)
    }

    pub fn try_with_kind(mut self, kind: PrincipalKind) -> Result<Self, AnonymousIdentityError> {
        let Self::Authenticated(identity) = &mut self else {
            return Err(AnonymousIdentityError);
        };
        Arc::make_mut(identity).kind = Some(kind);
        Ok(self)
    }
}
