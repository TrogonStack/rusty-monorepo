use std::sync::Arc;

use axum_core::extract::FromRequestParts;
use http::request::Parts;

use crate::authenticated_principal::AuthenticatedPrincipal;
use crate::claim_set::ClaimSet;
use crate::credential_outcome::CredentialOutcome;
use crate::current_principal::CurrentPrincipal;
use crate::principal::Principal;
use crate::principal_unavailable::PrincipalUnavailable;
use crate::refinement_extract::ExtractRefinement;
use crate::unauthenticated::Unauthenticated;

impl<S, C> FromRequestParts<S> for CurrentPrincipal<C>
where
    S: Send + Sync,
    C: ClaimSet + Send + Sync + 'static,
{
    type Rejection = PrincipalUnavailable;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Arc<Principal<C>>>()
            .map(|principal| Self::new(Arc::clone(principal)))
            .ok_or(PrincipalUnavailable)
    }
}

impl<S, C, R> FromRequestParts<S> for AuthenticatedPrincipal<C, R>
where
    S: Send + Sync,
    C: ClaimSet + Send + Sync + 'static,
    R: ExtractRefinement,
{
    type Rejection = Unauthenticated;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let principal = parts
            .extensions
            .get::<Arc<Principal<C>>>()
            .map(Arc::clone)
            .ok_or(Unauthenticated::LayerMissing)?;
        let authenticated = AuthenticatedPrincipal::try_new(principal).map_err(|_| {
            let outcome = parts
                .extensions
                .get::<CredentialOutcome>()
                .copied()
                .unwrap_or(CredentialOutcome::Absent);
            Unauthenticated::Anonymous(outcome)
        })?;
        R::refine(authenticated, parts)
    }
}
