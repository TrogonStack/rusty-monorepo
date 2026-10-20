use thiserror::Error;

use crate::errors::ValueObjectError;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ReferenceParseError {
    #[error("reference has no ':' between the object type and the object id")]
    MissingSeparator,
    #[error("a subject reference carries no relation, a subject set is the 'type:id#relation' form")]
    UnexpectedRelation,
    #[error("a subject set needs '#relation' after 'type:id'")]
    MissingRelation,
    #[error("{0}")]
    Invalid(#[from] ValueObjectError),
}
