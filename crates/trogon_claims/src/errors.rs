use std::fmt;

use thiserror::Error;
use trogon_claims_syntax::SyntaxViolation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Audience,
    AuthenticationType,
    ClaimPropertyKey,
    ClaimType,
    ClientId,
    Issuer,
    ObjectId,
    ObjectType,
    ParamName,
    PrincipalKind,
    RelationName,
    RoleName,
    SubjectId,
}

impl ValueKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Audience => "audience",
            Self::AuthenticationType => "authentication_type",
            Self::ClaimPropertyKey => "claim_property_key",
            Self::ClaimType => "claim_type",
            Self::ClientId => "client_id",
            Self::Issuer => "issuer",
            Self::ObjectId => "object_id",
            Self::ObjectType => "object_type",
            Self::ParamName => "param_name",
            Self::PrincipalKind => "principal_kind",
            Self::RelationName => "relation_name",
            Self::RoleName => "role_name",
            Self::SubjectId => "subject_id",
        }
    }
}

impl fmt::Display for ValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("{kind}: {violation}")]
pub struct ValueObjectError {
    kind: ValueKind,
    violation: SyntaxViolation,
}

impl ValueObjectError {
    pub(crate) const fn new(kind: ValueKind, violation: SyntaxViolation) -> Self {
        Self { kind, violation }
    }

    pub const fn kind(&self) -> ValueKind {
        self.kind
    }

    pub const fn violation(&self) -> SyntaxViolation {
        self.violation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("unsigned63: {value} exceeds the maximum of {max}")]
pub struct Unsigned63Error {
    pub(crate) value: u64,
    pub(crate) max: u64,
}

impl Unsigned63Error {
    pub const fn value(&self) -> u64 {
        self.value
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum UtcOffsetError {
    #[error("utc offset of {minutes} minutes is outside -1439 to 1439")]
    OutOfRange { minutes: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TimestampError {
    #[error("nanoseconds {nanos} is not below 1000000000")]
    NanosOutOfRange { nanos: u32 },
    #[error("seconds {secs} is outside year 0000 through 9999")]
    SecondsOutOfRange { secs: i64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum NumericDateError {
    #[error("numeric date is empty")]
    Empty,
    #[error("numeric date is not a JSON number")]
    NotANumber,
    #[error("numeric date is outside year 0000 through 9999")]
    OutOfRange,
}
