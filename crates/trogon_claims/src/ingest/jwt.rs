use crate::base64url::decode_unpadded;
use crate::bootstrap_credential::BootstrapCredential;
use crate::constants::COMPACT_JWT_PARTS;
use crate::ingest::jwt_claims::parse_registered_claims;
use crate::ingest_error::IngestError;
use crate::raw_claims::RawClaims;
use crate::raw_claims_error::RawClaimsError;

/// Parses the payload and registered identity claim representations without
/// verifying the token or applying token-acceptance policy.
///
/// `iss`, `sub`, and `aud` follow [RFC 7519 §2 and §4.1](https://www.rfc-editor.org/rfc/rfc7519.html#section-2).
/// Actor identity claims follow [RFC 8693 §4](https://www.rfc-editor.org/rfc/rfc8693.html#section-4).
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
    parse_registered_claims(&claims)?;
    Ok((claims, BootstrapCredential::new(compact)))
}
