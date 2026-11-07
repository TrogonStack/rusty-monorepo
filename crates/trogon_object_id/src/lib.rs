#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

use std::fmt;

extern crate self as trogon_object_id;

/// Derive checked identifier behavior for a transparent payload newtype or an enum of IDs.
///
/// ```
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// #[repr(transparent)]
/// struct ItemId(Text);
/// let id = ItemId::try_from_raw("abc")?;
/// assert_eq!(id.to_string(), "item_abc");
/// # Ok::<(), trogon_object_id::ParseError>(())
/// ```
///
/// ```compile_fail
/// use trogon_object_id::ObjectId;
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// #[repr(transparent)]
/// struct Empty;
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// #[repr(transparent)]
/// struct Named { payload: Text }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// #[repr(transparent)]
/// struct Pair(Text, Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// #[repr(transparent)]
/// struct PublicPayload(pub Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// struct MissingLayout(Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item", payload = Text)]
/// #[repr(transparent)]
/// struct RepeatedPayload(Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// #[repr(transparent)]
/// struct Generic<T>(T);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "")]
/// #[repr(transparent)]
/// struct InvalidPrefix(Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "item")]
/// #[repr(transparent)]
/// struct ItemId(Text);
/// #[derive(ObjectId)]
/// #[object_id(prefix = "order")]
/// #[repr(transparent)]
/// struct OrderId(Text);
/// let _: ItemId = OrderId::try_from_raw("abc").unwrap();
/// ```
///
/// ```compile_fail
/// mod domain {
///     use trogon_object_id::{ObjectId, Text};
///     #[derive(ObjectId)]
///     #[object_id(prefix = "item")]
///     #[repr(transparent)]
///     pub struct ItemId(Text);
/// }
/// let forged = domain::ItemId(trogon_object_id::Text::try_from("abc").unwrap());
/// ```
///
/// Enums derive parsing, formatting, member conversions, and prefix checks.
///
/// ```
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// #[object_id(prefix = "service")]
/// #[repr(transparent)]
/// struct ServiceId(Text);
/// #[derive(ObjectId)]
/// enum PrincipalId { User(UserId), Service(ServiceId) }
/// let id: PrincipalId = "user_abc".parse()?;
/// assert!(matches!(id, PrincipalId::User(_)));
/// # Ok::<(), trogon_object_id::ParseError>(())
/// ```
///
/// ```compile_fail
/// use trogon_object_id::ObjectId;
/// #[derive(ObjectId)]
/// enum Empty {}
/// ```
///
/// ```compile_fail
/// use trogon_object_id::ObjectId;
/// #[derive(ObjectId)]
/// enum UnitVariant { Empty }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// enum NamedVariant { User { value: UserId } }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::ObjectId;
/// #[derive(ObjectId)]
/// enum MultipleFields { Pair(String, String) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::ObjectId;
/// #[derive(ObjectId)]
/// enum GenericEnum<T> { Value(T) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct First(Text);
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct Second(Text);
/// #[derive(ObjectId)]
/// enum Duplicate { First(First), Second(Second) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "app")]
/// #[repr(transparent)]
/// struct AppId(Text);
/// #[derive(ObjectId)]
/// #[object_id(prefix = "app_admin")]
/// #[repr(transparent)]
/// struct AdminId(Text);
/// #[derive(ObjectId)]
/// enum Overlap { App(AppId), Admin(AdminId) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::ObjectId;
/// #[derive(ObjectId)]
/// union Unsupported { first: u64, second: u32 }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// #[object_id(prefix = "principal")]
/// enum PrincipalId { User(UserId) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// #[object_id(separator = ":")]
/// enum PrincipalId { User(UserId) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// #[object_id(payload = String)]
/// enum PrincipalId { User(UserId) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// #[object_id(unknown = true)]
/// enum PrincipalId { User(UserId) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// enum PrincipalId { #[object_id(prefix = "wrong")] User(UserId) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user")]
/// #[repr(transparent)]
/// struct UserId(Text);
/// #[derive(ObjectId)]
/// enum PrincipalId { User(#[object_id(prefix = "wrong")] UserId) }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[repr(transparent)]
/// struct MissingPrefix(Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user", invalid = true)]
/// #[repr(transparent)]
/// struct UnknownOption(Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user", prefix = "order")]
/// #[repr(transparent)]
/// struct DuplicateOption(Text);
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{ObjectId, Text};
/// #[derive(ObjectId)]
/// #[object_id(prefix = "user", separator = "")]
/// #[repr(transparent)]
/// struct InvalidSeparator(Text);
/// ```
#[cfg(feature = "macros")]
pub use trogon_object_id_derive::ObjectId;

/// Maximum UTF-8 byte length accepted by the built-in payload codecs.
pub const MAX_VALUE_BYTES: usize = 1024;

/// Validation failures that do not retain the rejected identifier text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    #[error("identifier value is empty")]
    EmptyValue,
    #[error("identifier value exceeds the byte limit")]
    ValueTooLong,
    #[error("identifier value contains a control character")]
    ControlCharacter,
    #[error("identifier prefix does not match the expected domain")]
    InvalidPrefix,
    #[error("identifier value does not satisfy the payload constraints")]
    InvalidValue,
    #[error("identifier does not match an enum variant")]
    UnknownPrefix,
}

/// An immutable, validated domain definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Definition {
    object_type: &'static str,
    separator: &'static str,
}

impl Definition {
    /// Defines the prefix grammar used by parsing and formatting.
    ///
    /// # Panics
    ///
    /// Panics unless `object_type` contains 1 to 64 ASCII letters, digits, `_`
    /// or `-` and starts with a letter, and `separator` contains 1 to 4 ASCII
    /// punctuation characters. Invalid definitions fail during const evaluation
    /// when used in a constant.
    pub const fn new(object_type: &'static str, separator: &'static str) -> Self {
        assert!(
            !object_type.is_empty() && object_type.len() <= 64,
            "object type must contain 1 to 64 bytes"
        );
        let kind = object_type.as_bytes();
        assert!(
            kind[0].is_ascii_alphabetic(),
            "object type must start with an ASCII letter"
        );
        let mut index = 0;
        while index < kind.len() {
            assert!(
                kind[index].is_ascii_alphanumeric() || kind[index] == b'_' || kind[index] == b'-',
                "object type contains an invalid character"
            );
            index += 1;
        }
        assert!(
            !separator.is_empty() && separator.len() <= 4,
            "separator must contain 1 to 4 bytes"
        );
        let bytes = separator.as_bytes();
        index = 0;
        while index < bytes.len() {
            assert!(
                bytes[index].is_ascii_punctuation(),
                "separator must contain ASCII punctuation"
            );
            index += 1;
        }
        Self { object_type, separator }
    }

    pub const fn object_type(self) -> &'static str {
        self.object_type
    }
    pub const fn separator(self) -> &'static str {
        self.separator
    }

    /// Removes this exact object type and separator without validating the suffix.
    ///
    /// `Some("")` means the prefix matched but the payload is empty; callers
    /// must still run the payload codec before accepting an identifier.
    pub fn strip_prefix(self, value: &str) -> Option<&str> {
        value.strip_prefix(self.object_type)?.strip_prefix(self.separator)
    }
}

mod object_id;
mod payload;

pub use object_id::ObjectId;
#[cfg(feature = "uuid")]
pub use payload::UuidText;
pub use payload::{IntegerText, Payload, Text};
#[cfg(feature = "uuid")]
pub use uuid::Uuid;

/// A domain's immutable prefix and payload representation.
///
/// ```compile_fail
/// use trogon_object_id::{Definition, Domain, ObjectId, Text};
/// struct User;
/// impl Domain for User {
///     const DEFINITION: Definition = Definition::new("user", "_");
///     type Payload = Text;
/// }
/// struct Order;
/// impl Domain for Order {
///     const DEFINITION: Definition = Definition::new("order", "_");
///     type Payload = Text;
/// }
/// let user: ObjectId<User> = ObjectId::<Order>::try_from_raw("123").unwrap();
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{Definition, Domain, ObjectId, Text};
/// struct InvalidDomain;
/// impl Domain for InvalidDomain {
///     const DEFINITION: Definition = Definition::new("", "_");
///     type Payload = Text;
/// }
/// let invalid = ObjectId::<InvalidDomain>::try_from_raw("123");
/// ```
pub trait Domain {
    const DEFINITION: Definition;
    type Payload: Payload;
}

/// A trusted source of typed payloads. Generated values still pass `Payload::validate`.
pub trait Generator {
    type Payload: Payload;
    /// Produces a candidate payload; [`ObjectId::generate`] validates it before use.
    fn generate() -> Result<Self::Payload, ParseError>;
}

/// Generates UUID version 4 values using the `uuid` crate's system randomness.
///
/// ```compile_fail
/// use trogon_object_id::{Definition, Domain, IntegerText, ObjectId, Uuid4};
/// struct InvoiceDomain;
/// impl Domain for InvoiceDomain {
///     const DEFINITION: Definition = Definition::new("invoice", "_");
///     type Payload = IntegerText;
/// }
/// let invalid = ObjectId::<InvoiceDomain>::generate::<Uuid4>();
/// ```
#[cfg(feature = "uuid")]
pub struct Uuid4;
#[cfg(feature = "uuid")]
impl Generator for Uuid4 {
    type Payload = Uuid;
    fn generate() -> Result<Uuid, ParseError> {
        Ok(Uuid::new_v4())
    }
}

/// Generates UUID version 7 values using the current time and system randomness.
#[cfg(feature = "uuid")]
pub struct Uuid7;
#[cfg(feature = "uuid")]
impl Generator for Uuid7 {
    type Payload = Uuid;
    fn generate() -> Result<Uuid, ParseError> {
        Ok(Uuid::now_v7())
    }
}

/// The raw representation of one concrete identifier domain.
///
/// Implementations are trusted to validate every construction path and preserve
/// their declared prefix. Enum parsers rely on this contract when dispatching
/// owned or borrowed values to a member identifier.
pub trait Identifier: Sized {
    const DEFINITION: Definition;
    type Raw: ?Sized + fmt::Display;
    /// Validates an unprefixed suffix before constructing the identifier.
    fn try_from_raw(value: &str) -> Result<Self, ParseError>;
    /// Applies the same validation as [`Self::try_from_raw`] to owned input.
    ///
    /// Text implementations should retain the input allocation where possible.
    fn try_from_owned(value: String) -> Result<Self, ParseError>;
    fn as_raw(&self) -> &Self::Raw;
}

/// Rejects duplicate or overlapping complete prefixes. Call in a `const` for a handwritten identifier enum.
pub const fn assert_disjoint(definitions: &[Definition]) {
    assert!(!definitions.is_empty(), "identifier enum must contain a variant");
    let mut left = 0;
    while left < definitions.len() {
        let a = definitions[left];
        let mut right = left + 1;
        while right < definitions.len() {
            let b = definitions[right];
            let a_len = a.object_type.len() + a.separator.len();
            let b_len = b.object_type.len() + b.separator.len();
            let common = if a_len < b_len { a_len } else { b_len };
            let mut index = 0;
            while index < common {
                if prefix_byte(a, index) != prefix_byte(b, index) {
                    break;
                }
                index += 1;
            }
            assert!(index < common, "enum prefixes must not duplicate or overlap");
            right += 1;
        }
        left += 1;
    }
}

const fn prefix_byte(definition: Definition, index: usize) -> u8 {
    if index < definition.object_type.len() {
        definition.object_type.as_bytes()[index]
    } else {
        definition.separator.as_bytes()[index - definition.object_type.len()]
    }
}

#[cfg(feature = "serde")]
#[doc(hidden)]
pub mod __private {
    pub use serde;
    use std::fmt;
    use std::marker::PhantomData;

    pub fn serialize<T: fmt::Display, S: serde::Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize_owned<'de, T, E, D, F, O>(deserializer: D, parse: F, parse_owned: O) -> Result<T, D::Error>
    where
        D: serde::Deserializer<'de>,
        F: Fn(&str) -> Result<T, E>,
        O: Fn(String) -> Result<T, E>,
        E: fmt::Display,
    {
        struct StringVisitor<T, F, O> {
            parse: F,
            parse_owned: O,
            marker: PhantomData<fn() -> T>,
        }
        impl<'de, T, E, F, O> serde::de::Visitor<'de> for StringVisitor<T, F, O>
        where
            F: Fn(&str) -> Result<T, E>,
            O: Fn(String) -> Result<T, E>,
            E: fmt::Display,
        {
            type Value = T;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a validated identifier string")
            }
            fn visit_str<E2: serde::de::Error>(self, value: &str) -> Result<T, E2> {
                (self.parse)(value).map_err(E2::custom)
            }
            fn visit_string<E2: serde::de::Error>(self, value: String) -> Result<T, E2> {
                (self.parse_owned)(value).map_err(E2::custom)
            }
        }
        deserializer.deserialize_str(StringVisitor {
            parse,
            parse_owned,
            marker: PhantomData,
        })
    }
}

#[cfg(feature = "serde")]
#[doc(hidden)]
#[macro_export]
macro_rules! __impl_serde {
    ($name:ident) => {
        impl $crate::__private::serde::Serialize for $name {
            fn serialize<S: $crate::__private::serde::Serializer>(
                &self,
                serializer: S,
            ) -> ::core::result::Result<S::Ok, S::Error> {
                $crate::__private::serialize(self, serializer)
            }
        }
        impl<'de> $crate::__private::serde::Deserialize<'de> for $name {
            fn deserialize<D: $crate::__private::serde::Deserializer<'de>>(
                deserializer: D,
            ) -> ::core::result::Result<Self, D::Error> {
                $crate::__private::deserialize_owned(
                    deserializer,
                    <Self as ::core::str::FromStr>::from_str,
                    <Self as ::core::convert::TryFrom<::std::string::String>>::try_from,
                )
            }
        }
    };
}

#[cfg(not(feature = "serde"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __impl_serde {
    ($name:ident) => {};
}

/// Explicit raw JSON field encoding for a concrete domain identifier.
#[cfg(feature = "serde")]
pub mod serde_raw {
    use crate::Identifier;
    /// Encodes only the raw payload, so storage must establish the identifier's domain.
    pub fn serialize<T: Identifier, S: serde::Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value.as_raw())
    }
    /// Decodes an unprefixed string through the identifier's checked raw constructor.
    ///
    /// The adapter accepts owned strings without discarding their allocation.
    pub fn deserialize<'de, T: Identifier, D: serde::Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
        crate::__private::deserialize_owned(deserializer, T::try_from_raw, T::try_from_owned)
    }
}
