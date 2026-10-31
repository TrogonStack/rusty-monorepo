use std::ops::Deref;
use std::sync::Arc;

use crate::authenticated_principal::{AuthenticatedPrincipal, NotAuthenticatedError};
use crate::claim_set::ClaimSet;
use crate::dynamic_claims::DynamicClaims;
use crate::principal::Principal;

#[derive(Debug)]
pub struct CurrentPrincipal<C: ClaimSet = DynamicClaims>(Arc<Principal<C>>);

impl<C: ClaimSet> CurrentPrincipal<C> {
    pub fn new(principal: Arc<Principal<C>>) -> Self {
        Self(principal)
    }

    pub fn into_arc(self) -> Arc<Principal<C>> {
        self.0
    }

    pub fn try_authenticated(self) -> Result<AuthenticatedPrincipal<C>, NotAuthenticatedError> {
        AuthenticatedPrincipal::try_new(self.0)
    }
}

impl<C: ClaimSet> Clone for CurrentPrincipal<C> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<C: ClaimSet> Deref for CurrentPrincipal<C> {
    type Target = Principal<C>;

    fn deref(&self) -> &Principal<C> {
        &self.0
    }
}

impl<C: ClaimSet> From<Principal<C>> for CurrentPrincipal<C> {
    fn from(principal: Principal<C>) -> Self {
        Self(Arc::new(principal))
    }
}

impl<C: ClaimSet> From<Arc<Principal<C>>> for CurrentPrincipal<C> {
    fn from(principal: Arc<Principal<C>>) -> Self {
        Self(principal)
    }
}
