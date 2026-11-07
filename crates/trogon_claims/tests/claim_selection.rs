#![allow(clippy::unwrap_used, clippy::expect_used)]

use trogon_claims::{
    AuthenticationType, Claim, ClaimText, ClaimType, ClaimValue, Identity, IdentityBuilder, Issuer, NameClaimType,
    Principal, RoleClaimType, RoleName, WellKnownClaim,
};

fn builder(name: WellKnownClaim, role: WellKnownClaim, username: Option<&'static str>) -> IdentityBuilder {
    let mut builder = IdentityBuilder::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(name.into()),
        RoleClaimType::new(role.into()),
    );
    for (name, value) in [
        (WellKnownClaim::Iss.into(), "https://issuer.example.com"),
        (WellKnownClaim::Sub.into(), "user-42"),
        (WellKnownClaim::Name.into(), "Display Name"),
        (
            ClaimType::try_new("http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name").unwrap(),
            "Windows Name",
        ),
        (
            ClaimType::try_new("http://schemas.microsoft.com/ws/2008/06/identity/claims/role").unwrap(),
            "ws-admin",
        ),
        (WellKnownClaim::Roles.into(), "jwt-admin"),
        (WellKnownClaim::Groups.into(), "group-admin"),
    ] {
        builder = builder.push(
            Claim::builder(name, ClaimValue::Text(ClaimText::from(value)))
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

fn role_identity(selected: WellKnownClaim, values: Vec<ClaimValue>) -> Identity {
    let mut builder = IdentityBuilder::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::Sub.into()),
        RoleClaimType::new(selected.into()),
    )
    .push(Claim::builder(WellKnownClaim::Iss.into(), ClaimValue::Text(ClaimText::from("issuer"))).build())
    .push(Claim::builder(WellKnownClaim::Sub.into(), ClaimValue::Text(ClaimText::from("subject"))).build());
    for value in values {
        builder = builder.push(Claim::builder(selected.into(), value).build());
    }
    builder.try_build().unwrap()
}

fn text(value: &'static str) -> ClaimValue {
    ClaimValue::Text(ClaimText::from(value))
}

fn assert_role_membership(identity: &Identity, role: &str, expected: bool) {
    let role = RoleName::try_new(role.to_owned()).unwrap();
    assert_eq!(identity.as_authenticated().unwrap().is_in_role(&role), expected);
    assert_eq!(Principal::new(identity.clone()).is_in_role(&role), expected);
}

#[test]
fn selected_role_lists_match_flat_claims_in_identity_and_principal_queries() {
    for selected in [
        WellKnownClaim::Roles,
        WellKnownClaim::Groups,
        #[cfg(feature = "ws-claims")]
        WellKnownClaim::WsRole,
    ] {
        for values in [
            vec![text("reader"), text("admin")],
            vec![ClaimValue::List(vec![text("reader"), text("admin")])],
        ] {
            let identity = role_identity(selected, values);
            assert_role_membership(&identity, "reader", true);
            assert_role_membership(&identity, "admin", true);
            assert_role_membership(&identity, "operator", false);
        }
    }
}

#[test]
fn malformed_role_collections_do_not_grant_partial_membership() {
    for values in [
        vec![ClaimValue::List(vec![text("admin"), ClaimValue::Boolean(true)])],
        vec![ClaimValue::List(vec![text("admin"), text("")])],
        vec![ClaimValue::List(vec![ClaimValue::List(vec![text("admin")])])],
        vec![ClaimValue::List(vec![text("admin")]), text("operator")],
        vec![text("admin"), ClaimValue::Boolean(true)],
        vec![text("admin"), ClaimValue::List(vec![text("operator")])],
        vec![text("admin"), text("")],
        vec![ClaimValue::List(vec![])],
        vec![],
    ] {
        let identity = role_identity(WellKnownClaim::Roles, values);
        assert_role_membership(&identity, "admin", false);
        assert_role_membership(&identity, "operator", false);
    }
}

#[cfg(feature = "json")]
#[test]
fn manually_constructed_role_lists_match_json_ingest_queries() {
    let manual = role_identity(WellKnownClaim::Roles, vec![ClaimValue::List(vec![text("jwt-admin")])]);
    let ingested = ingest(WellKnownClaim::Sub, WellKnownClaim::Roles, None);
    for identity in [manual, ingested] {
        let roles = identity.as_authenticated().unwrap().claims().standard().roles();
        assert_eq!(roles.unwrap().unwrap(), vec![RoleName::try_new("jwt-admin").unwrap()]);
        assert_role_membership(&identity, "jwt-admin", true);
        assert_role_membership(&identity, "group-admin", false);
    }
}

#[cfg(feature = "json")]
#[test]
fn malformed_json_role_arrays_do_not_grant_partial_membership() {
    use trogon_claims::{IngestProfile, RawClaims};

    let profile = IngestProfile::new(
        AuthenticationType::try_new("jwt").unwrap(),
        NameClaimType::new(WellKnownClaim::Sub.into()),
        RoleClaimType::new(WellKnownClaim::Roles.into()),
    );
    for body in [
        r#"{"iss":"issuer","sub":"subject","roles":["admin",null]}"#,
        r#"{"iss":"issuer","sub":"subject","roles":["admin",{}]}"#,
        r#"{"iss":"issuer","sub":"subject","roles":["admin",["admin"]]}"#,
        r#"{"iss":"issuer","sub":"subject","roles":["admin",true]}"#,
        r#"{"iss":"issuer","sub":"subject","roles":["admin",""]}"#,
    ] {
        let raw = RawClaims::from_slice(body.as_bytes()).unwrap();
        let identity: Identity = Identity::ingest(&raw, &profile).unwrap().into_identity();
        assert!(identity
            .as_authenticated()
            .unwrap()
            .claims()
            .standard()
            .roles()
            .is_err());
        assert_role_membership(&identity, "admin", false);
    }
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
        "http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name": "Windows Name",
        "http://schemas.microsoft.com/ws/2008/06/identity/claims/role": "ws-admin"
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
