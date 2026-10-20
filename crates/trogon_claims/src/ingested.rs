use std::sync::Arc;

use crate::claim_set::ClaimSet;
use crate::current_actor::CurrentActor;
use crate::identity::{AuthenticatedIdentity, Identity};

#[derive(Clone, Debug)]
pub enum Ingested<C: ClaimSet> {
    Direct(Arc<AuthenticatedIdentity<C>>),
    Delegated {
        identity: Arc<AuthenticatedIdentity<C>>,
        actor: Arc<CurrentActor>,
    },
}

impl<C: ClaimSet> Ingested<C> {
    pub fn identity(&self) -> &AuthenticatedIdentity<C> {
        match self {
            Self::Direct(identity) | Self::Delegated { identity, .. } => identity,
        }
    }

    pub fn actor(&self) -> Option<&CurrentActor> {
        match self {
            Self::Direct(_) => None,
            Self::Delegated { actor, .. } => Some(actor),
        }
    }

    pub fn into_identity(self) -> Identity<C> {
        match self {
            Self::Direct(identity) | Self::Delegated { identity, .. } => Identity::Authenticated(identity),
        }
    }
}
