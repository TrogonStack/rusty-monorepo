use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use crate::authentication_context::AuthenticationContext;
use crate::authentication_type::AuthenticationType;
use crate::claim::Claim;
use crate::claim_set::{ClaimSet, HasSubject, SubjectRef};
use crate::claim_type::ClaimType;
use crate::current_actor::CurrentActor;
use crate::delegation::Delegation;
use crate::dynamic_claims::DynamicClaims;
use crate::identity::{AuthenticatedIdentity, Identity};
use crate::kind::{Classified, Kind, Unclassified};
use crate::principal::{Principal, SourcedClaim};
use crate::principal_key::PrincipalKey;
use crate::principal_kind::PrincipalKind;
use crate::refinement::{
    AnyBinding, AnyDelegation, BindingState, BoundProof, Delegated, DelegationState, Direct, KindState, KnownKind,
    Refined, Refinement, WithBinding, WithDelegation, WithKind,
};
use crate::role_name::RoleName;
use crate::verified_binding::{BindingError, Method};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("the principal has no authenticated primary identity")]
pub struct NotAuthenticatedError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RefinementError {
    #[error("the primary identity carries no application classification")]
    Unclassified,
    #[error("the primary identity is classified as another kind")]
    KindMismatch,
    #[error("the primary identity is not acting on behalf of a delegate")]
    NotDelegated,
    #[error("the primary identity is acting through a delegate")]
    NotDirect,
}

pub struct AuthenticatedPrincipal<C: ClaimSet = DynamicClaims, R: Refinement = Refined> {
    principal: Arc<Principal<C>>,
    primary: Arc<AuthenticatedIdentity<C>>,
    kind: <R::Kind as KindState>::Value,
    actor: <R::Delegation as DelegationState>::Actor,
    binding: <R::Binding as BindingState>::Proof,
}

impl<C: ClaimSet> AuthenticatedPrincipal<C> {
    pub fn try_new(principal: Arc<Principal<C>>) -> Result<Self, NotAuthenticatedError> {
        let primary = match principal.primary() {
            Identity::Anonymous => return Err(NotAuthenticatedError),
            Identity::Authenticated(identity) => Arc::clone(identity),
        };
        let actor = primary.actor().cloned();
        let binding = primary.binding().clone();
        Ok(Self {
            principal,
            primary,
            kind: (),
            actor,
            binding,
        })
    }
}

impl<C: ClaimSet, R: Refinement> AuthenticatedPrincipal<C, R> {
    pub fn primary(&self) -> &AuthenticatedIdentity<C> {
        &self.primary
    }

    pub fn authentication_type(&self) -> &AuthenticationType {
        self.primary.authentication_type()
    }

    pub fn key(&self) -> &PrincipalKey {
        self.primary.key()
    }

    pub fn primary_claims(&self) -> &C {
        self.primary.claims()
    }

    pub fn delegation(&self) -> &Delegation {
        self.primary.delegation()
    }

    pub fn context(&self) -> &AuthenticationContext {
        self.primary.context()
    }

    pub fn binding(&self) -> &<R::Binding as BindingState>::Proof {
        &self.binding
    }

    pub fn authenticated_identities(&self) -> impl Iterator<Item = &AuthenticatedIdentity<C>> {
        self.principal.authenticated_identities()
    }
}

impl<C: ClaimSet, R: Refinement> AuthenticatedPrincipal<C, R>
where
    R::Kind: KnownKind,
{
    pub fn kind(&self) -> &PrincipalKind {
        <R::Kind as KnownKind>::resolve(&self.kind)
    }
}

impl<C: ClaimSet, R: Refinement<Delegation = Delegated>> AuthenticatedPrincipal<C, R> {
    pub fn current_actor(&self) -> &CurrentActor {
        &self.actor
    }
}

impl<C: ClaimSet, R: Refinement<Kind = Unclassified>> AuthenticatedPrincipal<C, R> {
    pub fn try_kind<K: Kind>(self) -> Result<AuthenticatedPrincipal<C, WithKind<R, K>>, RefinementError> {
        match self.primary.kind() {
            None => Err(RefinementError::Unclassified),
            Some(kind) if kind == K::KIND => Ok(AuthenticatedPrincipal {
                principal: self.principal,
                primary: self.primary,
                kind: (),
                actor: self.actor,
                binding: self.binding,
            }),
            Some(_) => Err(RefinementError::KindMismatch),
        }
    }

    pub fn try_classified(self) -> Result<AuthenticatedPrincipal<C, WithKind<R, Classified>>, RefinementError> {
        let kind = self.primary.kind().cloned().ok_or(RefinementError::Unclassified)?;
        Ok(AuthenticatedPrincipal {
            principal: self.principal,
            primary: self.primary,
            kind,
            actor: self.actor,
            binding: self.binding,
        })
    }
}

impl<C: ClaimSet, R: Refinement<Delegation = AnyDelegation>> AuthenticatedPrincipal<C, R> {
    pub fn try_delegated(self) -> Result<AuthenticatedPrincipal<C, WithDelegation<R, Delegated>>, RefinementError> {
        let actor = self.actor.ok_or(RefinementError::NotDelegated)?;
        Ok(AuthenticatedPrincipal {
            principal: self.principal,
            primary: self.primary,
            kind: self.kind,
            actor,
            binding: self.binding,
        })
    }

    pub fn try_direct(self) -> Result<AuthenticatedPrincipal<C, WithDelegation<R, Direct>>, RefinementError> {
        match self.actor {
            Some(_) => Err(RefinementError::NotDirect),
            None => Ok(AuthenticatedPrincipal {
                principal: self.principal,
                primary: self.primary,
                kind: self.kind,
                actor: (),
                binding: self.binding,
            }),
        }
    }
}

impl<C: ClaimSet, R: Refinement<Binding = AnyBinding>> AuthenticatedPrincipal<C, R> {
    pub fn try_bind<M: Method>(
        self,
        presented: &str,
    ) -> Result<AuthenticatedPrincipal<C, WithBinding<R, M>>, BindingError> {
        let verified = self.binding.verify::<M>(presented)?;
        Ok(AuthenticatedPrincipal {
            principal: self.principal,
            primary: self.primary,
            kind: self.kind,
            actor: self.actor,
            binding: BoundProof::new(verified),
        })
    }
}

impl<C: HasSubject, R: Refinement> AuthenticatedPrincipal<C, R> {
    pub fn subject(&self) -> SubjectRef<'_> {
        self.primary.subject()
    }
}

impl<R: Refinement> AuthenticatedPrincipal<DynamicClaims, R> {
    pub fn claims(&self) -> impl Iterator<Item = SourcedClaim<'_>> {
        self.principal.claims()
    }

    pub fn find_first(&self, claim_type: &ClaimType) -> Option<&Claim> {
        self.principal.find_first(claim_type)
    }

    pub fn find_all<'a>(&'a self, claim_type: &'a ClaimType) -> impl Iterator<Item = &'a Claim> + 'a {
        self.principal.find_all(claim_type)
    }

    pub fn has_claim(&self, claim_type: &ClaimType) -> bool {
        self.principal.has_claim(claim_type)
    }

    pub fn name(&self) -> Option<&str> {
        self.primary.name()
    }

    pub fn is_in_role(&self, role: &RoleName) -> bool {
        self.principal.is_in_role(role)
    }
}

impl<C: ClaimSet, R: Refinement> Clone for AuthenticatedPrincipal<C, R>
where
    <R::Kind as KindState>::Value: Clone,
    <R::Delegation as DelegationState>::Actor: Clone,
    <R::Binding as BindingState>::Proof: Clone,
{
    fn clone(&self) -> Self {
        Self {
            principal: Arc::clone(&self.principal),
            primary: Arc::clone(&self.primary),
            kind: self.kind.clone(),
            actor: self.actor.clone(),
            binding: self.binding.clone(),
        }
    }
}

impl<C: ClaimSet, R: Refinement> fmt::Debug for AuthenticatedPrincipal<C, R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthenticatedPrincipal")
            .field("key", self.primary.key())
            .field("authentication_type", self.primary.authentication_type())
            .finish()
    }
}
