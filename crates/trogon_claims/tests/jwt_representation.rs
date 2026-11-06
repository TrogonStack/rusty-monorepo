#![cfg(feature = "json")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{json, Value};
use trogon_claims::ingest::jwt_payload;
use trogon_claims::{ActorError, ClaimValueError, IngestError, RawClaims, WellKnownClaim};

fn compact(value: &Value) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let bytes = serde_json::to_vec(value).unwrap();
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        encoded.push(char::from(ALPHABET[usize::from(first >> 2)]));
        let second = chunk.get(1).copied().unwrap_or_default();
        encoded.push(char::from(ALPHABET[usize::from((first & 3) << 4 | second >> 4)]));
        if chunk.len() > 1 {
            let third = chunk.get(2).copied().unwrap_or_default();
            encoded.push(char::from(ALPHABET[usize::from((second & 15) << 2 | third >> 6)]));
            if chunk.len() > 2 {
                encoded.push(char::from(ALPHABET[usize::from(third & 63)]));
            }
        }
    }
    format!("header.{encoded}.signature")
}

#[test]
fn jwt_rejects_colon_strings_that_are_not_uris() {
    for name in ["iss", "sub", "aud"] {
        for value in [
            "bad scheme:value",
            "1scheme:value",
            "/relative:path",
            "https://example.com/a b",
            "https://example.com/%zz",
            "urn:example:é",
        ] {
            let claims = json!({name: value});
            assert!(jwt_payload(&compact(&claims)).is_err(), "{claims}");
        }
    }
}

#[test]
fn jwt_rejects_wrong_registered_string_claim_shapes() {
    for name in ["iss", "sub"] {
        for value in [json!(null), json!(42), json!(true), json!([]), json!({})] {
            let claims = json!({name: value});
            assert!(jwt_payload(&compact(&claims)).is_err(), "{claims}");
        }
    }
    for value in [json!(null), json!(42), json!(true), json!({}), json!(["ok", 7])] {
        let claims = json!({"aud": value});
        assert!(jwt_payload(&compact(&claims)).is_err(), "{claims}");
    }
}

#[test]
fn jwt_checks_every_audience_entry() {
    let claims = json!({"aud": ["urn:example:good", "https://example.com/%xx"]});
    assert!(matches!(
        jwt_payload(&compact(&claims)),
        Err(IngestError::Claim { name, kind: ClaimValueError::InvalidStringOrUri })
            if name.well_known() == Some(WellKnownClaim::Aud)
    ));
}

#[test]
fn jwt_preserves_string_or_uri_values_exactly() {
    for value in [
        "",
        "user@example.com#fragment",
        "spaces and Unicode é",
        "line\nbreak",
        "urn:example:ABC",
        "HTTPS://Example.COM/%2f?q=%41#Fragment",
        "custom:",
    ] {
        let token = compact(&json!({"iss": value, "sub": value, "aud": [value]}));
        let (raw, _) = jwt_payload(&token).unwrap();
        assert_eq!(raw.get_named("iss").unwrap().as_string().unwrap(), value);
        assert_eq!(raw.get_named("sub").unwrap().as_string().unwrap(), value);
        assert_eq!(
            raw.get_named("aud").unwrap().as_array().unwrap()[0]
                .as_string()
                .unwrap(),
            value
        );
    }
}

#[test]
fn jwt_does_not_impose_requiredness_or_token_acceptance() {
    for claims in [json!({}), json!({"aud": []}), json!({"exp": 0, "nbf": 9999999999_i64})] {
        assert!(jwt_payload(&compact(&claims)).is_ok(), "{claims}");
    }
}

#[test]
fn generic_raw_claims_do_not_inherit_jwt_uri_restrictions() {
    let raw = RawClaims::from_slice(br#"{"iss":"opaque issuer: value","sub":17,"aud":null}"#).unwrap();
    assert_eq!(
        raw.get_named("iss").unwrap().as_string().unwrap(),
        "opaque issuer: value"
    );
}

#[test]
fn jwt_checks_actor_identity_claims_in_nested_actor_chains() {
    for claims in [
        json!({"act": {"sub": "bad scheme:value"}}),
        json!({"may_act": {"iss": "https://example.com/%xx"}}),
        json!({"act": {"sub": "user", "act": {"iss": "bad scheme:value"}}}),
    ] {
        let error = jwt_payload(&compact(&claims)).unwrap_err();
        assert!(matches!(
            error,
            IngestError::Claim {
                kind: ClaimValueError::InvalidStringOrUri,
                ..
            }
        ));
    }
}

#[test]
fn jwt_actor_checks_do_not_apply_audience_acceptance_to_actor_extras() {
    let claims = json!({"act": {"sub": "urn:actor:User", "aud": "bad scheme:value", "exp": 0}});
    assert!(jwt_payload(&compact(&claims)).is_ok());
}

#[test]
fn jwt_actor_parsing_uses_the_existing_depth_limit() {
    let mut actor = json!({"sub": "actor"});
    for _ in 1..10 {
        actor = json!({"sub": "actor", "act": actor});
    }
    assert!(jwt_payload(&compact(&json!({"act": actor.clone()}))).is_ok());
    let claims = json!({"act": {"sub": "actor", "act": actor}});
    assert!(matches!(
        jwt_payload(&compact(&claims)),
        Err(IngestError::Claim {
            kind: ClaimValueError::InvalidActor(ActorError::TooDeep { depth: 11, max: 10 }),
            ..
        })
    ));
}

#[test]
fn jwt_rejects_malformed_actor_objects_without_exposing_input() {
    for name in ["act", "may_act"] {
        for value in [json!(null), json!("opaque sensitive payload"), json!(17), json!([])] {
            let error = jwt_payload(&compact(&json!({name: value}))).unwrap_err();
            assert!(matches!(
                error,
                IngestError::Claim {
                    kind: ClaimValueError::WrongType { expected: "object" },
                    ..
                }
            ));
            assert!(!error.to_string().contains("opaque sensitive payload"));
        }
    }
}
