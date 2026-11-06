#![allow(clippy::unwrap_used, clippy::expect_used)]

use trogon_claims::{
    AuthenticationType, Claim, ClaimText, ClaimValue, Identity, IdentityBuilder, Issuer, NameClaimType, RoleClaimType,
    RoleName, WellKnownClaim,
};

fn builder(name: WellKnownClaim, role: WellKnownClaim, username: Option<&'static str>) -> IdentityBuilder {
    let mut builder = IdentityBuilder::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(name.into()),
        RoleClaimType::new(role.into()),
    );
    for (name, value) in [
        (WellKnownClaim::Iss, "https://issuer.example.com"),
        (WellKnownClaim::Sub, "user-42"),
        (WellKnownClaim::Name, "Display Name"),
        (WellKnownClaim::WsName, "Windows Name"),
        (WellKnownClaim::WsRole, "ws-admin"),
        (WellKnownClaim::Roles, "jwt-admin"),
        (WellKnownClaim::Groups, "group-admin"),
    ] {
        builder = builder.push(
            Claim::builder(name.into(), ClaimValue::Text(ClaimText::from(value)))
                .issuer(Issuer::try_new("https://issuer.example.com").unwrap())
                .build(),
        );
    }
    if let Some(username) = username {
        builder = builder.push(
            Claim::builder(
                WellKnownClaim::PreferredUsername.into(),
                ClaimValue::Text(ClaimText::from(username)),
            )
            .issuer(Issuer::try_new("https://issuer.example.com").unwrap())
            .build(),
        );
    }
    builder
}

fn assert_selection(identity: &Identity, name: Option<&str>, role: &str, excluded_roles: &[&str]) {
    let identity = identity.as_authenticated().unwrap();
    assert_eq!(identity.name(), name);
    assert!(identity.is_in_role(&RoleName::try_new(role.to_owned()).unwrap()));
    for excluded in excluded_roles {
        assert!(!identity.is_in_role(&RoleName::try_new((*excluded).to_owned()).unwrap()));
    }
    assert_eq!(identity.key().issuer().as_str(), "https://issuer.example.com");
    assert_eq!(identity.key().sub().as_str(), "user-42");
}

#[test]
fn builder_uses_only_the_explicit_name_and_role_claims() {
    let identity = builder(WellKnownClaim::PreferredUsername, WellKnownClaim::Roles, Some("alex"))
        .try_build()
        .unwrap();
    assert_selection(&identity, Some("alex"), "jwt-admin", &["ws-admin", "group-admin"]);
    let identity = identity.as_authenticated().unwrap();
    let name: &NameClaimType = identity.name_claim_type();
    let role: &RoleClaimType = identity.role_claim_type();
    assert_eq!(name.claim_type(), &WellKnownClaim::PreferredUsername.into());
    assert_eq!(role.claim_type(), &WellKnownClaim::Roles.into());
}

#[test]
fn builder_does_not_fall_back_when_the_selected_name_is_absent() {
    let identity = builder(WellKnownClaim::PreferredUsername, WellKnownClaim::Roles, None)
        .try_build()
        .unwrap();
    assert_selection(&identity, None, "jwt-admin", &["ws-admin", "group-admin"]);
}

#[test]
fn builder_can_explicitly_use_subject_names_and_group_roles() {
    let identity = builder(WellKnownClaim::Sub, WellKnownClaim::Groups, Some("alex"))
        .try_build()
        .unwrap();
    assert_selection(&identity, Some("user-42"), "group-admin", &["ws-admin", "jwt-admin"]);
}

#[test]
fn non_text_selected_claims_do_not_trigger_name_or_role_fallback() {
    let identity = builder(WellKnownClaim::PreferredUsername, WellKnownClaim::Roles, None)
        .push(Claim::builder(WellKnownClaim::PreferredUsername.into(), ClaimValue::Boolean(true)).build())
        .try_build()
        .unwrap();
    assert_eq!(identity.as_authenticated().unwrap().name(), None);

    let identity = IdentityBuilder::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::Sub.into()),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
    )
    .push(Claim::builder(WellKnownClaim::Iss.into(), ClaimValue::Text(ClaimText::from("issuer"))).build())
    .push(Claim::builder(WellKnownClaim::Sub.into(), ClaimValue::Text(ClaimText::from("subject"))).build())
    .push(Claim::builder(WellKnownClaim::Roles.into(), ClaimValue::Boolean(true)).build())
    .push(
        Claim::builder(
            WellKnownClaim::Groups.into(),
            ClaimValue::Text(ClaimText::from("admin")),
        )
        .build(),
    )
    .try_build()
    .unwrap();
    assert!(!identity
        .as_authenticated()
        .unwrap()
        .is_in_role(&RoleName::try_new("admin").unwrap()));
}

#[cfg(feature = "json")]
fn ingest(name: WellKnownClaim, role: WellKnownClaim, username: Option<&str>) -> Identity {
    use trogon_claims::{DynamicClaims, IngestProfile, RawClaims};

    let mut payload = serde_json::json!({
        "iss": "https://issuer.example.com",
        "sub": "user-42",
        "name": "Display Name",
        "roles": ["jwt-admin"],
        "groups": ["group-admin"],
        WellKnownClaim::WsName.as_str(): "Windows Name",
        WellKnownClaim::WsRole.as_str(): "ws-admin"
    });
    if let Some(username) = username {
        payload["preferred_username"] = serde_json::Value::String(username.to_owned());
    }
    let raw = RawClaims::from_slice(&serde_json::to_vec(&payload).unwrap()).unwrap();
    let profile = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(name.into()),
        RoleClaimType::new(role.into()),
    );
    let name_selection: &NameClaimType = profile.name_claim_type();
    let role_selection: &RoleClaimType = profile.role_claim_type();
    assert_eq!(name_selection.claim_type(), &name.into());
    assert_eq!(role_selection.claim_type(), &role.into());
    Identity::<DynamicClaims>::ingest(&raw, &profile)
        .unwrap()
        .into_identity()
}

#[cfg(feature = "json")]
#[test]
fn ingest_uses_only_the_explicit_name_and_role_claims() {
    let identity = ingest(WellKnownClaim::PreferredUsername, WellKnownClaim::Roles, Some("alex"));
    assert_selection(&identity, Some("alex"), "jwt-admin", &["ws-admin", "group-admin"]);
}

#[cfg(feature = "json")]
#[test]
fn ingest_does_not_fall_back_when_the_selected_name_is_absent() {
    let identity = ingest(WellKnownClaim::PreferredUsername, WellKnownClaim::Roles, None);
    assert_selection(&identity, None, "jwt-admin", &["ws-admin", "group-admin"]);
}

#[cfg(feature = "json")]
#[test]
fn ingest_can_explicitly_use_subject_names_and_group_roles() {
    let identity = ingest(WellKnownClaim::Sub, WellKnownClaim::Groups, Some("alex"));
    assert_selection(&identity, Some("user-42"), "group-admin", &["ws-admin", "jwt-admin"]);
}
