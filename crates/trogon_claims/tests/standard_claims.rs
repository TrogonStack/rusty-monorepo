#![allow(clippy::unwrap_used, clippy::expect_used)]

use trogon_claims::{
    Claim, ClaimLookupError, ClaimText, ClaimValue, DynamicClaims, NumericDate, ScopeName, WellKnownClaim,
};

fn claim(name: WellKnownClaim, value: ClaimValue) -> Claim {
    Claim::builder(name.into(), value).build()
}

fn text(value: &'static str) -> ClaimValue {
    ClaimValue::Text(ClaimText::from(value))
}

#[test]
fn typed_accessors_distinguish_absent_malformed_and_valid_claims() {
    assert_eq!(DynamicClaims::default().standard().expires_at(), Ok(None));
    let malformed = DynamicClaims::from_claims(vec![claim(WellKnownClaim::Exp, text("not-a-date"))]);
    assert_eq!(
        malformed.standard().expires_at(),
        Err(ClaimLookupError::WrongType {
            claim: WellKnownClaim::Exp
        })
    );
    let date = NumericDate::try_new(100, 123_000_000).unwrap();
    let valid = DynamicClaims::from_claims(vec![claim(
        WellKnownClaim::Exp,
        ClaimValue::Timestamp(date.to_timestamp()),
    )]);
    assert_eq!(valid.standard().expires_at(), Ok(Some(date)));
}

#[test]
fn duplicate_singleton_claims_are_rejected_without_echoing_values() {
    let claims = DynamicClaims::from_claims(vec![
        claim(WellKnownClaim::ClientId, text("first-secret-value")),
        claim(WellKnownClaim::ClientId, text("second-secret-value")),
    ]);
    let error = claims.standard().client_id().unwrap_err();
    assert_eq!(
        error,
        ClaimLookupError::MultipleValues {
            claim: WellKnownClaim::ClientId
        }
    );
    assert!(!format!("{error:?} {error}").contains("secret-value"));
}

#[test]
fn standard_collections_keep_scopes_roles_and_groups_distinct() {
    let claims = DynamicClaims::from_claims(vec![
        claim(WellKnownClaim::Aud, text("service-a")),
        claim(WellKnownClaim::Scope, text("read write")),
        claim(WellKnownClaim::Roles, ClaimValue::List(vec![text("admin")])),
        claim(WellKnownClaim::Groups, ClaimValue::List(vec![text("engineering")])),
    ]);
    let standard = claims.standard();
    assert_eq!(standard.audiences().unwrap().unwrap()[0].as_str(), "service-a");
    assert!(standard
        .scopes()
        .unwrap()
        .unwrap()
        .contains(&ScopeName::try_new("read").unwrap()));
    assert_eq!(standard.roles().unwrap().unwrap()[0].as_str(), "admin");
    assert_eq!(standard.groups().unwrap().unwrap()[0].as_str(), "engineering");
}

#[cfg(feature = "json")]
fn from_json(body: &str) -> DynamicClaims {
    use trogon_claims::ingest::IngestClaims;
    use trogon_claims::{Issuer, PrincipalKey, RawClaims, SubjectId};

    let raw = RawClaims::from_slice(body.as_bytes()).unwrap();
    let key = PrincipalKey::new(
        Issuer::try_new("https://issuer.example.com").unwrap(),
        SubjectId::try_new("user").unwrap(),
    );
    DynamicClaims::try_from_raw(&raw, &key).unwrap()
}

#[cfg(feature = "json")]
#[test]
fn json_shapes_survive_projection_for_typed_lookup() {
    for body in [
        r#"{"aud":null}"#,
        r#"{"aud":{}}"#,
        r#"{"aud":["valid",null]}"#,
        r#"{"aud":["valid",{}]}"#,
    ] {
        assert!(from_json(body).standard().audiences().is_err(), "{body}");
    }
    assert_eq!(from_json(r#"{"aud":[]}"#).standard().audiences(), Ok(Some(vec![])));
    assert_eq!(from_json(r#"{}"#).standard().audiences(), Ok(None));
    assert!(from_json(r#"{"client_id":["one"]}"#).standard().client_id().is_err());
    assert!(from_json(r#"{"exp":"100"}"#).standard().expires_at().is_err());
    assert!(from_json(r#"{"roles":"admin"}"#).standard().roles().is_err());
    assert!(from_json(r#"{"groups":["engineering",false]}"#)
        .standard()
        .groups()
        .is_err());
}

#[cfg(feature = "json")]
#[test]
fn json_valid_standard_claims_preserve_precision_and_custom_claims() {
    let claims = from_json(
        r#"{"aud":["service-a","service-b"],"exp":100.125,"iat":9e1,"nbf":99,"client_id":"client-a","scope":"read write","roles":["admin"],"groups":["engineering"],"custom":true}"#,
    );
    let standard = claims.standard();
    assert_eq!(standard.audiences().unwrap().unwrap().len(), 2);
    assert_eq!(
        standard.expires_at(),
        Ok(Some(NumericDate::try_new(100, 125_000_000).unwrap()))
    );
    assert_eq!(standard.issued_at(), Ok(Some(NumericDate::try_new(90, 0).unwrap())));
    assert_eq!(standard.not_before(), Ok(Some(NumericDate::try_new(99, 0).unwrap())));
    assert_eq!(standard.client_id().unwrap().unwrap().as_str(), "client-a");
    assert_eq!(standard.scopes().unwrap().unwrap().scopes().len(), 2);
    assert_eq!(standard.roles().unwrap().unwrap()[0].as_str(), "admin");
    assert_eq!(standard.groups().unwrap().unwrap()[0].as_str(), "engineering");
    assert!(claims.has_claim(&trogon_claims::ClaimType::try_new("custom").unwrap()));
}

#[test]
fn scope_tokens_reject_ambiguous_delimiters_and_unsafe_characters() {
    for value in ["", "two words", "quote\"", "slash\\", "caf\u{e9}"] {
        assert!(ScopeName::try_new(value.to_owned()).is_err());
    }
    assert!(trogon_claims::ScopeSet::try_from_space_delimited("read  write").is_err());
    assert!(trogon_claims::ScopeSet::try_from_space_delimited(" read").is_err());
    let repeated = trogon_claims::ScopeSet::try_from_space_delimited("read read").unwrap();
    assert_eq!(repeated.scopes().len(), 2);
    assert!(repeated.contains(&ScopeName::try_new("read").unwrap()));
}

#[test]
fn text_boolean_and_name_claims_have_no_implicit_coercions_or_fallbacks() {
    let claims = DynamicClaims::from_claims(vec![
        claim(WellKnownClaim::Iss, text("https://issuer.example.com")),
        claim(WellKnownClaim::Sub, text("user-42")),
        claim(WellKnownClaim::Upn, text("principal-name")),
        claim(WellKnownClaim::EmailVerified, ClaimValue::Boolean(true)),
    ]);
    let standard = claims.standard();
    assert_eq!(
        standard.issuer().unwrap().unwrap().as_str(),
        "https://issuer.example.com"
    );
    assert_eq!(standard.subject().unwrap().unwrap().as_str(), "user-42");
    assert_eq!(standard.upn().unwrap().unwrap().as_str(), "principal-name");
    assert_eq!(standard.preferred_username(), Ok(None));
    assert_eq!(standard.email_verified(), Ok(Some(true)));
    #[cfg(feature = "ws-claims")]
    assert_ne!(WellKnownClaim::Upn.as_str(), WellKnownClaim::WsUpn.as_str());
    assert_eq!(WellKnownClaim::from_name("upn"), Some(WellKnownClaim::Upn));
    let malformed = DynamicClaims::from_claims(vec![claim(WellKnownClaim::EmailVerified, text("true"))]);
    assert!(malformed.standard().email_verified().is_err());
}

#[test]
fn timestamp_views_preserve_the_instant_instead_of_applying_the_display_offset() {
    use trogon_claims::{Timestamp, UtcOffset};

    let timestamp = Timestamp::try_new(100, 125_000_000, UtcOffset::try_from_minutes(60).unwrap()).unwrap();
    let claims = DynamicClaims::from_claims(vec![claim(WellKnownClaim::Iat, ClaimValue::Timestamp(timestamp))]);
    assert_eq!(
        claims.standard().issued_at(),
        Ok(Some(NumericDate::try_new(100, 125_000_000).unwrap()))
    );
}
