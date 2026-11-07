#![cfg(all(feature = "serde", feature = "macros"))]

use trogon_object_id::{validators::Integer, ObjectId, ObjectIdDomain, ParseError, Text};

#[derive(ObjectIdDomain)]
#[object_id(prefix = "user")]
struct UserDomain;
type UserId = ObjectId<UserDomain>;

#[derive(ObjectIdDomain)]
#[object_id(prefix = "invoice", payload = Text<Integer>)]
struct InvoiceDomain;
type InvoiceId = ObjectId<InvoiceDomain>;

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
enum PrincipalId {
    User(UserId),
    Invoice(InvoiceId),
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct Record {
    #[serde(with = "trogon_object_id::serde_raw")]
    invoice: InvoiceId,
}

#[test]
fn serde_round_trips_full_identifier_and_enum_forms() {
    let id = UserId::try_from_raw("abc_🦀").expect("valid user");
    let json = serde_json::to_string(&id).expect("serialize identifier");
    assert_eq!(json, "\"user_abc_🦀\"", "canonical full JSON form");
    assert_eq!(
        serde_json::from_str::<UserId>(&json).expect("deserialize identifier"),
        id,
        "full JSON round trip"
    );
    let principal = PrincipalId::from(id);
    let encoded = serde_json::to_string(&principal).expect("serialize enum");
    assert_eq!(
        serde_json::from_str::<PrincipalId>(&encoded).expect("deserialize enum"),
        principal,
        "enum JSON retains discriminator"
    );
}

#[test]
fn serde_never_bypasses_the_domain_parser() {
    for json in [
        "\"\"",
        "\"abc\"",
        "\"service_abc\"",
        "\"user_\"",
        "\"user_a\\n\"",
        "null",
        "42",
        "{}",
        "[]",
    ] {
        assert!(
            serde_json::from_str::<UserId>(json).is_err(),
            "invalid full ID is rejected"
        );
    }
    for value in ["invoice_bad", "invoice_", "unknown_123"] {
        let json = serde_json::to_string(value).expect("encode input fixture");
        assert!(
            serde_json::from_str::<PrincipalId>(&json).is_err(),
            "enum preserves validation"
        );
    }
    let too_long = serde_json::to_string(&format!("user_{}", "x".repeat(1025))).expect("encode oversized fixture");
    assert!(
        serde_json::from_str::<UserId>(&too_long).is_err(),
        "JSON honors byte limit"
    );
    let error = serde_json::from_str::<InvoiceId>("\"invoice_attacker-input\"").expect_err("bad integer");
    assert!(
        !error.to_string().contains("attacker-input"),
        "our errors do not expose raw values"
    );
    assert_eq!(
        "invoice_bad".parse::<InvoiceId>(),
        Err(ParseError::InvalidValue),
        "same domain validator"
    );
}

#[test]
fn raw_field_adapter_is_explicit_and_validated() {
    let record = Record {
        invoice: InvoiceId::try_from_raw("+00042").expect("valid integer"),
    };
    let json = serde_json::to_string(&record).expect("encode raw field");
    assert_eq!(json, "{\"invoice\":\"+00042\"}", "only annotated field drops prefix");
    assert_eq!(
        serde_json::from_str::<Record>(&json).expect("decode raw field"),
        record,
        "raw field round trip"
    );
    for json in [
        "{\"invoice\":\"bad\"}",
        "{\"invoice\":\"invoice_42\"}",
        "{\"invoice\":\"\"}",
    ] {
        assert!(
            serde_json::from_str::<Record>(json).is_err(),
            "raw adapter validates suffix"
        );
    }
    assert!(
        serde_json::from_str::<InvoiceId>("\"42\"").is_err(),
        "default decoder never accepts raw form"
    );
}
