use std::fmt;

use axum_core::response::{IntoResponse, Response};
use http::StatusCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrincipalUnavailable;

impl fmt::Display for PrincipalUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no principal was attached to the request")
    }
}

impl IntoResponse for PrincipalUnavailable {
    fn into_response(self) -> Response {
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    }
}
