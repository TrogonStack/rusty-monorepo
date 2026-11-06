use crate::claim::Claim;
use crate::claim_set::ClaimSet;
use crate::claim_type::ClaimType;
use crate::dynamic_claims::DynamicClaims;
use crate::identity::{AuthenticatedIdentity, Identity};
use crate::principal_builder::{PrimaryIdentityIndex, PrincipalBuildError};
use crate::role_name::RoleName;

#[derive(Debug)]
pub struct Principal<C: ClaimSet = DynamicClaims> {
    identities: Vec<Identity<C>>,
    primary: Identity<C>,
}

impl<C: ClaimSet> Clone for Principal<C> {
    fn clone(&self) -> Self {
        Self {
            identities: self.identities.clone(),
            primary: self.primary.clone(),
        }
    }
}

#[derive(Debug)]
pub struct SourcedClaim<'a> {
    identity: &'a AuthenticatedIdentity<DynamicClaims>,
    claim: &'a Claim,
}

impl<'a> SourcedClaim<'a> {
    pub fn identity(&self) -> &'a AuthenticatedIdentity<DynamicClaims> {
        self.identity
    }

    pub fn claim(&self) -> &'a Claim {
        self.claim
    }
}

impl<C: ClaimSet> Principal<C> {
    pub(crate) fn from_selected(identities: Vec<Identity<C>>, primary: Identity<C>) -> Self {
        Self { identities, primary }
    }

    pub fn anonymous() -> Self {
        Self::new(Identity::Anonymous)
    }

    pub fn new(identity: Identity<C>) -> Self {
        Self {
            identities: vec![identity.clone()],
            primary: identity,
        }
    }

    pub fn primary(&self) -> &Identity<C> {
        &self.primary
    }

    pub fn identities(&self) -> &[Identity<C>] {
        &self.identities
    }

    pub fn is_authenticated(&self) -> bool {
        self.primary.is_authenticated()
    }

    pub fn authenticated_identities(&self) -> impl Iterator<Item = &AuthenticatedIdentity<C>> {
        self.identities.iter().filter_map(Identity::as_authenticated)
    }

    /// Appends an identity while preserving the currently selected primary.
    /// Use `try_with_primary` to select another authenticated identity explicitly.
    pub fn with_identity(mut self, identity: Identity<C>) -> Self {
        self.identities.push(identity);
        self
    }

    /// Selects an existing authenticated identity as primary.
    pub fn try_with_primary(mut self, index: PrimaryIdentityIndex) -> Result<Self, PrincipalBuildError> {
        let index = index.get();
        let chosen = self
            .identities
            .get(index)
            .ok_or(PrincipalBuildError::PrimaryOutOfRange {
                index,
                len: self.identities.len(),
            })?;
        if !chosen.is_authenticated() {
            return Err(PrincipalBuildError::PrimaryAnonymous);
        }
        self.primary = chosen.clone();
        Ok(self)
    }
}

impl Principal<DynamicClaims> {
    pub fn claims(&self) -> impl Iterator<Item = SourcedClaim<'_>> {
        self.authenticated_identities().flat_map(|identity| {
            identity
                .claims()
                .claims()
                .iter()
                .map(move |claim| SourcedClaim { identity, claim })
        })
    }

    /// Finds the first matching assertion in identity order, preserving its source.
    pub fn find_first(&self, claim_type: &ClaimType) -> Option<SourcedClaim<'_>> {
        self.authenticated_identities().find_map(|identity| {
            identity
                .find_first(claim_type)
                .map(|claim| SourcedClaim { identity, claim })
        })
    }

    pub fn find_all<'a>(&'a self, claim_type: &'a ClaimType) -> impl Iterator<Item = SourcedClaim<'a>> + 'a {
        self.authenticated_identities().flat_map(move |identity| {
            identity
                .find_all(claim_type)
                .map(move |claim| SourcedClaim { identity, claim })
        })
    }

    pub fn has_claim(&self, claim_type: &ClaimType) -> bool {
        self.find_first(claim_type).is_some()
    }

    pub fn name(&self) -> Option<&str> {
        self.primary.as_authenticated().and_then(AuthenticatedIdentity::name)
    }

    pub fn is_in_role(&self, role: &RoleName) -> bool {
        self.authenticated_identities()
            .any(|identity| identity.is_in_role(role))
    }
}
