use thiserror::Error;

use crate::constants::{MAX_BASE64URL_SOURCE_BYTES, WILDCARD_SUBJECT};
use crate::errors::ValueObjectError;
use crate::object_id::ObjectId;
use crate::subject_id::SubjectId;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SubjectMappingError {
    #[error("subject is the SpiceDB wildcard")]
    Wildcard,
    #[error("subject of {bytes} bytes exceeds the {max} byte limit of the base64url mapping")]
    TooLong { bytes: usize, max: usize },
    #[error("{0}")]
    Invalid(#[from] ValueObjectError),
}

pub trait SubjectMapping {
    fn map(subject: &SubjectId) -> Result<ObjectId, SubjectMappingError>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Verbatim;

impl SubjectMapping for Verbatim {
    fn map(subject: &SubjectId) -> Result<ObjectId, SubjectMappingError> {
        if subject.as_str() == WILDCARD_SUBJECT {
            return Err(SubjectMappingError::Wildcard);
        }
        Ok(ObjectId::try_new(subject.as_str().to_owned())?)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Base64UrlUnpadded;

impl SubjectMapping for Base64UrlUnpadded {
    fn map(subject: &SubjectId) -> Result<ObjectId, SubjectMappingError> {
        let bytes = subject.as_str().len();
        if bytes > MAX_BASE64URL_SOURCE_BYTES {
            return Err(SubjectMappingError::TooLong {
                bytes,
                max: MAX_BASE64URL_SOURCE_BYTES,
            });
        }
        Ok(ObjectId::from_bytes_base64url_unpadded(subject.as_str().as_bytes())?)
    }
}
