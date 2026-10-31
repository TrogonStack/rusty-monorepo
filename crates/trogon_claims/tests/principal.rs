#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use trogon_claims::{
    AuthenticationType, Claim, ClaimText, ClaimType, ClaimValue, Identity, IdentityBuilder, Issuer, Principal,
    PrincipalBuildError, PrincipalBuilder, RoleName, WellKnownClaim,
};

fn text_claim(name: impl Into<ClaimType>, value: &'static str) -> Claim {
    Claim::builder(name.into(), ClaimValue::Text(ClaimText::from(value)))
        .issuer(Issuer::try_new("https://issuer.example.com").unwrap())
        .build()
}

fn identity(sub: &'static str, name: &'static str, role: &'static str) -> Identity {
    IdentityBuilder::new(AuthenticationType::try_new("test").unwrap())
        .push(text_claim(WellKnownClaim::Sub, sub))
        .push(text_claim(WellKnownClaim::WsName, name))
        .push(text_claim(WellKnownClaim::WsRole, role))
        .try_build()
        .unwrap()
}

fn role(value: &str) -> RoleName {
    RoleName::try_new(value.to_owned()).unwrap()
}

#[test]
fn the_anonymous_principal_answers_every_query() {
    let principal = Principal::anonymous();
    assert!(!principal.is_authenticated());
    assert!(!principal.primary().is_authenticated());
    assert_eq!(principal.identities().len(), 1);
    assert_eq!(principal.claims().count(), 0);
    assert_eq!(principal.name(), None);
    assert!(!principal.is_in_role(&role("admin")));
    assert!(!principal.has_claim(&WellKnownClaim::Sub.into()));
    assert!(principal.find_first(&WellKnownClaim::Sub.into()).is_none());
    assert_eq!(principal.find_all(&WellKnownClaim::Sub.into()).count(), 0);
    assert_eq!(principal.authenticated_identities().count(), 0);
}

#[test]
fn the_primary_defaults_to_the_first_authenticated_identity() {
    let principal = PrincipalBuilder::new(Identity::Anonymous)
        .push(identity("user-1", "Ada", "admin"))
        .push(identity("user-2", "Grace", "auditor"))
        .try_build()
        .unwrap();
    assert!(principal.is_authenticated());
    assert_eq!(principal.name(), Some("Ada"));
    assert_eq!(principal.identities().len(), 3);
}

#[test]
fn a_designated_primary_is_honoured() {
    let principal = PrincipalBuilder::new(identity("user-1", "Ada", "admin"))
        .push(identity("user-2", "Grace", "auditor"))
        .primary(1)
        .try_build()
        .unwrap();
    assert_eq!(principal.name(), Some("Grace"));
}

#[test]
fn a_bad_primary_is_refused() {
    let out_of_range = PrincipalBuilder::new(identity("user-1", "Ada", "admin"))
        .primary(4)
        .try_build()
        .unwrap_err();
    assert_eq!(
        out_of_range,
        PrincipalBuildError::PrimaryOutOfRange { index: 4, len: 1 }
    );

    let anonymous = PrincipalBuilder::new(Identity::Anonymous)
        .push(identity("user-1", "Ada", "admin"))
        .primary(0)
        .try_build()
        .unwrap_err();
    assert_eq!(anonymous, PrincipalBuildError::PrimaryAnonymous);
}

#[test]
fn role_checks_cross_identities_each_with_its_own_role_claim_type() {
    let custom_role: ClaimType = ClaimType::try_new("groups").unwrap();
    let custom = IdentityBuilder::new(AuthenticationType::try_new("test").unwrap())
        .push(text_claim(WellKnownClaim::Sub, "user-2"))
        .push(text_claim(custom_role.clone(), "ops"))
        .role_claim_type(custom_role)
        .try_build()
        .unwrap();
    let principal = PrincipalBuilder::new(identity("user-1", "Ada", "admin"))
        .push(custom)
        .try_build()
        .unwrap();
    assert!(principal.is_in_role(&role("admin")));
    assert!(principal.is_in_role(&role("ops")));
    assert!(!principal.is_in_role(&role("root")));
}

#[test]
fn claims_are_paired_with_their_source_identity() {
    let principal = PrincipalBuilder::new(identity("user-1", "Ada", "admin"))
        .push(identity("user-2", "Grace", "auditor"))
        .try_build()
        .unwrap();
    let subs: Vec<_> = principal
        .claims()
        .filter(|sourced| sourced.claim().claim_type() == &ClaimType::from(WellKnownClaim::Sub))
        .map(|sourced| sourced.identity().key().sub().as_str().to_owned())
        .collect();
    assert_eq!(subs, ["user-1", "user-2"]);
    assert_eq!(principal.find_all(&WellKnownClaim::Sub.into()).count(), 2);
}

#[test]
fn with_identity_promotes_an_authenticated_identity_over_anonymous() {
    let principal = Principal::anonymous().with_identity(identity("user-1", "Ada", "admin"));
    assert!(principal.is_authenticated());
    let kept = principal.with_identity(identity("user-2", "Grace", "auditor"));
    assert_eq!(kept.name(), Some("Ada"));
}
