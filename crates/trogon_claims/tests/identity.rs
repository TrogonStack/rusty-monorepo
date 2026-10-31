#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use trogon_claims::{
    AuthenticationType, Binding, Claim, ClaimText, ClaimType, ClaimValue, Delegation, Identity, IdentityBuildError,
    IdentityBuilder, Issuer, PrincipalKind, RoleName, WellKnownClaim,
};

fn text_claim(name: impl Into<ClaimType>, value: &'static str) -> Claim {
    Claim::builder(name.into(), ClaimValue::Text(ClaimText::from(value)))
        .issuer(Issuer::try_new("https://issuer.example.com").unwrap())
        .build()
}

fn builder() -> IdentityBuilder {
    IdentityBuilder::new(AuthenticationType::try_new("test").unwrap())
        .push(text_claim(WellKnownClaim::Sub, "user-1"))
        .push(text_claim(WellKnownClaim::WsName, "Ada"))
        .push(text_claim(WellKnownClaim::WsRole, "admin"))
}

#[test]
fn the_builder_derives_the_key_from_the_claims() {
    let identity = builder().try_build().unwrap();
    let authenticated = identity.as_authenticated().unwrap();
    assert_eq!(authenticated.key().sub().as_str(), "user-1");
    assert_eq!(authenticated.key().issuer().as_str(), "https://issuer.example.com");
    assert_eq!(authenticated.name(), Some("Ada"));
    assert!(authenticated.is_in_role(&RoleName::try_new("admin").unwrap()));
    assert!(!authenticated.is_in_role(&RoleName::try_new("root").unwrap()));
    assert_eq!(authenticated.binding(), &Binding::Bearer);
    assert!(matches!(authenticated.delegation(), Delegation::Direct));
    assert_eq!(authenticated.authentication_type().as_str(), "test");
}

#[test]
fn an_explicit_iss_claim_wins_over_the_claim_issuer() {
    let identity = builder()
        .push(text_claim(WellKnownClaim::Iss, "https://other.example.com"))
        .try_build()
        .unwrap();
    assert_eq!(
        identity.as_authenticated().unwrap().key().issuer().as_str(),
        "https://other.example.com"
    );
}

#[test]
fn the_builder_requires_a_textual_subject() {
    let missing = IdentityBuilder::new(AuthenticationType::try_new("test").unwrap()).try_build();
    assert_eq!(missing.unwrap_err(), IdentityBuildError::MissingSubject);
    let numeric = IdentityBuilder::new(AuthenticationType::try_new("test").unwrap())
        .push(Claim::builder(WellKnownClaim::Sub.into(), ClaimValue::Integer(1)).build())
        .try_build();
    assert_eq!(numeric.unwrap_err(), IdentityBuildError::MissingSubject);
}

#[test]
fn an_anonymous_identity_carries_nothing() {
    let identity: Identity = Identity::anonymous();
    assert!(!identity.is_authenticated());
    assert!(identity.as_authenticated().is_none());
}

#[test]
fn kind_is_unset_until_classified_and_does_not_alter_the_source() {
    let original = builder().try_build().unwrap();
    assert!(original.as_authenticated().unwrap().kind().is_none());
    let classified = original.clone().with_kind(PrincipalKind::try_new("service").unwrap());
    assert_eq!(
        classified.as_authenticated().unwrap().kind().unwrap().as_str(),
        "service"
    );
    assert!(original.as_authenticated().unwrap().kind().is_none());
}

#[test]
fn a_custom_name_claim_type_is_honoured_by_the_builder() {
    let identity = builder()
        .name_claim_type(WellKnownClaim::Name.into())
        .push(text_claim(WellKnownClaim::Name, "Grace"))
        .try_build()
        .unwrap();
    assert_eq!(identity.as_authenticated().unwrap().name(), Some("Grace"));
}
