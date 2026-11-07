#![cfg(feature = "macros")]

use trogon_object_id::{ObjectId, Text};

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "user")]
#[repr(transparent)]
struct UserId(Text);

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "service")]
#[repr(transparent)]
struct ServiceId(Text);

#[derive(Debug, Clone, PartialEq, Eq, ObjectId)]
enum PrincipalId {
    User(UserId),
    Service(ServiceId),
}

#[derive(Debug, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "invoice", separator = ":")]
#[repr(transparent)]
struct InvoiceId(Text<trogon_object_id::validators::Integer>);

#[derive(Debug, PartialEq, Eq, ObjectId)]
enum ConditionalId {
    #[cfg(any())]
    Disabled(MissingDomain),
    #[cfg_attr(all(), cfg(any()))]
    Conditional(MissingOtherDomain),
    #[cfg_attr(any(), cfg(any()))]
    User(UserId),
    Invoice(InvoiceId),
}

#[test]
fn derived_identifier_uses_its_declared_prefix() {
    let user = UserId::try_from_raw("abc-123").expect("valid suffix");
    assert_eq!(
        user.to_string(),
        "user_abc-123",
        "the declaration defines the domain prefix"
    );
}

#[test]
fn derived_enum_parses_and_formats_normal_enum_variants() {
    let principal = "user_abc-123".parse::<PrincipalId>().expect("known domain prefix");
    assert!(
        matches!(principal, PrincipalId::User(_)),
        "prefix selects the typed variant"
    );
    assert_eq!(
        principal.to_string(),
        "user_abc-123",
        "formatted enum retains its discriminator"
    );
}

#[test]
fn owned_enum_input_reuses_its_text_payload_allocation() {
    let input = String::from("user_owned-value");
    let pointer = input.as_ptr();
    let principal = PrincipalId::try_from(input).expect("owned full enum identifier");
    let PrincipalId::User(user) = principal else {
        panic!("expected user variant")
    };
    assert_eq!(
        user.as_raw().as_ptr(),
        pointer,
        "enum parsing consumes the existing buffer"
    );
}

#[test]
fn native_conditional_variants_preserve_dispatch_and_validation() {
    let invoice = "invoice:+00042".parse::<ConditionalId>().expect("custom typed payload");
    assert!(
        matches!(invoice, ConditionalId::Invoice(ref id) if id.as_raw() == "+00042"),
        "payload preserves integer spelling"
    );
    assert!(
        matches!("user_abc".parse::<ConditionalId>(), Ok(ConditionalId::User(_))),
        "enabled variant remains available"
    );
    assert_eq!(
        "invoice:bad".parse::<ConditionalId>(),
        Err(trogon_object_id::ParseError::InvalidValue),
        "enum keeps member validation errors"
    );
    assert_eq!(
        "unknown_abc".parse::<ConditionalId>(),
        Err(trogon_object_id::ParseError::UnknownPrefix),
        "unknown domain is rejected"
    );
}

#[cfg(feature = "uuid")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "session")]
#[repr(transparent)]
struct SessionId(trogon_object_id::Uuid);

#[cfg(feature = "uuid")]
#[test]
fn derived_identifiers_preserve_compact_copy_payloads() {
    fn copy<T: Copy>(value: T) -> (T, T) {
        (value, value)
    }
    let session = SessionId::generate::<trogon_object_id::Uuid7>().expect("UUID7 generation");
    let (first, second) = copy(session);
    assert_eq!(first, second, "direct identifiers preserve Copy payloads");
    assert_eq!(
        std::mem::size_of::<SessionId>(),
        std::mem::size_of::<trogon_object_id::Uuid>(),
        "payload layout remains transparent"
    );
}

#[cfg(feature = "serde")]
#[test]
fn derived_enum_serde_preserves_full_prefix_and_validation() {
    let principal = "user_abc".parse::<PrincipalId>().expect("known prefix");
    let json = serde_json::to_string(&principal).expect("serialize derived enum");
    assert_eq!(json, "\"user_abc\"", "wire encoding keeps the discriminator");
    assert_eq!(
        serde_json::from_str::<PrincipalId>(&json).expect("decode derived enum"),
        principal,
        "derived serde round trip"
    );
    assert!(
        serde_json::from_str::<PrincipalId>("\"user_\"").is_err(),
        "serde cannot construct an empty member"
    );
}
