use thiserror::Error;

use crate::subject_mapping::SubjectMappingError;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ActorSubjectError {
    #[error("the current actor is a client and has no subject")]
    NoSubject,
    #[error("{0}")]
    Mapping(#[from] SubjectMappingError),
}
