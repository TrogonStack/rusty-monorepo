#![cfg(feature = "macros")]

#[cfg(feature = "serde")]
use serde::de::value::{Error, StringDeserializer};
#[cfg(feature = "serde")]
use serde::Deserialize;
use std::collections::HashSet;
use trogon_object_id::{IntegerText, ObjectId, ParseError, Payload, Text};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ObjectId)]
#[object_id(prefix = "user")]
#[repr(transparent)]
pub struct UserId(Text);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ObjectId)]
#[object_id(prefix = "service")]
#[repr(transparent)]
pub struct ServiceId(Text);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ObjectId)]
#[object_id(prefix = "invoice", separator = ":")]
#[repr(transparent)]
pub struct InvoiceId(IntegerText);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ObjectId)]
#[object_id(prefix = "app")]
#[repr(transparent)]
pub struct AppId(Text);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ObjectId)]
#[object_id(prefix = "apple")]
#[repr(transparent)]
pub struct AppleId(Text);

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
pub enum PrincipalId {
    User(UserId),
    Service(ServiceId),
    Invoice(InvoiceId),
}

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
pub enum AdjacentId {
    App(AppId),
    Apple(AppleId),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct AsciiToken(Text);

impl TryFrom<Text> for AsciiToken {
    type Error = ParseError;

    fn try_from(value: Text) -> Result<Self, Self::Error> {
        if value.as_str().bytes().all(|byte| byte.is_ascii_lowercase()) {
            Ok(Self(value))
        } else {
            Err(ParseError::InvalidValue)
        }
    }
}

impl TryFrom<&str> for AsciiToken {
    type Error = ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Text::try_from(value)?.try_into()
    }
}

impl TryFrom<String> for AsciiToken {
    type Error = ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Text::try_from(value)?.try_into()
    }
}

impl From<AsciiToken> for Text {
    fn from(value: AsciiToken) -> Self {
        value.0
    }
}

impl std::fmt::Display for AsciiToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Payload for AsciiToken {
    type Raw = str;

    fn parse(value: &str) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    fn validate(&self) -> Result<(), ParseError> {
        if self.0.as_str().bytes().all(|byte| byte.is_ascii_lowercase()) {
            Ok(())
        } else {
            Err(ParseError::InvalidValue)
        }
    }

    fn as_raw(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, ObjectId)]
#[object_id(prefix = "token")]
#[repr(transparent)]
pub struct TokenId(AsciiToken);

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
        "custom conversion accepts domain value"
    );
    assert_eq!(
        "token_ABC".parse::<TokenId>(),
        Err(ParseError::InvalidValue),
        "custom conversion rejects parsed value"
    );
}

#[test]
fn custom_try_from_rules_cover_raw_full_typed_and_generated_inputs() {
    let owned = String::from("token_abc");
    let pointer = owned.as_ptr();
    let token = TokenId::try_from(owned).expect("owned custom conversion");
    assert_eq!(
        token.as_raw().as_ptr(),
        pointer,
        "custom conversion keeps owned storage"
    );
    let text = Text::from(token.into_payload());
    assert_eq!(text.as_str().as_ptr(), pointer, "custom widening keeps owned storage");

    for (raw, expected) in [
        (String::new(), ParseError::EmptyValue),
        ("a".repeat(1025), ParseError::ValueTooLong),
        (String::from("a\n"), ParseError::ControlCharacter),
        (String::from("ABC"), ParseError::InvalidValue),
    ] {
        assert_eq!(
            AsciiToken::try_from(raw.as_str()),
            Err(expected),
            "borrowed payload conversion"
        );
        assert_eq!(
            AsciiToken::try_from(raw.clone()),
            Err(expected),
            "owned payload conversion"
        );
        assert_eq!(
            TokenId::try_from_raw(&raw),
            Err(expected),
            "raw borrowed ID construction"
        );
        assert_eq!(
            TokenId::try_from_owned(raw.clone()),
            Err(expected),
            "raw owned ID construction"
        );
        let full = format!("token_{raw}");
        assert_eq!(
            TokenId::try_from(full.as_str()),
            Err(expected),
            "full borrowed ID conversion"
        );
        assert_eq!(TokenId::try_from(full), Err(expected), "full owned ID conversion");
    }

    struct RejectedToken;
    impl trogon_object_id::Generator for RejectedToken {
        type Payload = AsciiToken;

        fn generate() -> Result<Self::Payload, ParseError> {
            Ok(AsciiToken(Text::try_from("ABC").expect("valid general text")))
        }
    }
    assert_eq!(
        TokenId::try_from_payload(AsciiToken(Text::try_from("ABC").expect("valid general text"))),
        Err(ParseError::InvalidValue),
        "typed values must still satisfy the payload rule"
    );
    assert_eq!(
        TokenId::generate::<RejectedToken>(),
        Err(ParseError::InvalidValue),
        "generation cannot bypass the payload rule"
    );
}

#[cfg(feature = "serde")]
#[test]
fn custom_try_from_rules_apply_to_full_and_raw_serde() {
    let full = String::from("token_abc");
    let pointer = full.as_ptr();
    let token = TokenId::deserialize(StringDeserializer::<Error>::new(full)).expect("owned custom serde");
    assert_eq!(token.as_raw().as_ptr(), pointer, "full serde reuses custom storage");
    let raw = String::from("abc");
    let pointer = raw.as_ptr();
    let token: TokenId =
        trogon_object_id::serde_raw::deserialize(StringDeserializer::<Error>::new(raw)).expect("raw custom serde");
    assert_eq!(token.as_raw().as_ptr(), pointer, "raw serde reuses custom storage");
    assert!(
        TokenId::deserialize(StringDeserializer::<Error>::new(String::from("token_ABC"))).is_err(),
        "full serde rejects invalid custom syntax"
    );
    assert!(
        trogon_object_id::serde_raw::deserialize::<TokenId, _>(StringDeserializer::<Error>::new(String::from("ABC")))
            .is_err(),
        "raw serde rejects invalid custom syntax"
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
fn enum_dispatches_without_losing_the_discriminator() {
    let user = UserId::try_from_raw("123").expect("valid user");
    let principal = PrincipalId::from(user.clone());
    assert_eq!(principal.to_string(), "user_123", "enum storage retains prefix");
    assert_eq!(
        "user_123".parse::<PrincipalId>().expect("parse enum"),
        principal,
        "enum round trip"
    );
    assert!(
        matches!(principal, PrincipalId::User(id) if id == user),
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
        "member validation survives enum dispatch"
    );
    assert_eq!(
        "user_".parse::<PrincipalId>(),
        Err(ParseError::EmptyValue),
        "empty known member is invalid"
    );
}

#[test]
fn enforces_utf8_byte_limits_without_losing_boundary_values() {
    let boundary = "🦀".repeat(256);
    let id = UserId::try_from_raw(&boundary).expect("exactly 1024 UTF-8 bytes");
    assert_eq!(id.as_raw(), boundary, "valid boundary is preserved");
    assert_eq!(
        UserId::try_from_raw(&"🦀".repeat(257)),
        Err(ParseError::ValueTooLong),
        "limit counts bytes rather than characters"
    );
}

#[cfg(any())]
#[derive(ObjectId)]
#[object_id(prefix = "")]
#[repr(transparent)]
struct DisabledId(MissingPayload);

#[cfg_attr(all(), cfg(any()), doc = "disabled")]
#[derive(ObjectId)]
#[object_id(prefix = "")]
#[repr(transparent)]
struct ConditionalId(Text);

#[cfg_attr(any(), cfg(any()))]
#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "enabled")]
#[repr(transparent)]
struct EnabledId(Text);

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
enum ConditionalPrincipalId {
    #[cfg(any())]
    Missing(MissingId),
    #[cfg_attr(all(), cfg(any()))]
    AlsoMissing(AlsoMissingId),
    User(UserId),
    #[cfg(feature = "macros")]
    Service(ServiceId),
    #[cfg_attr(feature = "macros", cfg_attr(feature = "macros", cfg(feature = "macros")))]
    Enabled(EnabledId),
}

#[test]
fn native_conditional_attributes_cover_complete_declarations_and_variants() {
    assert!(
        matches!(
            "user_123".parse::<ConditionalPrincipalId>(),
            Ok(ConditionalPrincipalId::User(_))
        ),
        "an enabled variant remains parsable"
    );
    assert!(
        matches!(
            "service_123".parse::<ConditionalPrincipalId>(),
            Ok(ConditionalPrincipalId::Service(_))
        ),
        "a cfg-enabled variant remains parsable"
    );
    let enabled = EnabledId::try_from_raw("123").expect("active conditional domain");
    assert_eq!(
        ConditionalPrincipalId::from(enabled).to_string(),
        "enabled_123",
        "active cfg_attr branches retain their implementations"
    );
    assert_eq!(
        "missing_123".parse::<ConditionalPrincipalId>(),
        Err(ParseError::UnknownPrefix),
        "inactive variants do not become recognized domains"
    );
}
