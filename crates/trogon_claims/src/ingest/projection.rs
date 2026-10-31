use std::sync::Arc;

use crate::claim::Claim;
use crate::claim_text::ClaimText;
use crate::claim_type::ClaimType;
use crate::claim_value::ClaimValue;
use crate::errors::NumericDateError;
use crate::ingest_error::{ClaimValueError, IngestError};
use crate::issuer::Issuer;
use crate::numeric_date::NumericDate;
use crate::raw_json::{RawJson, RawKind};
use crate::well_known_claim::WellKnownClaim;

fn is_time_claim(name: &ClaimType) -> bool {
    matches!(
        name.well_known(),
        Some(WellKnownClaim::Exp | WellKnownClaim::Nbf | WellKnownClaim::Iat | WellKnownClaim::AuthTime)
    )
}

fn number_value(name: &ClaimType, text: &str) -> Result<ClaimValue, ClaimValueError> {
    if is_time_claim(name) {
        let date = NumericDate::try_from_lexical(text).map_err(|error| match error {
            NumericDateError::OutOfRange => ClaimValueError::OutOfRange(error),
            other => ClaimValueError::OutOfRange(other),
        })?;
        return Ok(ClaimValue::Timestamp(date.to_timestamp()));
    }
    if text.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-') {
        if let Ok(integer) = text.parse::<i64>() {
            return Ok(ClaimValue::Integer(integer));
        }
    }
    match text.parse::<f64>() {
        Ok(double) if double.is_finite() => Ok(ClaimValue::Double(double)),
        _ => Err(ClaimValueError::WrongType {
            expected: "finite number",
        }),
    }
}

fn scalar_value(name: &ClaimType, raw: &RawJson) -> Result<Option<ClaimValue>, ClaimValueError> {
    Ok(match raw.kind() {
        RawKind::String => raw.as_string().map(|text| ClaimValue::Text(ClaimText::from(text))),
        RawKind::Bool => raw.as_bool().map(ClaimValue::Boolean),
        RawKind::Number => match raw.number_text() {
            Some(text) => Some(number_value(name, text)?),
            None => None,
        },
        RawKind::Null | RawKind::Array | RawKind::Object => None,
    })
}

pub(crate) fn project_member(
    claims: &mut Vec<Claim>,
    unprojected: &mut Vec<ClaimType>,
    name: &ClaimType,
    raw: &RawJson,
    issuer: &Arc<Issuer>,
) -> Result<(), IngestError> {
    let fail = |kind: ClaimValueError| IngestError::claim(name.clone(), kind);
    let mut push = |value: ClaimValue| {
        claims.push(
            Claim::builder(name.clone(), value)
                .shared_issuer(Arc::clone(issuer))
                .build(),
        );
    };
    if raw.kind() == RawKind::Array {
        let elements = raw
            .as_array()
            .ok_or_else(|| fail(ClaimValueError::WrongType { expected: "array" }))?;
        let mut skipped = false;
        for element in &elements {
            match scalar_value(name, element).map_err(fail)? {
                Some(value) => push(value),
                None => skipped = true,
            }
        }
        if skipped {
            unprojected.push(name.clone());
        }
        return Ok(());
    }
    match scalar_value(name, raw).map_err(fail)? {
        Some(value) => push(value),
        None => unprojected.push(name.clone()),
    }
    Ok(())
}
