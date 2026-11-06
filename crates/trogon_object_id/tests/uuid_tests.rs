#![cfg(feature = "uuid")]

use trogon_object_id::{object_id, ParseError, Uuid, Uuid4, Uuid7};
object_id! { struct SessionId("session", payload = Uuid); }

#[test]
fn parses_uuid_into_a_canonical_binary_payload() {
    let raw = "550E8400-E29B-41D4-A716-446655440000";
    let id = SessionId::try_from_raw(raw).expect("valid uppercase UUID");
    assert_eq!(
        id.as_raw().to_string(),
        raw.to_ascii_lowercase(),
        "binary UUID uses canonical text"
    );
    assert_eq!(
        format!("session_{raw}").parse::<SessionId>().expect("parse full UUID"),
        id,
        "UUID round trip"
    );
    for raw in [
        "invalid",
        "550e8400e29b41d4a716446655440000",
        "urn:uuid:550e8400-e29b-41d4-a716-446655440000",
        "550e8400-e29b-41d4-a716-44665544000z",
    ] {
        assert_eq!(
            SessionId::try_from_raw(raw),
            Err(ParseError::InvalidValue),
            "invalid UUID representation"
        );
    }
}

#[test]
fn uuid_generators_preserve_versions_and_pass_domain_validation() {
    let random = SessionId::generate::<Uuid4>().expect("generate UUID4");
    let time = SessionId::generate::<Uuid7>().expect("generate UUID7");
    assert_eq!(random.payload().get_version_num(), 4, "UUID4 version");
    assert_eq!(time.payload().get_version_num(), 7, "UUID7 version");
    assert_ne!(
        random,
        SessionId::generate::<Uuid4>().expect("generate another UUID4"),
        "UUID4 randomness"
    );
}
