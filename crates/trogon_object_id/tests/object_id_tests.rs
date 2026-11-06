use std::collections::HashSet;
use trogon_object_id::{object_id, union_object_id, validators::Integer, ParseError, Validator};

object_id! { pub struct UserId("user"); }
object_id! { pub struct ServiceId("service"); }
object_id! { pub struct InvoiceId("invoice", separator = ":", validator = Integer); }
object_id! { pub struct AppId("app"); }
object_id! { pub struct AppleId("apple"); }
union_object_id! { pub enum PrincipalId { User(UserId), Service(ServiceId), Invoice(InvoiceId) } }
union_object_id! { pub enum AdjacentId { App(AppId), Apple(AppleId) } }

pub struct AsciiToken;
impl Validator for AsciiToken {
    fn validate(value: &str) -> Result<(), ParseError> {
        if value.bytes().all(|byte| byte.is_ascii_lowercase()) {
            Ok(())
        } else {
            Err(ParseError::InvalidValue)
        }
    }
}
object_id! { pub struct TokenId("token", validator = AsciiToken); }

#[test]
fn accepts_a_nonempty_raw_identifier() {
    let id = UserId::try_from_raw("abc-123").expect("valid raw identifier");
    assert_eq!(id.as_raw(), "abc-123", "raw suffix is preserved");
}

#[test]
fn rejects_empty_control_and_oversized_values() {
    for value in ["", "abc\nxyz", "abc\0xyz", "abc\u{0085}xyz", &"x".repeat(1025)] {
        assert!(UserId::try_from_raw(value).is_err(), "invalid suffix must be rejected");
    }
}

#[test]
fn preserves_full_prefix_raw_unicode_and_separator_characters() {
    for raw in ["abc-123", "hello_world:again", "日本語", "🦀", " abc "] {
        let id = UserId::try_from_raw(raw).expect("valid raw identifier");
        let full = format!("user_{raw}");
        assert_eq!(id.to_string(), full, "display contains full prefix");
        assert_eq!(full.parse::<UserId>().expect("parse full identifier"), id, "round trip");
        assert_eq!(
            UserId::try_from(full.as_str()).expect("TryFrom full identifier"),
            id,
            "TryFrom round trip"
        );
        assert_eq!(
            UserId::try_from_raw(id.as_raw()).expect("raw storage round trip"),
            id,
            "raw storage round trip"
        );
    }
    assert_eq!(
        "invoice:+00042".parse::<InvoiceId>().expect("signed integer").as_raw(),
        "+00042",
        "integer text preserved"
    );
}

#[test]
fn errors_distinguish_failures_without_reflecting_input() {
    for value in ["", "wrong_abc", "user", "User_abc", "🦀_abc", "abc"] {
        assert_eq!(value.parse::<UserId>(), Err(ParseError::InvalidPrefix), "wrong prefix");
    }
    assert_eq!("user_".parse::<UserId>(), Err(ParseError::EmptyValue), "missing suffix");
    assert_eq!(
        "user_a\n".parse::<UserId>(),
        Err(ParseError::ControlCharacter),
        "control suffix"
    );
    assert_eq!(
        format!("user_{}", "x".repeat(1025)).parse::<UserId>(),
        Err(ParseError::ValueTooLong),
        "bounded suffix"
    );
    assert!(
        !ParseError::InvalidValue.to_string().contains("attacker-input"),
        "errors carry no raw input"
    );
}

#[test]
fn applies_integer_and_custom_validation_on_every_input_path() {
    for raw in ["1", "-42", "+00042", "0", &"9".repeat(1024)] {
        assert!(InvoiceId::try_from_raw(raw).is_ok(), "valid decimal syntax");
    }
    for raw in ["+", "-", "1x", "1.0", " 1", "１２", "١"] {
        assert_eq!(
            InvoiceId::try_from_raw(raw),
            Err(ParseError::InvalidValue),
            "invalid decimal syntax"
        );
        assert_eq!(
            format!("invoice:{raw}").parse::<InvoiceId>(),
            Err(ParseError::InvalidValue),
            "parse validates decimal syntax"
        );
    }
    assert!(
        TokenId::try_from_raw("abc").is_ok(),
        "custom validator accepts domain value"
    );
    assert_eq!(
        "token_ABC".parse::<TokenId>(),
        Err(ParseError::InvalidValue),
        "custom validator rejects parsed value"
    );
}

#[test]
fn value_traits_work_without_runtime_registries() {
    let a = UserId::try_from_raw("a").expect("valid suffix");
    let b = UserId::try_from_raw("b").expect("valid suffix");
    assert!(a < b, "ordering follows raw value within a domain");
    let set = HashSet::from([a.clone(), a, b]);
    assert_eq!(set.len(), 2, "hash and equality agree");
}

#[test]
fn union_dispatches_without_losing_the_discriminator() {
    let user = UserId::try_from_raw("123").expect("valid user");
    let union = PrincipalId::from(user.clone());
    assert_eq!(union.to_string(), "user_123", "union storage retains prefix");
    assert_eq!(
        "user_123".parse::<PrincipalId>().expect("parse union"),
        union,
        "union round trip"
    );
    assert!(
        matches!(union, PrincipalId::User(id) if id == user),
        "exhaustive enum retains domain"
    );
    assert!(
        matches!("apple_123".parse::<AdjacentId>(), Ok(AdjacentId::Apple(_))),
        "full prefixes distinguish adjacent names"
    );
    assert_eq!(
        "unknown_123".parse::<PrincipalId>(),
        Err(ParseError::UnknownPrefix),
        "unknown discriminator"
    );
    assert_eq!(
        "invoice:bad".parse::<PrincipalId>(),
        Err(ParseError::InvalidValue),
        "member validation survives union dispatch"
    );
    assert_eq!(
        "user_".parse::<PrincipalId>(),
        Err(ParseError::EmptyValue),
        "empty known member is invalid"
    );
}
