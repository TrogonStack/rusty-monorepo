use std::ops::Deref;

use axum_core::extract::FromRequestParts;
use axum_core::response::{IntoResponse, Response};
use http::request::Parts;
use http::StatusCode;

use crate::authenticated_principal::AuthenticatedPrincipal;
use crate::claim_set::ClaimSet;
use crate::refinement::{Refined, Refinement};
use crate::refinement_extract::ExtractRefinement;
use crate::unauthenticated::Unauthenticated;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Masked<T>(pub T);

impl<S, T> FromRequestParts<S> for Masked<T>
where
    S: Send + Sync,
    T: FromRequestParts<S>,
{
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        T::from_request_parts(parts, state)
            .await
            .map(Self)
            .map_err(|_| StatusCode::NOT_FOUND)
    }
}

pub struct PrincipalOrNotFound<C: ClaimSet, R: Refinement = Refined>(AuthenticatedPrincipal<C, R>);

impl<C: ClaimSet, R: Refinement> PrincipalOrNotFound<C, R> {
    pub fn into_inner(self) -> AuthenticatedPrincipal<C, R> {
        self.0
    }
}

impl<C: ClaimSet, R: Refinement> Deref for PrincipalOrNotFound<C, R> {
    type Target = AuthenticatedPrincipal<C, R>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HiddenResource(Unauthenticated);

impl IntoResponse for HiddenResource {
    fn into_response(self) -> Response {
        match self.0 {
            Unauthenticated::LayerMissing => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

impl<S, C, R> FromRequestParts<S> for PrincipalOrNotFound<C, R>
where
    S: Send + Sync,
    C: ClaimSet + Send + Sync + 'static,
    R: ExtractRefinement,
{
    type Rejection = HiddenResource;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        AuthenticatedPrincipal::<C, R>::from_request_parts(parts, state)
            .await
            .map(Self)
            .map_err(HiddenResource)
    }
}
