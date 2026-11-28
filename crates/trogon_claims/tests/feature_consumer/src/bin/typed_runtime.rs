use trogon_claims::ingest::IngestClaims;
use trogon_claims::{
    AuthenticationType, ClaimSet, Identity, IngestError, IngestProfile, NameClaimType, PrincipalKey, RawClaims,
    RoleClaimType, SubjectId, WellKnownClaim,
};

struct UserClaims(SubjectId);

impl ClaimSet for UserClaims {}

impl IngestClaims for UserClaims {
    fn try_from_raw(_: &RawClaims, key: &PrincipalKey) -> Result<Self, IngestError> {
        Ok(Self(key.sub().clone()))
    }
}

fn main() {
    let raw =
        RawClaims::from_slice(br#"{"iss":"https://issuer.example.com","sub":"user@example.com#fragment"}"#).unwrap();
    let profile = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::Sub.into()),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
    );
    let ingested = Identity::<UserClaims>::ingest(&raw, &profile).unwrap();
    assert_eq!(ingested.identity().claims().0.as_str(), "user@example.com#fragment");
    assert_eq!(
        ingested.identity().key().issuer().as_str(),
        "https://issuer.example.com"
    );
}
