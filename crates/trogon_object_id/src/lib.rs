#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

pub const MAX_VALUE_BYTES: usize = 1024;

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
    #[error("identifier value does not satisfy the domain validator")]
    InvalidValue,
    #[error("identifier does not match a union member")]
    UnknownPrefix,
}

/// An immutable, validated domain definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Definition {
    object_type: &'static str,
    separator: &'static str,
}

impl Definition {
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

    pub fn strip_prefix(self, value: &str) -> Option<&str> {
        value.strip_prefix(self.object_type)?.strip_prefix(self.separator)
    }
}

/// Trusted, deterministic application validation after core value checks.
pub trait Validator {
    fn validate(value: &str) -> Result<(), ParseError>;
}

pub mod validators {
    #[cfg(feature = "uuid")]
    pub struct Uuid;

    #[cfg(feature = "uuid")]
    impl Validator for Uuid {
        fn validate(_value: &str) -> Result<(), ParseError> {
            Ok(())
        }
    }

    use crate::{ParseError, Validator};

    /// Accept any value passing the core length and control-character checks.
    pub struct Any;
    impl Validator for Any {
        fn validate(_value: &str) -> Result<(), ParseError> {
            Ok(())
        }
    }

    /// An arbitrarily sized signed ASCII decimal integer, preserving its text.
    pub struct Integer;
    impl Validator for Integer {
        fn validate(value: &str) -> Result<(), ParseError> {
            let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
            if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
                Ok(())
            } else {
                Err(ParseError::InvalidValue)
            }
        }
    }
}

/// The compile-time definition associated with an identifier's domain.
pub trait Domain {
    const DEFINITION: Definition;
    type Validator: Validator;
}

/// A typed identifier whose raw value can only be constructed through validation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId<D: Domain> {
    value: String,
    domain: PhantomData<fn() -> D>,
}

impl<D: Domain> ObjectId<D> {
    pub fn try_from_raw(value: &str) -> Result<Self, ParseError> {
        let _definition = D::DEFINITION;
        if value.is_empty() {
            return Err(ParseError::EmptyValue);
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(ParseError::ValueTooLong);
        }
        if value.chars().any(char::is_control) {
            return Err(ParseError::ControlCharacter);
        }
        D::Validator::validate(value)?;
        Ok(Self {
            value: value.to_owned(),
            domain: PhantomData,
        })
    }

    pub fn as_raw(&self) -> &str {
        &self.value
    }
}

impl<D: Domain> FromStr for ObjectId<D> {
    type Err = ParseError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let raw = D::DEFINITION.strip_prefix(value).ok_or(ParseError::InvalidPrefix)?;
        Self::try_from_raw(raw)
    }
}

impl<D: Domain> fmt::Display for ObjectId<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let definition = D::DEFINITION;
        formatter.write_str(definition.object_type())?;
        formatter.write_str(definition.separator())?;
        formatter.write_str(self.as_raw())
    }
}

/// Access to the validated raw storage form of a concrete domain identifier.
/// A trusted source of raw values for one validator type.
pub trait Generator {
    type Validator: Validator;
    fn generate() -> Result<String, ParseError>;
}

#[cfg(feature = "uuid")]
pub struct Uuid4;
#[cfg(feature = "uuid")]
impl Generator for Uuid4 {
    type Validator = validators::Uuid;
    fn generate() -> Result<String, ParseError> {
        Err(ParseError::InvalidValue)
    }
}

#[cfg(feature = "uuid")]
pub struct Uuid7;
#[cfg(feature = "uuid")]
impl Generator for Uuid7 {
    type Validator = validators::Uuid;
    fn generate() -> Result<String, ParseError> {
        Err(ParseError::InvalidValue)
    }
}

pub trait Identifier: Sized {
    fn try_from_raw(value: &str) -> Result<Self, ParseError>;
    fn as_raw(&self) -> &str;
}

/// Validates all complete prefixes before a union can be declared.
#[doc(hidden)]
pub const fn assert_disjoint(definitions: &[Definition]) {
    assert!(!definitions.is_empty(), "union must contain a member");
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
            assert!(index < common, "union prefixes must not duplicate or overlap");
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

/// Declare a distinct identifier type with a compile-time checked prefix.
///
/// ```compile_fail
/// use trogon_object_id::object_id;
/// object_id! { struct UserId("user"); }
/// object_id! { struct OrderId("order"); }
/// let user: UserId = OrderId::try_from_raw("123").unwrap();
/// ```
///
/// ```compile_fail
/// use trogon_object_id::object_id;
/// object_id! { struct EmptyId(""); }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::object_id;
/// object_id! { struct InvalidId("bad prefix"); }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::object_id;
/// object_id! { struct EmptySeparator("user", separator = ""); }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::object_id;
/// object_id! { struct InvalidSeparator("user", separator = "a"); }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::object_id;
/// mod domain { trogon_object_id::object_id! { pub struct UserId("user"); } }
/// let forged = domain::UserId("unvalidated".to_owned());
/// ```
#[macro_export]
macro_rules! object_id {
    ($(#[$attr:meta])* $vis:vis struct $name:ident($kind:literal);) => {
        $crate::object_id! { $(#[$attr])* $vis struct $name($kind, separator = "_", validator = $crate::validators::Any); }
    };
    ($(#[$attr:meta])* $vis:vis struct $name:ident($kind:literal, separator = $sep:literal);) => {
        $crate::object_id! { $(#[$attr])* $vis struct $name($kind, separator = $sep, validator = $crate::validators::Any); }
    };
    ($(#[$attr:meta])* $vis:vis struct $name:ident($kind:literal, validator = $validator:ty);) => {
        $crate::object_id! { $(#[$attr])* $vis struct $name($kind, separator = "_", validator = $validator); }
    };
    ($(#[$attr:meta])* $vis:vis struct $name:ident($kind:literal, separator = $sep:literal, validator = $validator:ty);) => {
        const _: $crate::Definition = $crate::Definition::new($kind, $sep);
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis struct $name($crate::ObjectId<Self>);
        impl $crate::Domain for $name {
            const DEFINITION: $crate::Definition = $crate::Definition::new($kind, $sep);
            type Validator = $validator;
        }
        impl $name {
            pub fn try_from_raw(value: &str) -> ::core::result::Result<Self, $crate::ParseError> {
                $crate::ObjectId::try_from_raw(value).map(Self)
            }
            pub fn as_raw(&self) -> &str { self.0.as_raw() }
            pub fn generate<G>() -> ::core::result::Result<Self, $crate::ParseError>
            where G: $crate::Generator<Validator = <Self as $crate::Domain>::Validator> {
                let raw = G::generate()?;
                Self::try_from_raw(&raw)
            }
        }
        impl $crate::Identifier for $name {
            fn try_from_raw(value: &str) -> ::core::result::Result<Self, $crate::ParseError> { Self::try_from_raw(value) }
            fn as_raw(&self) -> &str { self.as_raw() }
        }
        impl ::core::str::FromStr for $name {
            type Err = $crate::ParseError;
            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> { value.parse().map(Self) }
        }
        impl ::core::convert::TryFrom<&str> for $name {
            type Error = $crate::ParseError;
            fn try_from(value: &str) -> ::core::result::Result<Self, Self::Error> { value.parse() }
        }
        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result { ::core::fmt::Display::fmt(&self.0, formatter) }
        }
        $crate::__impl_serde!($name);
    };
}

/// Declare an exhaustive union with non-overlapping complete prefixes.
///
/// ```compile_fail
/// use trogon_object_id::{object_id, union_object_id};
/// object_id! { struct First("user"); }
/// object_id! { struct Second("user"); }
/// union_object_id! { enum Duplicate { First(First), Second(Second) } }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{object_id, union_object_id};
/// object_id! { struct First("app"); }
/// object_id! { struct Second("app_admin"); }
/// union_object_id! { enum Overlap { First(First), Second(Second) } }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::union_object_id;
/// union_object_id! { enum Empty {} }
/// ```
///
/// ```compile_fail
/// use trogon_object_id::{object_id, union_object_id};
/// object_id! { struct User("user"); }
/// object_id! { struct Order("order"); }
/// union_object_id! { enum Principal { User(User) } }
/// let invalid = Principal::from(Order::try_from_raw("123").unwrap());
/// ```
#[macro_export]
macro_rules! union_object_id {
    ($(#[$attr:meta])* $vis:vis enum $name:ident { $($variant:ident($member:ty)),+ $(,)? }) => {
        const _: () = $crate::assert_disjoint(&[$(<$member as $crate::Domain>::DEFINITION),+]);
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name { $($variant($member)),+ }
        $(impl ::core::convert::From<$member> for $name {
            fn from(value: $member) -> Self { Self::$variant(value) }
        })+
        impl ::core::str::FromStr for $name {
            type Err = $crate::ParseError;
            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> {
                $(if <$member as $crate::Domain>::DEFINITION.strip_prefix(value).is_some() {
                    return value.parse::<$member>().map(Self::$variant);
                })+
                Err($crate::ParseError::UnknownPrefix)
            }
        }
        impl ::core::convert::TryFrom<&str> for $name {
            type Error = $crate::ParseError;
            fn try_from(value: &str) -> ::core::result::Result<Self, Self::Error> { value.parse() }
        }
        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self { $(Self::$variant(value) => ::core::fmt::Display::fmt(value, formatter)),+ }
            }
        }
        $crate::__impl_serde!($name);
    };
}

#[cfg(feature = "serde")]
#[doc(hidden)]
pub mod __private {
    pub use serde;
    use std::fmt;
    use std::str::FromStr;

    pub fn serialize<T: fmt::Display, S: serde::Serializer>(_value: &T, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("identifier serialization is unavailable"))
    }

    pub fn deserialize<'de, T: FromStr, D: serde::Deserializer<'de>>(_deserializer: D) -> Result<T, D::Error>
    where
        T::Err: fmt::Display,
    {
        Err(serde::de::Error::custom("identifier deserialization is unavailable"))
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
                $crate::__private::deserialize(deserializer)
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
    pub fn serialize<T: Identifier, S: serde::Serializer>(_value: &T, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("raw identifier serialization is unavailable"))
    }
    pub fn deserialize<'de, T: Identifier, D: serde::Deserializer<'de>>(_deserializer: D) -> Result<T, D::Error> {
        Err(serde::de::Error::custom(
            "raw identifier deserialization is unavailable",
        ))
    }
}
