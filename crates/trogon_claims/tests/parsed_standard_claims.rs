#![allow(clippy::unwrap_used, clippy::expect_used)]

use trogon_claims::{
    Claim, ClaimLookupError, ClaimText, ClaimType, ClaimValue, DynamicClaims, NumericDate, ParsedStandardClaims,
    ScopeName, WellKnownClaim,
};

fn claim(name: WellKnownClaim, value: ClaimValue) -> Claim {
    Claim::builder(name.into(), value).build()
}

fn text(value: &'static str) -> ClaimValue {
    ClaimValue::Text(ClaimText::from(value))
}

#[test]
fn parsed_claims_expose_typed_values_without_fallible_accessors() {
    let instant = NumericDate::try_new(100, 125_000_000).unwrap();
    let claims = DynamicClaims::from_claims(vec![
        claim(WellKnownClaim::Iss, text("LOCAL_AUTHORITY")),
        claim(WellKnownClaim::Sub, text("用户@example.com#fragment")),
        claim(WellKnownClaim::PreferredUsername, text("Alice.Example")),
        claim(WellKnownClaim::Upn, text("Alice@EXAMPLE.COM")),
        claim(WellKnownClaim::Name, text("")),
        claim(WellKnownClaim::Email, text("Alice@EXAMPLE.COM")),
        claim(WellKnownClaim::EmailVerified, ClaimValue::Boolean(true)),
        claim(
            WellKnownClaim::Aud,
            ClaimValue::List(vec![text("Service A"), text("Service B")]),
        ),
        claim(WellKnownClaim::Exp, ClaimValue::Timestamp(instant.to_timestamp())),
        claim(WellKnownClaim::Iat, ClaimValue::Integer(90)),
        claim(WellKnownClaim::Nbf, ClaimValue::Integer(99)),
        claim(WellKnownClaim::AuthTime, ClaimValue::Integer(80)),
        claim(
            WellKnownClaim::Amr,
            ClaimValue::List(vec![text("pwd"), text("Custom-Device")]),
        ),
        claim(WellKnownClaim::ClientId, text("Client A")),
        claim(WellKnownClaim::Scope, text("read:all write read:all")),
        claim(WellKnownClaim::Roles, ClaimValue::List(vec![text("Admin Role")])),
        claim(WellKnownClaim::Groups, ClaimValue::List(vec![text("Équipe")])),
        Claim::builder(ClaimType::try_new("custom").unwrap(), ClaimValue::Boolean(true)).build(),
    ]);

    let parsed: ParsedStandardClaims<'_> = claims.parse_standard().unwrap();
    assert_eq!(parsed.issuer().unwrap().as_str(), "LOCAL_AUTHORITY");
    assert_eq!(parsed.subject().unwrap().as_str(), "用户@example.com#fragment");
    assert_eq!(parsed.preferred_username().unwrap().as_str(), "Alice.Example");
    assert_eq!(parsed.upn().unwrap().as_str(), "Alice@EXAMPLE.COM");
    assert_eq!(parsed.display_name().unwrap().as_str(), "");
    assert_eq!(parsed.email().unwrap().as_str(), "Alice@EXAMPLE.COM");
    assert_eq!(parsed.email_verified(), Some(true));
    assert_eq!(parsed.audiences().unwrap()[1].as_str(), "Service B");
    assert_eq!(parsed.expires_at(), Some(instant));
    assert_eq!(parsed.issued_at(), Some(NumericDate::try_new(90, 0).unwrap()));
    assert_eq!(parsed.not_before(), Some(NumericDate::try_new(99, 0).unwrap()));
    assert_eq!(parsed.authenticated_at(), Some(NumericDate::try_new(80, 0).unwrap()));
    assert_eq!(parsed.authentication_methods().unwrap()[1].as_str(), "Custom-Device");
    assert_eq!(parsed.client_id().unwrap().as_str(), "Client A");
    assert_eq!(parsed.scopes().unwrap().scopes().len(), 3);
    assert!(parsed
        .scopes()
        .unwrap()
        .contains(&ScopeName::try_new("read:all").unwrap()));
    assert_eq!(parsed.roles().unwrap()[0].as_str(), "Admin Role");
    assert_eq!(parsed.groups().unwrap()[0].as_str(), "Équipe");
    assert!(std::ptr::eq(parsed.issuer().unwrap(), parsed.issuer().unwrap()));
    assert!(std::ptr::eq(parsed.audiences().unwrap(), parsed.audiences().unwrap()));
    assert!(std::ptr::eq(
        parsed.email().unwrap(),
        claims.standard().email().unwrap().unwrap()
    ));
    assert!(claims.has_claim(&ClaimType::try_new("custom").unwrap()));
    assert_eq!(parsed, claims.standard().try_parse().unwrap());
    assert_eq!(parsed, ParsedStandardClaims::try_from(&claims).unwrap());
}

#[test]
fn absent_claims_remain_absent_without_implicit_defaults() {
    let claims = DynamicClaims::default();
    let parsed = claims.parse_standard().unwrap();
    assert_eq!(parsed.issuer(), None);
    assert_eq!(parsed.subject(), None);
    assert_eq!(parsed.preferred_username(), None);
    assert_eq!(parsed.upn(), None);
    assert_eq!(parsed.display_name(), None);
    assert_eq!(parsed.email(), None);
    assert_eq!(parsed.email_verified(), None);
    assert_eq!(parsed.audiences(), None);
    assert_eq!(parsed.expires_at(), None);
    assert_eq!(parsed.issued_at(), None);
    assert_eq!(parsed.not_before(), None);
    assert_eq!(parsed.authenticated_at(), None);
    assert_eq!(parsed.authentication_methods(), None);
    assert_eq!(parsed.client_id(), None);
    assert_eq!(parsed.scopes(), None);
    assert_eq!(parsed.roles(), None);
    assert_eq!(parsed.groups(), None);
}

#[test]
fn optional_malformed_claims_fail_the_explicit_typed_conversion() {
    for (name, value, expected) in [
        (
            WellKnownClaim::EmailVerified,
            text("true"),
            ClaimLookupError::WrongType {
                claim: WellKnownClaim::EmailVerified,
            },
        ),
        (
            WellKnownClaim::Exp,
            text("100"),
            ClaimLookupError::WrongType {
                claim: WellKnownClaim::Exp,
            },
        ),
        (
            WellKnownClaim::Scope,
            text("read  write"),
            ClaimLookupError::InvalidValue {
                claim: WellKnownClaim::Scope,
            },
        ),
        (
            WellKnownClaim::Amr,
            ClaimValue::List(vec![text("")]),
            ClaimLookupError::InvalidValue {
                claim: WellKnownClaim::Amr,
            },
        ),
    ] {
        let claims = DynamicClaims::from_claims(vec![claim(name, value)]);
        assert_eq!(claims.parse_standard().unwrap_err(), expected);
        assert_eq!(claims.claims().len(), 1);
    }
}

#[test]
fn repeated_singletons_fail_without_returning_a_partial_snapshot() {
    let claims = DynamicClaims::from_claims(vec![
        claim(WellKnownClaim::Sub, text("first")),
        claim(WellKnownClaim::Sub, text("second")),
    ]);
    assert_eq!(
        claims.parse_standard().unwrap_err(),
        ClaimLookupError::MultipleValues {
            claim: WellKnownClaim::Sub,
        }
    );
}

#[test]
fn present_empty_collections_remain_distinct_from_absent_collections() {
    let claims = DynamicClaims::from_claims(vec![
        claim(WellKnownClaim::Aud, ClaimValue::List(vec![])),
        claim(WellKnownClaim::Amr, ClaimValue::List(vec![])),
        claim(WellKnownClaim::Roles, ClaimValue::List(vec![])),
        claim(WellKnownClaim::Groups, ClaimValue::List(vec![])),
    ]);
    let parsed = claims.parse_standard().unwrap();
    assert_eq!(parsed.audiences(), Some([].as_slice()));
    assert_eq!(parsed.authentication_methods(), Some([].as_slice()));
    assert_eq!(parsed.roles(), Some([].as_slice()));
    assert_eq!(parsed.groups(), Some([].as_slice()));
}

#[cfg(feature = "json")]
fn from_json(body: &str) -> DynamicClaims {
    use trogon_claims::ingest::IngestClaims;
    use trogon_claims::{Issuer, PrincipalKey, RawClaims, SubjectId};

    let raw = RawClaims::from_slice(body.as_bytes()).unwrap();
    let key = PrincipalKey::new(
        Issuer::try_new("local").unwrap(),
        SubjectId::try_new("subject").unwrap(),
    );
    DynamicClaims::try_from_raw(&raw, &key).unwrap()
}

#[cfg(feature = "json")]
#[test]
fn json_snapshot_retains_collection_shapes_and_timestamp_precision() {
    let claims = from_json(
        r#"{"aud":[],"roles":[],"groups":[],"amr":[],"exp":100.123456789,"email_verified":false,"custom":{"nested":true}}"#,
    );
    let parsed = claims.parse_standard().unwrap();
    assert_eq!(parsed.audiences(), Some([].as_slice()));
    assert_eq!(parsed.roles(), Some([].as_slice()));
    assert_eq!(parsed.groups(), Some([].as_slice()));
    assert_eq!(parsed.authentication_methods(), Some([].as_slice()));
    assert_eq!(parsed.email_verified(), Some(false));
    assert_eq!(
        parsed.expires_at(),
        Some(NumericDate::try_new(100, 123_456_789).unwrap())
    );
    assert_eq!(claims.unprojected()[0].as_str(), "custom");
}

#[cfg(feature = "json")]
#[test]
fn json_snapshot_rejects_malformed_present_claims_before_access() {
    for body in [
        r#"{"roles":"admin"}"#,
        r#"{"groups":["valid",null]}"#,
        r#"{"aud":["valid",{}]}"#,
        r#"{"amr":"pwd"}"#,
        r#"{"email_verified":null}"#,
        r#"{"client_id":["client"]}"#,
        r#"{"preferred_username":{}}"#,
    ] {
        assert!(from_json(body).parse_standard().is_err(), "{body}");
    }
}
