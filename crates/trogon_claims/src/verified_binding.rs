use thiserror::Error;

use crate::claim_text::ClaimText;
use crate::sealed::Sealed;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BindingMethod {
    Dpop,
    MutualTls,
}

pub trait Method: Sealed {
    const METHOD: BindingMethod;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Dpop;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MutualTls;

impl Sealed for Dpop {}
impl Sealed for MutualTls {}

impl Method for Dpop {
    const METHOD: BindingMethod = BindingMethod::Dpop;
}

impl Method for MutualTls {
    const METHOD: BindingMethod = BindingMethod::MutualTls;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedBinding {
    method: BindingMethod,
    thumbprint: ClaimText,
}

impl VerifiedBinding {
    pub(crate) fn new(method: BindingMethod, thumbprint: ClaimText) -> Self {
        Self { method, thumbprint }
    }

    pub fn method(&self) -> BindingMethod {
        self.method
    }

    pub fn thumbprint(&self) -> &str {
        self.thumbprint.as_str()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum BindingError {
    #[error("the token asserts no sender constraint")]
    NotAsserted,
    #[error("the token's confirmation carries no member for {0:?}")]
    MemberMissing(BindingMethod),
    #[error("the presented thumbprint does not match the token's {0:?} confirmation")]
    Mismatch(BindingMethod),
}
