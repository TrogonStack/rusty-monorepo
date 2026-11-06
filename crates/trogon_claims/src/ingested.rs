use std::sync::Arc;

use crate::claim_set::ClaimSet;
use crate::current_actor::CurrentActor;
use crate::identity::{AuthenticatedIdentity, Identity};
use crate::principal::Principal;

/// An ingested identity whose current actor is always read from that identity.
/// Construction cannot attach an independent or contradictory actor value.
#[derive(Clone, Debug)]
pub struct Ingested<C: ClaimSet> {
    identity: Arc<AuthenticatedIdentity<C>>,
}

impl<C: ClaimSet> Ingested<C> {
    #[cfg(feature = "json")]
    pub(crate) fn new(identity: Arc<AuthenticatedIdentity<C>>) -> Self {
        Self { identity }
    }

    pub fn identity(&self) -> &AuthenticatedIdentity<C> {
        &self.identity
    }

    pub fn actor(&self) -> Option<&CurrentActor> {
        self.identity.actor().map(Arc::as_ref)
    }

    pub fn into_identity(self) -> Identity<C> {
        Identity::Authenticated(self.identity)
    }
}

impl<C: ClaimSet> Ingested<C> {
    pub fn into_principal(self) -> Principal<C> {
        Principal::new(self.into_identity())
    }
}

impl<C: ClaimSet> From<Ingested<C>> for Principal<C> {
    fn from(ingested: Ingested<C>) -> Self {
        ingested.into_principal()
    }
}
