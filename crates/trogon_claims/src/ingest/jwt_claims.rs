use fluent_uri::Uri;

use crate::actor_error::ActorError;
use crate::constants::MAX_ACTOR_DEPTH;
use crate::ingest_error::{ClaimValueError, IngestError};
use crate::raw_claims::RawClaims;
use crate::raw_json::{RawJson, RawKind};
use crate::well_known_claim::WellKnownClaim;

fn string_or_uri(raw: &RawJson) -> Result<(), ClaimValueError> {
    let text = raw
        .as_string()
        .ok_or(ClaimValueError::WrongType { expected: "string" })?;
    if text.contains(':') {
        Uri::parse(text.as_str()).map_err(|_| ClaimValueError::InvalidStringOrUri)?;
    }
    Ok(())
}

fn identity_members(claims: &RawClaims) -> Result<(), (WellKnownClaim, ClaimValueError)> {
    for name in [WellKnownClaim::Iss, WellKnownClaim::Sub] {
        if let Some(raw) = claims.get_named(name.as_str()) {
            string_or_uri(raw).map_err(|error| (name, error))?;
        }
    }
    Ok(())
}

fn actor(raw: &RawJson, depth: usize) -> Result<(), ClaimValueError> {
    if depth > MAX_ACTOR_DEPTH {
        return Err(ActorError::TooDeep {
            depth,
            max: MAX_ACTOR_DEPTH,
        }
        .into());
    }
    if raw.kind() != RawKind::Object {
        return Err(ClaimValueError::WrongType { expected: "object" });
    }
    let claims = RawClaims::from_object(raw).map_err(ClaimValueError::Nested)?;
    identity_members(&claims).map_err(|(_, error)| error)?;
    for name in [WellKnownClaim::Act, WellKnownClaim::MayAct] {
        if let Some(nested) = claims.get_named(name.as_str()) {
            actor(nested, depth + 1)?;
        }
    }
    Ok(())
}

pub(super) fn parse_registered_claims(claims: &RawClaims) -> Result<(), IngestError> {
    identity_members(claims).map_err(|(name, error)| IngestError::claim(name, error))?;
    if let Some(raw) = claims.get_named(WellKnownClaim::Aud.as_str()) {
        let audience = if raw.kind() == RawKind::Array {
            raw.as_array()
                .ok_or(ClaimValueError::WrongType {
                    expected: "array of strings",
                })
                .and_then(|entries| entries.iter().try_for_each(string_or_uri))
        } else {
            string_or_uri(raw)
        };
        audience.map_err(|error| IngestError::claim(WellKnownClaim::Aud, error))?;
    }
    for name in [WellKnownClaim::Act, WellKnownClaim::MayAct] {
        if let Some(raw) = claims.get_named(name.as_str()) {
            actor(raw, 1).map_err(|error| IngestError::claim(name, error))?;
        }
    }
    Ok(())
}
