#![allow(clippy::unwrap_used, clippy::expect_used)]

use trogon_claims::{
    AuthenticationType, Claim, ClaimText, ClaimValue, Identity, IdentityBuildError, IdentityBuilder, Issuer,
    NameClaimType, PrimaryIdentityIndex, Principal, PrincipalBuildError, PrincipalBuilder, RoleClaimType,
    WellKnownClaim,
};

fn builder() -> IdentityBuilder {
    IdentityBuilder::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::Sub.into()),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
    )
}

fn text_claim(name: WellKnownClaim, value: &'static str) -> Claim {
    Claim::builder(name.into(), ClaimValue::Text(ClaimText::from(value)))
        .issuer(Issuer::try_new("https://claim-issuer.example.com").unwrap())
        .build()
}

fn identity(subject: &'static str) -> Identity {
    builder()
        .push(text_claim(WellKnownClaim::Iss, "https://token-issuer.example.com"))
        .push(text_claim(WellKnownClaim::Sub, subject))
        .try_build()
        .unwrap()
}

#[test]
fn missing_issuer_is_not_inferred_from_the_subject_claim() {
    let result = builder().push(text_claim(WellKnownClaim::Sub, "user-1")).try_build();
    assert_eq!(result.unwrap_err(), IdentityBuildError::MissingIssuer);
}

#[test]
fn malformed_issuer_is_not_replaced_with_the_subject_claim_issuer() {
    let result = builder()
        .push(Claim::builder(WellKnownClaim::Iss.into(), ClaimValue::Integer(7)).build())
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .try_build();
    assert_eq!(result.unwrap_err(), IdentityBuildError::InvalidIssuerType);
}

#[test]
fn duplicate_subject_claims_are_ambiguous_even_when_equal() {
    let result = builder()
        .push(text_claim(WellKnownClaim::Iss, "https://token-issuer.example.com"))
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .try_build();
    assert_eq!(result.unwrap_err(), IdentityBuildError::DuplicateSubject);
}

#[test]
fn duplicate_issuer_claims_are_ambiguous_even_when_equal() {
    let result = builder()
        .push(text_claim(WellKnownClaim::Iss, "https://token-issuer.example.com"))
        .push(text_claim(WellKnownClaim::Iss, "https://token-issuer.example.com"))
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .try_build();
    assert_eq!(result.unwrap_err(), IdentityBuildError::DuplicateIssuer);
}

#[test]
fn multiple_identities_require_an_explicit_primary_choice() {
    let result = PrincipalBuilder::new(identity("user-1"))
        .push(identity("user-2"))
        .try_build();
    assert_eq!(result.unwrap_err(), PrincipalBuildError::MissingPrimary);
}

#[test]
fn appending_an_identity_preserves_the_selected_anonymous_primary() {
    let principal = Principal::anonymous().with_identity(identity("user-1"));
    assert!(!principal.is_authenticated());
    assert!(matches!(principal.primary(), Identity::Anonymous));
    assert_eq!(principal.authenticated_identities().count(), 1);
}

#[test]
fn an_explicit_missing_issuer_fallback_is_used_only_when_absent() {
    let identity = builder()
        .with_default_issuer(Issuer::try_new("https://configured.example.com").unwrap())
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .try_build()
        .unwrap();
    assert_eq!(
        identity.as_authenticated().unwrap().key().issuer().as_str(),
        "https://configured.example.com"
    );
    assert_eq!(
        identity.as_authenticated().unwrap().claims().standard().issuer(),
        Ok(None)
    );
    assert_eq!(
        identity
            .as_authenticated()
            .unwrap()
            .find_first(&WellKnownClaim::Sub.into())
            .unwrap()
            .issuer()
            .as_str(),
        "https://claim-issuer.example.com"
    );

    let error = builder()
        .with_default_issuer(Issuer::try_new("https://configured.example.com").unwrap())
        .push(Claim::builder(WellKnownClaim::Iss.into(), ClaimValue::Boolean(false)).build())
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .try_build()
        .unwrap_err();
    assert_eq!(error, IdentityBuildError::InvalidIssuerType);
}

#[test]
fn a_configured_fallback_does_not_hide_duplicate_issuers() {
    let error = builder()
        .with_default_issuer(Issuer::try_new("https://configured.example.com").unwrap())
        .push(text_claim(WellKnownClaim::Iss, "https://token-issuer.example.com"))
        .push(text_claim(WellKnownClaim::Iss, "https://other.example.com"))
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .try_build()
        .unwrap_err();
    assert_eq!(error, IdentityBuildError::DuplicateIssuer);
    let rendered = format!("{error} {error:?}");
    assert!(!rendered.contains("token-issuer.example.com"));
    assert!(!rendered.contains("other.example.com"));
    assert!(!rendered.contains("user-1"));
}

#[test]
fn explicit_primary_reselection_checks_range_and_authentication() {
    let principal = Principal::anonymous().with_identity(identity("user-1"));
    assert_eq!(
        principal
            .clone()
            .try_with_primary(PrimaryIdentityIndex::new(0))
            .unwrap_err(),
        PrincipalBuildError::PrimaryAnonymous
    );
    assert_eq!(
        principal
            .clone()
            .try_with_primary(PrimaryIdentityIndex::new(2))
            .unwrap_err(),
        PrincipalBuildError::PrimaryOutOfRange { index: 2, len: 2 }
    );
    let principal = principal.try_with_primary(PrimaryIdentityIndex::new(1)).unwrap();
    assert!(principal.is_authenticated());
    assert_eq!(principal.name(), Some("user-1"));
}

#[test]
fn cross_identity_queries_keep_identity_and_claim_issuer_provenance() {
    let principal = PrincipalBuilder::new(identity("user-1"))
        .push(identity("user-2"))
        .primary(PrimaryIdentityIndex::new(0))
        .try_build()
        .unwrap();
    let sub = WellKnownClaim::Sub.into();
    let first = principal.find_first(&sub).unwrap();
    assert_eq!(first.identity().key().sub().as_str(), "user-1");
    assert_eq!(first.claim().issuer().as_str(), "https://claim-issuer.example.com");
    let subjects: Vec<_> = principal
        .find_all(&sub)
        .map(|source| source.identity().key().sub().as_str())
        .collect();
    assert_eq!(subjects, ["user-1", "user-2"]);
}
