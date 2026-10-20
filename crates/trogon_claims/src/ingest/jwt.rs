use crate::base64url::decode_unpadded;
use crate::bootstrap_credential::BootstrapCredential;
use crate::constants::COMPACT_JWT_PARTS;
use crate::ingest_error::IngestError;
use crate::raw_claims::RawClaims;
use crate::raw_claims_error::RawClaimsError;

pub fn jwt_payload(compact: &str) -> Result<(RawClaims, BootstrapCredential), IngestError> {
    let mut parts = compact.split('.');
    let (Some(_header), Some(payload), Some(_signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(RawClaimsError::NotCompactJwt.into());
    };
    debug_assert_eq!(COMPACT_JWT_PARTS, 3);
    let bytes = decode_unpadded(payload).ok_or(RawClaimsError::InvalidBase64)?;
    let claims = RawClaims::from_slice(&bytes)?;
    Ok((claims, BootstrapCredential::new(compact)))
}
