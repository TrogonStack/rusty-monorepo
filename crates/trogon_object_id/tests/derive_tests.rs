#![cfg(feature = "derive")]

use trogon_object_id::{ObjectId, ObjectIdDomain, ObjectIdUnion};

#[derive(ObjectIdDomain)]
#[object_id(prefix = "user")]
struct UserDomain;
type UserId = ObjectId<UserDomain>;

#[derive(ObjectIdDomain)]
#[object_id(prefix = "service")]
struct ServiceDomain;
type ServiceId = ObjectId<ServiceDomain>;

#[derive(Debug, Clone, PartialEq, Eq, ObjectIdUnion)]
enum PrincipalId {
    User(UserId),
    Service(ServiceId),
}

#[derive(ObjectIdDomain)]
#[object_id(prefix = "invoice", separator = ":", payload = trogon_object_id::Text<trogon_object_id::validators::Integer>)]
struct InvoiceDomain;
type InvoiceId = ObjectId<InvoiceDomain>;

#[derive(Debug, PartialEq, Eq, ObjectIdUnion)]
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
fn derived_domain_uses_its_declared_prefix() {
    let user = UserId::try_from_raw("abc-123").expect("valid suffix");
    assert_eq!(
        user.to_string(),
        "user_abc-123",
        "the declaration defines the domain prefix"
    );
}

#[test]
fn derived_union_parses_and_formats_normal_enum_variants() {
    let principal = "user_abc-123".parse::<PrincipalId>().expect("known domain prefix");
    assert!(
        matches!(principal, PrincipalId::User(_)),
        "prefix selects the typed variant"
    );
    assert_eq!(
        principal.to_string(),
        "user_abc-123",
        "formatted union retains its discriminator"
    );
}

#[test]
fn owned_union_input_reuses_its_text_payload_allocation() {
    let input = String::from("user_owned-value");
    let pointer = input.as_ptr();
    let principal = PrincipalId::try_from(input).expect("owned full union identifier");
    let PrincipalId::User(user) = principal else {
        panic!("expected user variant")
    };
    assert_eq!(
        user.as_str().as_ptr(),
        pointer,
        "union parsing consumes the existing buffer"
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
        "union keeps member validation errors"
    );
    assert_eq!(
        "unknown_abc".parse::<ConditionalId>(),
        Err(trogon_object_id::ParseError::UnknownPrefix),
        "unknown domain is rejected"
    );
}

#[cfg(feature = "uuid")]
#[derive(ObjectIdDomain)]
#[object_id(prefix = "session", payload = trogon_object_id::Uuid)]
struct SessionDomain;

#[cfg(feature = "uuid")]
#[test]
fn derived_domains_preserve_compact_copy_payloads() {
    type SessionId = ObjectId<SessionDomain>;
    fn copy<T: Copy>(value: T) -> (T, T) {
        (value, value)
    }
    let session = SessionId::generate::<trogon_object_id::Uuid7>().expect("UUID7 generation");
    let (first, second) = copy(session);
    assert_eq!(first, second, "domain derive adds no marker trait requirements");
    assert_eq!(
        std::mem::size_of::<SessionId>(),
        std::mem::size_of::<trogon_object_id::Uuid>(),
        "payload layout remains transparent"
    );
}

#[cfg(feature = "serde")]
#[test]
fn derived_union_serde_preserves_full_prefix_and_validation() {
    let principal = "user_abc".parse::<PrincipalId>().expect("known prefix");
    let json = serde_json::to_string(&principal).expect("serialize derived union");
    assert_eq!(json, "\"user_abc\"", "wire encoding keeps the discriminator");
    assert_eq!(
        serde_json::from_str::<PrincipalId>(&json).expect("decode derived union"),
        principal,
        "derived serde round trip"
    );
    assert!(
        serde_json::from_str::<PrincipalId>("\"user_\"").is_err(),
        "serde cannot construct an empty member"
    );
}
