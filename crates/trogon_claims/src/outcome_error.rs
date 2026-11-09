use thiserror::Error;

use crate::errors::ValueObjectError;

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum OutcomeError {
    #[error("the permissionship is UNSPECIFIED, which is a protocol error and not a denial")]
    Unspecified,
    #[error("the permissionship value {value} is not one this crate knows")]
    Unknown { value: i32 },
    #[error("a missing context parameter name is invalid: {0}")]
    MissingParameter(#[from] ValueObjectError),
}
