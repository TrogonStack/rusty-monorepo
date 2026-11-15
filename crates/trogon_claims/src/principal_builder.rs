use crate::claim_set::ClaimSet;
use crate::dynamic_claims::DynamicClaims;
use crate::identity::Identity;
use crate::principal::{PrimaryIdentityIndex, Principal, PrincipalBuildError};

/// Composes identities without choosing an authenticated primary implicitly.
/// A single identity is unambiguous; multiple identities require `primary`.
#[derive(Debug)]
pub struct PrincipalBuilder<C: ClaimSet = DynamicClaims> {
    identities: Vec<Identity<C>>,
    primary: Option<PrimaryIdentityIndex>,
}

impl<C: ClaimSet> PrincipalBuilder<C> {
    pub fn new(identity: Identity<C>) -> Self {
        Self {
            identities: vec![identity],
            primary: None,
        }
    }

    pub fn push(mut self, identity: Identity<C>) -> Self {
        self.identities.push(identity);
        self
    }

    pub fn primary(mut self, index: PrimaryIdentityIndex) -> Self {
        self.primary = Some(index);
        self
    }

    pub fn try_build(self) -> Result<Principal<C>, PrincipalBuildError> {
        let Self { identities, primary } = self;
        let len = identities.len();
        let chosen = match primary {
            Some(index) => {
                let index = index.get();
                let chosen = identities
                    .get(index)
                    .ok_or(PrincipalBuildError::PrimaryOutOfRange { index, len })?;
                if !chosen.is_authenticated() {
                    return Err(PrincipalBuildError::PrimaryAnonymous);
                }
                chosen.clone()
            }
            None if len == 1 => identities[0].clone(),
            None => return Err(PrincipalBuildError::MissingPrimary),
        };
        Ok(Principal::from_selected(identities, chosen))
    }
}
