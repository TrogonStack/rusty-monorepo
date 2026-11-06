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
        Self { object_type, separator }
    }

    pub const fn object_type(self) -> &'static str { self.object_type }
    pub const fn separator(self) -> &'static str { self.separator }

    pub fn strip_prefix(self, value: &str) -> Option<&str> {
        value.strip_prefix(self.object_type)?.strip_prefix(self.separator)
    }
}

/// Trusted, deterministic application validation after core value checks.
pub trait Validator {
    fn validate(value: &str) -> Result<(), ParseError>;
}

pub mod validators {
    use crate::{ParseError, Validator};

    /// Accept any value passing the core length and control-character checks.
    pub struct Any;
    impl Validator for Any {
        fn validate(_value: &str) -> Result<(), ParseError> { Ok(()) }
    }

    /// An arbitrarily sized signed ASCII decimal integer, preserving its text.
    pub struct Integer;
    impl Validator for Integer {
        fn validate(_value: &str) -> Result<(), ParseError> { Ok(()) }
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
        if value.is_empty() { return Err(ParseError::EmptyValue); }
        if value.len() > MAX_VALUE_BYTES { return Err(ParseError::ValueTooLong); }
        if value.chars().any(char::is_control) { return Err(ParseError::ControlCharacter); }
        D::Validator::validate(value)?;
        Ok(Self { value: value.to_owned(), domain: PhantomData })
    }

    pub fn as_raw(&self) -> &str { &self.value }
}

impl<D: Domain> FromStr for ObjectId<D> {
    type Err = ParseError;
    fn from_str(_value: &str) -> Result<Self, Self::Err> { Err(ParseError::InvalidPrefix) }
}

impl<D: Domain> fmt::Display for ObjectId<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result { formatter.write_str("") }
}

/// Access to the validated raw storage form of a concrete domain identifier.
pub trait Identifier: Sized {
    fn try_from_raw(value: &str) -> Result<Self, ParseError>;
    fn as_raw(&self) -> &str;
}

/// Validates all complete prefixes before a union can be declared.
#[doc(hidden)]
pub const fn assert_disjoint(_definitions: &[Definition]) {}

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
            fn from_str(_value: &str) -> ::core::result::Result<Self, Self::Err> { Err($crate::ParseError::UnknownPrefix) }
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
    };
}
