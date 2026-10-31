use std::fmt;

use axum_core::response::{IntoResponse, Response};
use http::header::WWW_AUTHENTICATE;
use http::{HeaderValue, StatusCode};

use crate::authenticated_principal::RefinementError;
use crate::constants::{
    CHALLENGE_BEARER, CHALLENGE_BEARER_INVALID_TOKEN, CHALLENGE_DPOP, CHALLENGE_DPOP_INVALID_TOKEN,
};
use crate::credential_outcome::CredentialOutcome;
use crate::verified_binding::{BindingError, BindingMethod};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Unauthenticated {
    LayerMissing,
    Anonymous(CredentialOutcome),
    BindingRequired(BindingMethod),
    BindingRejected { method: BindingMethod, error: BindingError },
    Forbidden(RefinementError),
}

impl Unauthenticated {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::LayerMissing => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Anonymous(_) | Self::BindingRequired(_) | Self::BindingRejected { .. } => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
        }
    }

    fn challenge(&self) -> Option<&'static str> {
        match self {
            Self::LayerMissing | Self::Forbidden(_) => None,
            Self::Anonymous(CredentialOutcome::Rejected(_)) => Some(CHALLENGE_BEARER_INVALID_TOKEN),
            Self::Anonymous(_) => Some(CHALLENGE_BEARER),
            Self::BindingRequired(BindingMethod::Dpop) => Some(CHALLENGE_DPOP),
            Self::BindingRequired(_) => Some(CHALLENGE_BEARER),
            Self::BindingRejected {
                method: BindingMethod::Dpop,
                ..
            } => Some(CHALLENGE_DPOP_INVALID_TOKEN),
            Self::BindingRejected { .. } => Some(CHALLENGE_BEARER_INVALID_TOKEN),
        }
    }
}

impl fmt::Display for Unauthenticated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LayerMissing => f.write_str("no principal was attached to the request"),
            Self::Anonymous(_) => f.write_str("the request is not authenticated"),
            Self::BindingRequired(_) => f.write_str("the request presented no proof of possession"),
            Self::BindingRejected { .. } => f.write_str("the proof of possession does not match the token"),
            Self::Forbidden(_) => f.write_str("the principal does not satisfy the required refinement"),
        }
    }
}

impl IntoResponse for Unauthenticated {
    fn into_response(self) -> Response {
        let status = self.status();
        match self.challenge() {
            Some(challenge) => (status, [(WWW_AUTHENTICATE, HeaderValue::from_static(challenge))], ()).into_response(),
            None => status.into_response(),
        }
    }
}
