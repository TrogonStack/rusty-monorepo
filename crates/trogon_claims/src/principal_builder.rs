use thiserror::Error;

use crate::claim_set::ClaimSet;
use crate::dynamic_claims::DynamicClaims;
use crate::identity::Identity;
use crate::principal::Principal;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum PrincipalBuildError {
    #[error("primary index {index} is outside the {len} identities")]
    PrimaryOutOfRange { index: usize, len: usize },
    #[error("the designated primary identity is anonymous")]
    PrimaryAnonymous,
}

#[derive(Debug)]
pub struct PrincipalBuilder<C: ClaimSet = DynamicClaims> {
    identities: Vec<Identity<C>>,
    primary: Option<usize>,
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

    pub fn primary(mut self, index: usize) -> Self {
        self.primary = Some(index);
        self
    }

    pub fn try_build(self) -> Result<Principal<C>, PrincipalBuildError> {
        let Self { identities, primary } = self;
        let len = identities.len();
        let chosen = match primary {
            Some(index) => {
                let chosen = identities
                    .get(index)
                    .ok_or(PrincipalBuildError::PrimaryOutOfRange { index, len })?;
                if !chosen.is_authenticated() {
                    return Err(PrincipalBuildError::PrimaryAnonymous);
                }
                chosen.clone()
            }
            None => identities
                .iter()
                .find(|identity| identity.is_authenticated())
                .or_else(|| identities.first())
                .cloned()
                .unwrap_or(Identity::Anonymous),
        };
        Ok(Principal::from_selected(identities, chosen))
    }
}
