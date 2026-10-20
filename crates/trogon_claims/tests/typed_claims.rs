#![cfg(feature = "json")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use trogon_claims::ingest::IngestClaims;
use trogon_claims::{
    object_type, AuthenticationType, ClaimSet, HasSubject, Identity, IngestError, IngestProfile, ObjectId, ObjectType,
    PrincipalKey, RawClaims, SubjectMapping, SubjectRef, Verbatim, WellKnownClaim,
};

struct UserClaims {
    subject: ObjectId,
}

impl ClaimSet for UserClaims {}

impl HasSubject for UserClaims {
    const SUBJECT_TYPE: ObjectType = object_type!("user");
    type Mapping = Verbatim;

    fn subject(&self) -> SubjectRef<'_> {
        SubjectRef::new(Self::SUBJECT_TYPE, &self.subject)
    }
}

impl IngestClaims for UserClaims {
    fn try_from_raw(_raw: &RawClaims, key: &PrincipalKey) -> Result<Self, IngestError> {
        let subject = <<Self as HasSubject>::Mapping as SubjectMapping>::map(key.sub())
            .map_err(|error| IngestError::claim_for(WellKnownClaim::Sub, error))?;
        Ok(Self { subject })
    }
}

fn profile() -> IngestProfile {
    IngestProfile::new(AuthenticationType::try_new("jwt").unwrap())
}

#[test]
fn a_typed_claim_set_exposes_its_mapped_subject() {
    let raw = RawClaims::from_slice(br#"{"iss":"https://issuer.example.com","sub":"user-42"}"#).unwrap();
    let ingested = Identity::<UserClaims>::ingest(&raw, &profile()).unwrap();
    let subject = ingested.identity().subject();
    assert_eq!(subject.object_type().as_str(), "user");
    assert_eq!(subject.id().as_str(), "user-42");
}

#[test]
fn a_subject_the_mapping_refuses_fails_ingest() {
    let raw = RawClaims::from_slice(br#"{"iss":"https://issuer.example.com","sub":"*"}"#).unwrap();
    let error = Identity::<UserClaims>::ingest(&raw, &profile()).err().unwrap();
    assert!(matches!(error, IngestError::Claim { .. }));
}
