use std::marker::PhantomData;
use std::sync::Arc;

use crate::binding::Binding;
use crate::current_actor::CurrentActor;
use crate::kind::{Classified, Kind, Unclassified};
use crate::principal_kind::PrincipalKind;
use crate::sealed::Sealed;
use crate::verified_binding::{Method, VerifiedBinding};

pub trait KindState: Sealed {
    type Value;
}

impl<K: Kind> Sealed for K {}

impl<K: Kind> KindState for K {
    type Value = ();
}

impl Sealed for Unclassified {}

impl KindState for Unclassified {
    type Value = ();
}

impl Sealed for Classified {}

impl KindState for Classified {
    type Value = PrincipalKind;
}

pub trait KnownKind: KindState {
    fn resolve(value: &Self::Value) -> &PrincipalKind;
}

impl<K: Kind> KnownKind for K {
    fn resolve(_: &()) -> &PrincipalKind {
        K::KIND
    }
}

impl KnownKind for Classified {
    fn resolve(value: &PrincipalKind) -> &PrincipalKind {
        value
    }
}

pub trait DelegationState: Sealed {
    type Actor;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnyDelegation;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Direct;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delegated;

impl Sealed for AnyDelegation {}
impl Sealed for Direct {}
impl Sealed for Delegated {}

impl DelegationState for AnyDelegation {
    type Actor = Option<Arc<CurrentActor>>;
}

impl DelegationState for Direct {
    type Actor = ();
}

impl DelegationState for Delegated {
    type Actor = Arc<CurrentActor>;
}

pub trait BindingState: Sealed {
    type Proof;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnyBinding;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SenderConstrained<M: Method>(PhantomData<fn() -> M>);

impl Sealed for AnyBinding {}
impl<M: Method> Sealed for SenderConstrained<M> {}

impl BindingState for AnyBinding {
    type Proof = Binding;
}

impl<M: Method> BindingState for SenderConstrained<M> {
    type Proof = BoundProof<M>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundProof<M: Method> {
    verified: VerifiedBinding,
    method: PhantomData<fn() -> M>,
}

impl<M: Method> BoundProof<M> {
    pub(crate) fn new(verified: VerifiedBinding) -> Self {
        Self {
            verified,
            method: PhantomData,
        }
    }

    pub fn thumbprint(&self) -> &str {
        self.verified.thumbprint()
    }

    pub fn verified(&self) -> &VerifiedBinding {
        &self.verified
    }
}

pub trait Refinement: Sealed {
    type Kind: KindState;
    type Delegation: DelegationState;
    type Binding: BindingState;
}

type Markers<K, D, B> = fn() -> (K, D, B);

#[derive(Debug)]
pub struct Refined<K = Unclassified, D = AnyDelegation, B = AnyBinding>(PhantomData<Markers<K, D, B>>);

pub type WithKind<R, K> = Refined<K, <R as Refinement>::Delegation, <R as Refinement>::Binding>;
pub type WithDelegation<R, D> = Refined<<R as Refinement>::Kind, D, <R as Refinement>::Binding>;
pub type WithBinding<R, M> = Refined<<R as Refinement>::Kind, <R as Refinement>::Delegation, SenderConstrained<M>>;

impl<K, D, B> Sealed for Refined<K, D, B> {}

impl<K: KindState, D: DelegationState, B: BindingState> Refinement for Refined<K, D, B> {
    type Kind = K;
    type Delegation = D;
    type Binding = B;
}
