#![cfg(feature = "json")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use trogon_claims::ingest::IngestClaims;
use trogon_claims::{
    AuthenticationType, ClaimSet, Identity, IngestError, IngestProfile, NameClaimType, PrincipalKey, RawClaims,
    RoleClaimType, SubjectId, WellKnownClaim,
};

struct IdentityClaims {
    subject: SubjectId,
}

impl ClaimSet for IdentityClaims {}

impl IngestClaims for IdentityClaims {
    fn try_from_raw(_raw: &RawClaims, key: &PrincipalKey) -> Result<Self, IngestError> {
        Ok(Self {
            subject: key.sub().clone(),
        })
    }
}

#[test]
fn typed_claims_keep_the_identity_subject_without_backend_mapping() {
    let raw =
        RawClaims::from_slice(br#"{"iss":"https://issuer.example.com","sub":"user@example.com#fragment"}"#).unwrap();
    let profile = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::Sub.into()),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
    );
    let ingested = Identity::<IdentityClaims>::ingest(&raw, &profile).unwrap();
    assert_eq!(
        ingested.identity().claims().subject.as_str(),
        "user@example.com#fragment"
    );
    assert_eq!(ingested.identity().key().sub().as_str(), "user@example.com#fragment");
}
