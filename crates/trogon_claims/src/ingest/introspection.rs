use crate::bootstrap_credential::BootstrapCredential;
use crate::claim_type::ClaimType;
use crate::constants::MEMBER_ACTIVE;
use crate::ingest_error::{ClaimValueError, IngestError};
use crate::raw_claims::RawClaims;

fn active_error(kind: ClaimValueError) -> IngestError {
    IngestError::claim(ClaimType::from_static_unchecked(MEMBER_ACTIVE), kind)
}

pub fn introspection(
    body: &[u8],
    presented: Option<&str>,
) -> Result<(RawClaims, Option<BootstrapCredential>), IngestError> {
    let claims = RawClaims::from_slice(body)?;
    let active = claims
        .get_named(MEMBER_ACTIVE)
        .ok_or_else(|| active_error(ClaimValueError::Missing))?
        .as_bool()
        .ok_or_else(|| active_error(ClaimValueError::WrongType { expected: "boolean" }))?;
    if !active {
        return Err(IngestError::Inactive);
    }
    Ok((claims, presented.map(BootstrapCredential::new)))
}
