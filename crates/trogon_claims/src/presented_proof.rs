use std::marker::PhantomData;

use crate::verified_binding::Method;

#[derive(Debug)]
pub struct PresentedProof<M: Method> {
    thumbprint: String,
    method: PhantomData<fn() -> M>,
}

impl<M: Method> PresentedProof<M> {
    pub fn new(thumbprint: impl Into<String>) -> Self {
        Self {
            thumbprint: thumbprint.into(),
            method: PhantomData,
        }
    }

    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }
}

impl<M: Method> Clone for PresentedProof<M> {
    fn clone(&self) -> Self {
        Self::new(self.thumbprint.clone())
    }
}

impl<M: Method> PartialEq for PresentedProof<M> {
    fn eq(&self, other: &Self) -> bool {
        self.thumbprint == other.thumbprint
    }
}

impl<M: Method> Eq for PresentedProof<M> {}
