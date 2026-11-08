#![cfg(feature = "macros")]

use trogon_object_id::{ObjectId, ParseError, Payload};

#[derive(Debug)]
struct Custom(bool);
impl core::fmt::Display for Custom {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result { f.write_str("allowed") }
}
impl Payload for Custom {
    type Raw = Self;
    fn parse(value: &str) -> Result<Self, ParseError> {
        if value == "allowed" { Ok(Self(true)) } else { Err(ParseError::InvalidValue) }
    }
    fn validate(&self) -> Result<(), ParseError> {
        if self.0 { Ok(()) } else { Err(ParseError::InvalidValue) }
    }
    fn as_raw(&self) -> &Self { self }
}
#[derive(Debug, ObjectId)]
#[object_id(prefix = "custom", fallible)]
#[repr(transparent)]
struct CustomId(Custom);

#[test]
fn custom_standard_conversions_cannot_bypass_validation() {
    assert!(matches!(CustomId::try_from(Custom(false)), Err(ParseError::InvalidValue)));
    let id = CustomId::try_from(Custom(true)).expect("checked custom payload");
    assert_eq!(id.to_string(), "custom_allowed", "domain remains attached");
    let generic = trogon_object_id::ObjectId::<CustomId>::try_from(trogon_object_id::RawPayload(Custom(true)))
        .expect("generic checked custom payload");
    assert_eq!(generic.to_string(), "custom_allowed", "generic conversion agrees");
    assert!(trogon_object_id::ObjectId::<CustomId>::try_from(trogon_object_id::RawPayload(Custom(false))).is_err());
}

#[cfg(feature = "alloc")]
#[test]
fn validated_text_converts_infallibly_and_full_strings_stay_unambiguous() {
    use trogon_object_id::{IntegerText, Text};
    #[derive(ObjectId)]
    #[object_id(prefix = "text")]
    #[repr(transparent)]
    struct TextId(Text);
    #[derive(ObjectId)]
    #[object_id(prefix = "integer")]
    #[repr(transparent)]
    struct IntegerId(IntegerText);
    let value = String::from("text_already-prefixed");
    let pointer = value.as_ptr();
    let payload = Text::try_from(value).expect("validated text");
    let id = TextId::from(payload);
    assert_eq!(id.to_string(), "text_text_already-prefixed", "typed payload is never guessed to be full form");
    assert_eq!(id.as_raw().as_ptr(), pointer, "typed conversion reuses storage");
    let id = TextId::try_from(String::from("text_already-prefixed")).expect("full form");
    assert_eq!(id.as_raw(), "already-prefixed", "strings always mean full form");
    assert!(TextId::try_from("raw").is_err(), "borrowed strings require a prefix");
    let integer = IntegerId::from(IntegerText::try_from("+00042").expect("validated integer"));
    assert_eq!(integer.as_raw(), "+00042", "spelling is unchanged");
    let generic: trogon_object_id::ObjectId<IntegerId> = IntegerText::try_from("+00042").expect("integer").into();
    assert_eq!(generic.as_raw(), "+00042", "generic infallible conversion");
}

#[cfg(feature = "uuid")]
#[test]
fn binary_payload_converts_infallibly() {
    #[derive(ObjectId)]
    #[object_id(prefix = "uuid")]
    #[repr(transparent)]
    struct UuidId(trogon_object_id::Uuid);
    let uuid = trogon_object_id::Uuid::nil();
    let id = UuidId::from(uuid);
    let generic = trogon_object_id::ObjectId::<UuidId>::from(uuid);
    assert_eq!(id.as_raw(), generic.as_raw(), "compact conversions agree");
}
