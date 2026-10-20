use crate::principal_kind::PrincipalKind;

pub trait Kind {
    const KIND: PrincipalKind;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unclassified;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classified(PrincipalKind);

impl Classified {
    pub fn new(kind: PrincipalKind) -> Self {
        Self(kind)
    }

    pub fn kind(&self) -> &PrincipalKind {
        &self.0
    }
}
