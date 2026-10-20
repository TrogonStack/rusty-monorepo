use crate::authentication_context::AuthenticationContext;
use crate::authentication_method::AuthenticationMethod;
use crate::claim_text::ClaimText;
use crate::ingest_error::{ClaimValueError, IngestError};
use crate::numeric_date::NumericDate;
use crate::raw_claims::RawClaims;
use crate::raw_json::RawKind;
use crate::well_known_claim::WellKnownClaim;

pub(crate) fn parse_context(raw: &RawClaims) -> Result<AuthenticationContext, IngestError> {
    let acr = match raw.get_named(WellKnownClaim::Acr.as_str()) {
        Some(value) => Some(ClaimText::from(value.as_string().ok_or_else(|| {
            IngestError::claim(WellKnownClaim::Acr, ClaimValueError::WrongType { expected: "string" })
        })?)),
        None => None,
    };
    let amr = match raw.get_named(WellKnownClaim::Amr.as_str()) {
        Some(value) => {
            let fail = || {
                IngestError::claim(
                    WellKnownClaim::Amr,
                    ClaimValueError::WrongType {
                        expected: "array of strings",
                    },
                )
            };
            value
                .as_array()
                .ok_or_else(fail)?
                .iter()
                .map(|element| {
                    element
                        .as_string()
                        .map(AuthenticationMethod::from_value)
                        .ok_or_else(fail)
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        None => Vec::new(),
    };
    let auth_time = match raw.get_named(WellKnownClaim::AuthTime.as_str()) {
        Some(value) => {
            let text = match value.kind() {
                RawKind::Number => value.number_text(),
                _ => None,
            }
            .ok_or_else(|| {
                IngestError::claim(
                    WellKnownClaim::AuthTime,
                    ClaimValueError::WrongType { expected: "number" },
                )
            })?;
            Some(
                NumericDate::try_from_lexical(text)
                    .map_err(|error| IngestError::claim(WellKnownClaim::AuthTime, error))?,
            )
        }
        None => None,
    };
    Ok(AuthenticationContext::new(acr, amr, auth_time))
}
