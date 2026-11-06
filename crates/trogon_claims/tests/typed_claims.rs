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

#[cfg(feature = "spicedb")]
mod spicedb {
    use super::*;
    use trogon_claims::{HasSubject, ObjectId, ObjectType, SubjectMapping, SubjectRef, Verbatim};
    struct UserClaims {
        subject: ObjectId,
    }

    impl ClaimSet for UserClaims {}

    impl HasSubject for UserClaims {
        const SUBJECT_TYPE: ObjectType = ObjectType::from_static_unchecked("user");
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
        IngestProfile::new(
            AuthenticationType::try_new("jwt").unwrap(),
            NameClaimType::new(WellKnownClaim::WsName.into()),
            RoleClaimType::new(WellKnownClaim::WsRole.into()),
        )
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
}
