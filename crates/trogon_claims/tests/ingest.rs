#![cfg(feature = "json")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use trogon_claims::ingest::jwt_payload;
use trogon_claims::{IngestError, RawClaims, RawClaimsError};

#[test]
fn a_duplicate_member_is_rejected() {
    let error = RawClaims::from_slice(br#"{"sub":"a","sub":"b"}"#).unwrap_err();
    assert_eq!(error, RawClaimsError::Duplicate { name: "sub".to_owned() });
}

#[test]
fn a_duplicate_member_spelled_with_an_escape_is_rejected() {
    let error = RawClaims::from_slice(br#"{"sub":"a","\u0073ub":"b"}"#).unwrap_err();
    assert_eq!(error, RawClaimsError::Duplicate { name: "sub".to_owned() });
}

#[test]
fn non_objects_and_malformed_documents_are_rejected() {
    assert_eq!(RawClaims::from_slice(b"[]").unwrap_err(), RawClaimsError::NotObject);
    assert_eq!(RawClaims::from_slice(b"\"x\"").unwrap_err(), RawClaimsError::NotObject);
    assert!(matches!(
        RawClaims::from_slice(b"{\"a\":1,}").unwrap_err(),
        RawClaimsError::InvalidJson { .. }
    ));
    assert!(matches!(
        RawClaims::from_slice(b"{\"a\":1} trailing").unwrap_err(),
        RawClaimsError::InvalidJson { .. }
    ));
    assert_eq!(
        RawClaims::from_slice(br#"{"":1}"#).unwrap_err(),
        RawClaimsError::EmptyName
    );
}

#[test]
fn member_order_is_preserved() {
    let raw = RawClaims::from_slice(br#"{"z":1,"a":{"b":[1,{"c":"}"}]},"m":"x,y"}"#).unwrap();
    let names: Vec<&str> = raw.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["z", "a", "m"]);
    assert_eq!(raw.get_named("a").unwrap().as_str(), r#"{"b":[1,{"c":"}"}]}"#);
    assert_eq!(raw.get_named("m").unwrap().as_string().unwrap(), "x,y");
}

#[test]
fn a_malformed_compact_token_is_rejected() {
    for token in ["", "a.b", "a.b.c.d", "a..c"] {
        assert!(jwt_payload(token).is_err(), "{token:?}");
    }
    assert_eq!(
        jwt_payload("a.not+base64!.c").unwrap_err(),
        IngestError::Malformed(RawClaimsError::InvalidBase64)
    );
    assert_eq!(
        jwt_payload("a.b.c.d").unwrap_err(),
        IngestError::Malformed(RawClaimsError::NotCompactJwt)
    );
}

#[test]
fn a_payload_with_non_zero_leftover_bits_is_rejected() {
    assert!(jwt_payload("a.e30.c").is_ok());
    assert!(jwt_payload("a.eyAgfQ.c").is_ok());
    for token in ["a.e31.c", "a.eyAgfR.c"] {
        assert_eq!(
            jwt_payload(token).unwrap_err(),
            IngestError::Malformed(RawClaimsError::InvalidBase64),
            "{token:?}"
        );
    }
}
