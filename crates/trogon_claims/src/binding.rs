use crate::confirmation::Confirmation;

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Binding {
    Bearer,
    Asserted(Box<Confirmation>),
}
