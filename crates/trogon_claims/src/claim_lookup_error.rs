use thiserror::Error;

use crate::well_known_claim::WellKnownClaim;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ClaimLookupError {
    #[error("{claim:?} has an unsupported value shape")]
    WrongType { claim: WellKnownClaim },
    #[error("{claim:?} has multiple values where one is required")]
    MultipleValues { claim: WellKnownClaim },
    #[error("{claim:?} has an invalid value")]
    InvalidValue { claim: WellKnownClaim },
}
