use http::request::Parts;

use crate::authenticated_principal::{AuthenticatedPrincipal, RefinementError};
use crate::claim_set::ClaimSet;
use crate::kind::{Classified, Kind, Unclassified};
use crate::presented_proof::PresentedProof;
use crate::refinement::{
    AnyBinding, AnyDelegation, BindingState, Delegated, DelegationState, Direct, KindState, Refined, Refinement,
    SenderConstrained,
};
use crate::unauthenticated::Unauthenticated;
use crate::verified_binding::Method;

type Start<C> = AuthenticatedPrincipal<C, Refined>;
type AfterKind<C, K> = AuthenticatedPrincipal<C, Refined<K, AnyDelegation, AnyBinding>>;
type AfterDelegation<C, K, D> = AuthenticatedPrincipal<C, Refined<K, D, AnyBinding>>;
type Complete<C, K, D, B> = AuthenticatedPrincipal<C, Refined<K, D, B>>;

pub trait KindStep: KindState + Sized {
    fn apply<C: ClaimSet>(principal: Start<C>) -> Result<AfterKind<C, Self>, RefinementError>;
}

impl<K: Kind> KindStep for K {
    fn apply<C: ClaimSet>(principal: Start<C>) -> Result<AfterKind<C, Self>, RefinementError> {
        principal.try_kind::<K>()
    }
}

impl KindStep for Unclassified {
    fn apply<C: ClaimSet>(principal: Start<C>) -> Result<AfterKind<C, Self>, RefinementError> {
        Ok(principal)
    }
}

impl KindStep for Classified {
    fn apply<C: ClaimSet>(principal: Start<C>) -> Result<AfterKind<C, Self>, RefinementError> {
        principal.try_classified()
    }
}

pub trait DelegationStep<K: KindStep>: DelegationState + Sized {
    fn apply<C: ClaimSet>(principal: AfterKind<C, K>) -> Result<AfterDelegation<C, K, Self>, RefinementError>;
}

impl<K: KindStep> DelegationStep<K> for AnyDelegation {
    fn apply<C: ClaimSet>(principal: AfterKind<C, K>) -> Result<AfterDelegation<C, K, Self>, RefinementError> {
        Ok(principal)
    }
}

impl<K: KindStep> DelegationStep<K> for Delegated {
    fn apply<C: ClaimSet>(principal: AfterKind<C, K>) -> Result<AfterDelegation<C, K, Self>, RefinementError> {
        principal.try_delegated()
    }
}

impl<K: KindStep> DelegationStep<K> for Direct {
    fn apply<C: ClaimSet>(principal: AfterKind<C, K>) -> Result<AfterDelegation<C, K, Self>, RefinementError> {
        principal.try_direct()
    }
}

pub trait BindingStep<K: KindStep, D: DelegationStep<K>>: BindingState + Sized {
    fn apply<C: ClaimSet>(
        principal: AfterDelegation<C, K, D>,
        parts: &Parts,
    ) -> Result<Complete<C, K, D, Self>, Unauthenticated>;
}

impl<K: KindStep, D: DelegationStep<K>> BindingStep<K, D> for AnyBinding {
    fn apply<C: ClaimSet>(
        principal: AfterDelegation<C, K, D>,
        _: &Parts,
    ) -> Result<Complete<C, K, D, Self>, Unauthenticated> {
        Ok(principal)
    }
}

fn bind<C, K, D, M>(
    principal: AfterDelegation<C, K, D>,
    parts: &Parts,
) -> Result<Complete<C, K, D, SenderConstrained<M>>, Unauthenticated>
where
    C: ClaimSet,
    K: KindStep,
    D: DelegationStep<K>,
    M: Method + Send + Sync + 'static,
{
    let proof = parts
        .extensions
        .get::<PresentedProof<M>>()
        .ok_or(Unauthenticated::BindingRequired(M::METHOD))?;
    principal
        .try_bind::<M>(proof.thumbprint())
        .map_err(|error| Unauthenticated::BindingRejected {
            method: M::METHOD,
            error,
        })
}

impl<K, D, M> BindingStep<K, D> for SenderConstrained<M>
where
    K: KindStep,
    D: DelegationStep<K>,
    M: Method + Send + Sync + 'static,
{
    fn apply<C: ClaimSet>(
        principal: AfterDelegation<C, K, D>,
        parts: &Parts,
    ) -> Result<Complete<C, K, D, Self>, Unauthenticated> {
        bind::<C, K, D, M>(principal, parts)
    }
}

pub trait ExtractRefinement: Refinement + Sized {
    fn refine<C: ClaimSet>(
        principal: Start<C>,
        parts: &Parts,
    ) -> Result<AuthenticatedPrincipal<C, Self>, Unauthenticated>;
}

impl<K, D, B> ExtractRefinement for Refined<K, D, B>
where
    K: KindStep,
    D: DelegationStep<K>,
    B: BindingStep<K, D>,
{
    fn refine<C: ClaimSet>(
        principal: Start<C>,
        parts: &Parts,
    ) -> Result<AuthenticatedPrincipal<C, Self>, Unauthenticated> {
        let principal = K::apply(principal).map_err(Unauthenticated::Forbidden)?;
        let principal = D::apply(principal).map_err(Unauthenticated::Forbidden)?;
        B::apply(principal, parts)
    }
}
