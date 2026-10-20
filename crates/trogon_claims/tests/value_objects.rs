#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;

use trogon_claims::{
    object_id, object_type, Audience, AuthenticationType, ClaimPropertyKey, ClaimText, ClaimType, ClaimValue, ClientId,
    Issuer, ObjectId, ObjectType, RoleName, SubjectId, Unsigned63, ValueKind, WellKnownClaim,
};

#[test]
fn non_empty_value_objects_reject_empty_and_keep_everything_else_exact() {
    assert!(Issuer::try_new("").is_err());
    assert!(Audience::try_new("").is_err());
    assert!(AuthenticationType::try_new("").is_err());
    assert!(RoleName::try_new("").is_err());
    assert!(ClaimPropertyKey::try_new("").is_err());
    assert!(SubjectId::try_new("").is_err());
    assert!(ClaimType::try_new("").is_err());

    let issuer = Issuer::try_new("https://x.example.com/").unwrap();
    assert_ne!(issuer, Issuer::try_new("https://x.example.com").unwrap());
    assert_eq!(issuer.as_str(), "https://x.example.com/");
    assert_ne!(RoleName::try_new("Admin").unwrap(), RoleName::try_new("admin").unwrap());
    assert!(ClaimType::try_new("   ").is_ok());
    assert!(SubjectId::try_new("user@example.com").is_ok());
}

#[test]
fn claim_text_accepts_empty_text() {
    let empty = ClaimText::from("");
    assert_eq!(empty.as_str(), "");
    assert_eq!(ClaimText::from(String::from("a")).into_string(), "a");
}

#[test]
fn client_id_is_printable_ascii_only() {
    assert!(ClientId::try_new("my client~1").is_ok());
    assert!(ClientId::try_new("").is_err());
    assert!(ClientId::try_new("tab\there").is_err());
    assert!(ClientId::try_new("caf\u{e9}").is_err());
}

#[test]
fn unsigned63_stops_at_i64_max() {
    assert_eq!(Unsigned63::try_new(i64::MAX as u64).unwrap().get(), i64::MAX as u64);
    let err = Unsigned63::try_new(i64::MAX as u64 + 1).unwrap_err();
    assert_eq!(err.value(), i64::MAX as u64 + 1);
    assert!(Unsigned63::try_from(u64::MAX).is_err());
    assert_eq!(u64::from(Unsigned63::from(7_u32)), 7);
}

#[test]
fn object_id_reports_kind_and_position() {
    let err = ObjectId::try_new("a@b").unwrap_err();
    assert_eq!(err.kind(), ValueKind::ObjectId);
    assert_eq!(
        err.to_string(),
        "object_id: invalid character '@' at byte 1 (allowed: [a-zA-Z0-9/_|\\-=+])"
    );
    assert!(ObjectId::try_new("*").is_err());
    assert!("readme".parse::<ObjectId>().is_ok());
    assert!(ObjectId::try_from(String::from("a b")).is_err());
}

#[test]
fn object_type_follows_the_definition_name_grammar() {
    assert!(ObjectType::try_new("myapp/document").is_ok());
    assert!(ObjectType::try_new("Document").is_err());
    assert!(ObjectType::try_new("ab").is_err());
}

#[test]
fn literal_macros_build_the_newtypes() {
    let id: ObjectId = object_id!("readme");
    let kind: ObjectType = object_type!("myapp/document");
    assert_eq!(id.as_str(), "readme");
    assert_eq!(kind.to_string(), "myapp/document");
    assert_eq!(id, ObjectId::try_new("readme").unwrap());
}

#[test]
fn claim_type_matches_exactly_and_only_exactly() {
    let sub = ClaimType::try_new("sub").unwrap();
    assert_eq!(sub.well_known(), Some(WellKnownClaim::Sub));
    assert_eq!(sub, ClaimType::from(WellKnownClaim::Sub));
    let upper = ClaimType::try_new("Sub").unwrap();
    assert_eq!(upper.well_known(), None);
    assert_ne!(sub, upper);

    let uri = ClaimType::try_new("http://schemas.xmlsoap.org/ws/2005/05/identity/claims/nameidentifier").unwrap();
    assert_eq!(uri.well_known(), Some(WellKnownClaim::WsNameIdentifier));
    assert_ne!(uri, sub);

    let set: HashSet<ClaimType> = [sub.clone(), ClaimType::from(WellKnownClaim::Sub), upper]
        .into_iter()
        .collect();
    assert_eq!(set.len(), 2);
}

#[test]
fn well_known_claims_round_trip_through_their_spelling() {
    for claim in [
        WellKnownClaim::Iss,
        WellKnownClaim::MayAct,
        WellKnownClaim::WsRole,
        WellKnownClaim::WsX500DistinguishedName,
        WellKnownClaim::WsActor,
    ] {
        assert_eq!(WellKnownClaim::from_name(claim.as_str()), Some(claim));
    }
    assert_eq!(WellKnownClaim::from_name("unknown"), None);
}

#[test]
fn claim_value_types_are_pinned_per_variant() {
    use std::time::Duration;
    let text = ClaimValue::from(ClaimText::from("x"));
    assert_eq!(text.value_type(), Some("http://www.w3.org/2001/XMLSchema#string"));
    assert_eq!(
        ClaimValue::from(1_i64).value_type(),
        Some("http://www.w3.org/2001/XMLSchema#integer64")
    );
    assert_eq!(
        ClaimValue::from(Unsigned63::from(1_u32)).value_type(),
        Some("http://www.w3.org/2001/XMLSchema#uinteger64")
    );
    assert_eq!(
        ClaimValue::Double(1.5).value_type(),
        Some("http://www.w3.org/2001/XMLSchema#double")
    );
    assert_eq!(
        ClaimValue::from(true).value_type(),
        Some("http://www.w3.org/2001/XMLSchema#boolean")
    );
    assert_eq!(
        ClaimValue::from(Duration::from_secs(1)).value_type(),
        Some("http://www.w3.org/TR/2002/WD-xquery-operators-20020816#dayTimeDuration")
    );
    assert_eq!(
        ClaimValue::Bytes(vec![1]).value_type(),
        Some("http://www.w3.org/2001/XMLSchema#base64Binary")
    );
    assert_eq!(ClaimValue::List(vec![]).value_type(), None);
    assert_eq!(ClaimValue::Map(Default::default()).value_type(), None);
}
