use crate::principal_kind::PrincipalKind;

pub trait Kind {
    const KIND: &'static PrincipalKind;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unclassified;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Classified;
