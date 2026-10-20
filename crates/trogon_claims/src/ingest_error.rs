use thiserror::Error;

use crate::actor_error::ActorError;
use crate::claim_type::ClaimType;
use crate::errors::{NumericDateError, ValueObjectError};
use crate::raw_claims_error::RawClaimsError;
use crate::subject_mapping::SubjectMappingError;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ConfirmationError {
    #[error("confirmation has no members")]
    Empty,
    #[error("confirmation names more than one of jwk, jwe and jku")]
    MultipleKeyMembers,
    #[error("confirmation carries a symmetric key")]
    SymmetricKey,
    #[error("confirmation carries private key material")]
    PrivateKeyMaterial,
    #[error("confirmation jwk has no kty")]
    MissingKeyType,
    #[error("confirmation member {member} has the wrong JSON type")]
    WrongType { member: &'static str },
    #[error("{0}")]
    Malformed(#[from] RawClaimsError),
    #[error("{0}")]
    Invalid(#[from] ValueObjectError),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ClaimValueError {
    #[error("expected a JSON {expected}")]
    WrongType { expected: &'static str },
    #[error("required member is missing")]
    Missing,
    #[error("numeric date is out of range")]
    OutOfRange(#[from] NumericDateError),
    #[error("invalid actor: {0}")]
    InvalidActor(#[from] ActorError),
    #[error("token has no sub")]
    MissingSubject,
    #[error("token has no iss and the profile names no default issuer")]
    MissingIssuer,
    #[error("invalid confirmation: {0}")]
    Confirmation(#[from] ConfirmationError),
    #[error("{0}")]
    Invalid(#[from] ValueObjectError),
    #[error("subject mapping failed: {0}")]
    SubjectMapping(#[from] SubjectMappingError),
    #[error("nested object is malformed: {0}")]
    Nested(#[from] RawClaimsError),
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum IngestError {
    #[error("malformed claims: {0}")]
    Malformed(#[from] RawClaimsError),
    #[error("claim {name}: {kind}")]
    Claim { name: ClaimType, kind: ClaimValueError },
    #[error("introspection response reports an inactive token")]
    Inactive,
}

impl IngestError {
    pub fn claim_for(name: impl Into<ClaimType>, kind: impl Into<ClaimValueError>) -> Self {
        Self::Claim {
            name: name.into(),
            kind: kind.into(),
        }
    }

    pub(crate) fn claim(name: impl Into<ClaimType>, kind: impl Into<ClaimValueError>) -> Self {
        Self::Claim {
            name: name.into(),
            kind: kind.into(),
        }
    }
}
