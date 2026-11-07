use trogon_object_id::{Definition, Domain, ObjectId};

struct TextDomain;
impl Domain for TextDomain {
    const DEFINITION: Definition = Definition::new("text", "_");
    type Payload = trogon_object_id::Text;
}

#[test]
fn owned_text_construction_reuses_the_callers_buffer() {
    let raw = String::from("owned-text-value");
    let original = raw.as_ptr();
    let id = ObjectId::<TextDomain>::try_from_owned(raw).expect("valid text");
    assert_eq!(id.as_raw().as_ptr(), original, "owned storage should not be copied");
}

#[cfg(feature = "uuid")]
struct SessionDomain;
#[cfg(feature = "uuid")]
impl Domain for SessionDomain {
    const DEFINITION: Definition = Definition::new("session", "_");
    type Payload = trogon_object_id::Uuid;
}

#[cfg(feature = "uuid")]
#[test]
fn uuid_domain_uses_only_its_binary_payload() {
    assert_eq!(
        std::mem::size_of::<ObjectId<SessionDomain>>(),
        std::mem::size_of::<uuid::Uuid>(),
        "UUID identifiers should not carry a heap string"
    );
}

#[test]
fn value_traits_do_not_require_traits_on_the_domain_marker() {
    let first = ObjectId::<TextDomain>::try_from_raw("a").expect("valid text");
    let second = ObjectId::<TextDomain>::try_from_raw("b").expect("valid text");
    assert!(first < second, "payload ordering remains available");
    let values = std::collections::HashSet::from([first.clone(), first, second]);
    assert_eq!(values.len(), 2, "marker requires no Hash or Eq derive");
    assert!(!format!("{values:?}").is_empty(), "marker requires no Debug derive");
}

#[test]
fn owned_full_text_reuses_its_buffer_after_checked_prefix_removal() {
    let raw = String::from("text_owned-value");
    let pointer = raw.as_ptr();
    let id = ObjectId::<TextDomain>::try_from(raw).expect("valid full text");
    assert_eq!(id.as_raw(), "owned-value", "full prefix was removed");
    assert_eq!(id.as_raw().as_ptr(), pointer, "prefix removal reused the buffer");
    for invalid in ["wrong_value", "text_", "text_a\n"] {
        assert!(
            ObjectId::<TextDomain>::try_from(invalid.to_owned()).is_err(),
            "owned full input is validated"
        );
    }
}

#[cfg(feature = "uuid")]
#[test]
fn binary_ids_are_copy_without_traits_on_the_domain_marker() {
    fn copy<T: Copy>(value: T) -> (T, T) {
        (value, value)
    }
    let raw = trogon_object_id::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").expect("UUID fixture");
    let id = ObjectId::<SessionDomain>::try_from_payload(raw).expect("typed UUID");
    let (first, second) = copy(id);
    assert_eq!(first, second, "copy preserves the typed identifier");
    assert_eq!(first.into_payload(), raw, "typed payload is recoverable");
}

#[cfg(feature = "serde")]
#[test]
fn generic_and_raw_serde_decoders_reuse_owned_text() {
    use serde::de::value::{Error, StringDeserializer};
    use serde::Deserialize;
    let full = String::from("text_owned-value");
    let pointer = full.as_ptr();
    let id = ObjectId::<TextDomain>::deserialize(StringDeserializer::<Error>::new(full)).expect("owned serde text");
    assert_eq!(id.as_raw().as_ptr(), pointer, "generic serde consumes owned strings");
    let raw = String::from("owned-value");
    let pointer = raw.as_ptr();
    let id: ObjectId<TextDomain> =
        trogon_object_id::serde_raw::deserialize(StringDeserializer::<Error>::new(raw)).expect("owned raw serde text");
    assert_eq!(id.as_raw().as_ptr(), pointer, "raw serde consumes owned strings");
}

struct CheckedPayload(bool);
impl std::fmt::Display for CheckedPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.0 { "allowed" } else { "rejected" })
    }
}
impl TryFrom<&str> for CheckedPayload {
    type Error = trogon_object_id::ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value == "allowed" {
            Ok(Self(true))
        } else {
            Err(trogon_object_id::ParseError::InvalidValue)
        }
    }
}
impl trogon_object_id::Payload for CheckedPayload {
    type Raw = str;
    fn parse(value: &str) -> Result<Self, trogon_object_id::ParseError> {
        Self::try_from(value)
    }
    fn validate(&self) -> Result<(), trogon_object_id::ParseError> {
        if self.0 {
            Ok(())
        } else {
            Err(trogon_object_id::ParseError::InvalidValue)
        }
    }
    fn as_raw(&self) -> &str {
        if self.0 {
            "allowed"
        } else {
            "rejected"
        }
    }
}
struct CheckedDomain;
impl Domain for CheckedDomain {
    const DEFINITION: Definition = Definition::new("checked", "_");
    type Payload = CheckedPayload;
}
struct RejectedGenerator;
impl trogon_object_id::Generator for RejectedGenerator {
    type Payload = CheckedPayload;
    fn generate() -> Result<Self::Payload, trogon_object_id::ParseError> {
        Ok(CheckedPayload(false))
    }
}

#[test]
fn custom_typed_inputs_and_generators_cannot_skip_payload_validation() {
    assert!(
        matches!(
            ObjectId::<CheckedDomain>::try_from_payload(CheckedPayload(false)),
            Err(trogon_object_id::ParseError::InvalidValue)
        ),
        "typed input invokes validation"
    );
    assert!(
        matches!(
            ObjectId::<CheckedDomain>::generate::<RejectedGenerator>(),
            Err(trogon_object_id::ParseError::InvalidValue)
        ),
        "generation invokes validation"
    );
}

struct CountingText(trogon_object_id::Text);
static VALIDATION_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
impl TryFrom<&str> for CountingText {
    type Error = trogon_object_id::ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        VALIDATION_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        trogon_object_id::Text::try_from(value).map(Self)
    }
}
impl std::fmt::Display for CountingText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}
impl trogon_object_id::Payload for CountingText {
    type Raw = str;
    fn parse(value: &str) -> Result<Self, trogon_object_id::ParseError> {
        Self::try_from(value)
    }
    fn validate(&self) -> Result<(), trogon_object_id::ParseError> {
        Ok(())
    }
    fn as_raw(&self) -> &str {
        self.0.as_str()
    }
}
struct CountingDomain;
impl Domain for CountingDomain {
    const DEFINITION: Definition = Definition::new("counted", "_");
    type Payload = CountingText;
}
struct CountingGenerator;
impl trogon_object_id::Generator for CountingGenerator {
    type Payload = CountingText;
    fn generate() -> Result<Self::Payload, trogon_object_id::ParseError> {
        CountingText::try_from("generated")
    }
}

#[test]
fn immutable_text_guarantees_are_established_once() {
    use std::sync::atomic::Ordering;
    VALIDATION_CALLS.store(0, Ordering::Relaxed);
    let payload = CountingText::try_from("valid").expect("validated text");
    let id = ObjectId::<CountingDomain>::try_from_payload(payload).expect("wrap validated text");
    assert_eq!(id.as_raw(), "valid", "typed wrapping preserves payload");
    assert_eq!(
        VALIDATION_CALLS.load(Ordering::Relaxed),
        1,
        "wrapping immutable text should not repeat validation"
    );
    VALIDATION_CALLS.store(0, Ordering::Relaxed);
    let id = ObjectId::<CountingDomain>::generate::<CountingGenerator>().expect("generate validated text");
    assert_eq!(id.as_raw(), "generated", "typed generation preserves payload");
    assert_eq!(
        VALIDATION_CALLS.load(Ordering::Relaxed),
        1,
        "typed generation should not repeat validation"
    );
}
