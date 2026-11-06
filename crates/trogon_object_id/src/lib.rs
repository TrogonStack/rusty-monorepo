#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

use std::fmt;

extern crate self as trogon_object_id;

#[cfg(feature = "derive")]
pub use trogon_object_id_derive::{ObjectIdDomain, ObjectIdUnion};

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

/// Trusted, deterministic application validation for text payloads.
pub trait Validator {
    fn validate(value: &str) -> Result<(), ParseError>;
}

pub mod validators {
    use crate::{ParseError, Validator};

    pub struct Any;
    impl Validator for Any {
        fn validate(_value: &str) -> Result<(), ParseError> {
            Ok(())
        }
    }

    /// Signed ASCII decimal syntax without a numeric range or normalization.
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

    /// Spelling-preserving UUID validation for a `Text` payload.
    #[cfg(feature = "uuid")]
    pub struct Uuid;
    #[cfg(feature = "uuid")]
    impl Validator for Uuid {
        fn validate(value: &str) -> Result<(), ParseError> {
            <crate::Uuid as crate::Payload>::parse(value).map(|_| ())
        }
    }
}

mod object_id;
mod payload;

pub use object_id::ObjectId;
pub use payload::{Payload, Text};
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
    fn generate() -> Result<Self::Payload, ParseError>;
}

/// Generates UUID version 4 values using the `uuid` crate's system randomness.
///
/// ```compile_fail
/// use trogon_object_id::{object_id, validators::Integer, Uuid4};
/// object_id! { struct InvoiceId("invoice", validator = Integer); }
/// let invalid = InvoiceId::generate::<Uuid4>();
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
pub trait Identifier: Sized {
    const DEFINITION: Definition;
    type Raw: ?Sized + fmt::Display;
    fn try_from_raw(value: &str) -> Result<Self, ParseError>;
    fn try_from_owned(value: String) -> Result<Self, ParseError>;
    fn as_raw(&self) -> &Self::Raw;
}

/// Rejects duplicate or overlapping complete prefixes. Call in a `const` for a handwritten union.
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
/// ```
/// use trogon_object_id::{object_id, union_object_id};
/// object_id! { #[cfg(any())] pub struct DisabledId("disabled"); }
/// object_id! { /// A conditional domain.
///     #[cfg(any())] pub struct DocumentedId("disabled");
/// }
/// object_id! { #[cfg_attr(all(), cfg(any()), doc = "disabled")] pub struct ConditionalId("disabled"); }
/// object_id! { #[cfg_attr(any(), cfg(any()))] pub struct EnabledId("enabled"); }
/// union_object_id! { #[cfg(any())] pub enum DisabledUnion { Missing(MissingId) } }
/// union_object_id! { #[cfg_attr(all(), cfg(any()))] pub enum ConditionalUnion { Missing(MissingId) } }
/// union_object_id! { #[cfg(all())] pub enum EnabledUnion { Enabled(EnabledId) } }
/// let enabled = EnabledId::try_from_raw("123")?;
/// assert_eq!(EnabledUnion::from(enabled).to_string(), "enabled_123");
/// # Ok::<(), trogon_object_id::ParseError>(())
/// ```
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
    (@attrs [$($kept:tt)*] #[cfg($($condition:tt)*)] $($rest:tt)*) => {
        #[cfg($($condition)*)]
        $crate::object_id! { @attrs [$($kept)*] $($rest)* }
    };
    (@attrs [$($kept:tt)*] #[cfg_attr($condition:meta, $($attribute:tt)*)] $($rest:tt)*) => {
        #[cfg($condition)]
        $crate::object_id! { @split_attrs [$($kept)*] [$($rest)*] [] [] $($attribute)* }
        #[cfg(not($condition))]
        $crate::object_id! { @attrs [$($kept)*] $($rest)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [] ) => {
        $crate::object_id! { @attrs [$($kept)*] $($attrs)* $($rest)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [$($current:tt)+] ) => {
        $crate::object_id! { @attrs [$($kept)*] $($attrs)* #[$($current)+] $($rest)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [$($current:tt)+] , $($tail:tt)*) => {
        $crate::object_id! { @split_attrs [$($kept)*] [$($rest)*] [$($attrs)* #[$($current)+]] [] $($tail)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [$($current:tt)*] $next:tt $($tail:tt)*) => {
        $crate::object_id! { @split_attrs [$($kept)*] [$($rest)*] [$($attrs)*] [$($current)* $next] $($tail)* }
    };
    (@attrs [$($kept:tt)*] #[$attr:meta] $($rest:tt)*) => {
        $crate::object_id! { @attrs [$($kept)* #[$attr]] $($rest)* }
    };

    (@attrs [$($attr:tt)*] $vis:vis struct $name:ident($kind:literal);) => {
        $crate::object_id! { @attrs [$($attr)*] $vis struct $name($kind, separator = "_", payload = $crate::Text); }
    };
    (@attrs [$($attr:tt)*] $vis:vis struct $name:ident($kind:literal, separator = $sep:literal);) => {
        $crate::object_id! { @attrs [$($attr)*] $vis struct $name($kind, separator = $sep, payload = $crate::Text); }
    };
    (@attrs [$($attr:tt)*] $vis:vis struct $name:ident($kind:literal, validator = $validator:ty);) => {
        $crate::object_id! { @attrs [$($attr)*] $vis struct $name($kind, separator = "_", payload = $crate::Text<$validator>); }
    };
    (@attrs [$($attr:tt)*] $vis:vis struct $name:ident($kind:literal, payload = $payload:ty);) => {
        $crate::object_id! { @attrs [$($attr)*] $vis struct $name($kind, separator = "_", payload = $payload); }
    };
    (@attrs [$($attr:tt)*] $vis:vis struct $name:ident($kind:literal, separator = $sep:literal, validator = $validator:ty);) => {
        $crate::object_id! { @attrs [$($attr)*] $vis struct $name($kind, separator = $sep, payload = $crate::Text<$validator>); }
    };
    (@attrs [$($attr:tt)*] $vis:vis struct $name:ident($kind:literal, separator = $sep:literal, payload = $payload:ty);) => {
        const _: $crate::Definition = $crate::Definition::new($kind, $sep);
        $($attr)*
        #[repr(transparent)]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis struct $name($crate::ObjectId<Self>);
        impl $crate::Domain for $name {
            const DEFINITION: $crate::Definition = $crate::Definition::new($kind, $sep);
            type Payload = $payload;
        }
        impl $name {
            pub fn try_from_raw(value: &str) -> ::core::result::Result<Self, $crate::ParseError> {
                $crate::ObjectId::try_from_raw(value).map(Self)
            }
            pub fn try_from_owned(value: ::std::string::String) -> ::core::result::Result<Self, $crate::ParseError> {
                $crate::ObjectId::try_from_owned(value).map(Self)
            }
            pub fn try_from_payload(value: $payload) -> ::core::result::Result<Self, $crate::ParseError> {
                $crate::ObjectId::try_from_payload(value).map(Self)
            }
            pub fn payload(&self) -> &$payload { self.0.payload() }
            pub fn into_payload(self) -> $payload { self.0.into_payload() }
            pub fn as_raw(&self) -> &<$payload as $crate::Payload>::Raw { self.0.as_raw() }
            pub fn generate<G>() -> ::core::result::Result<Self, $crate::ParseError>
            where G: $crate::Generator<Payload = $payload> {
                $crate::ObjectId::generate::<G>().map(Self)
            }
        }
        impl $crate::Identifier for $name {
            const DEFINITION: $crate::Definition = <Self as $crate::Domain>::DEFINITION;
            type Raw = <$payload as $crate::Payload>::Raw;
            fn try_from_raw(value: &str) -> ::core::result::Result<Self, $crate::ParseError> { Self::try_from_raw(value) }
            fn try_from_owned(value: ::std::string::String) -> ::core::result::Result<Self, $crate::ParseError> { Self::try_from_owned(value) }
            fn as_raw(&self) -> &Self::Raw { self.as_raw() }
        }
        impl ::core::str::FromStr for $name {
            type Err = $crate::ParseError;
            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> { value.parse().map(Self) }
        }
        impl ::core::convert::TryFrom<&str> for $name {
            type Error = $crate::ParseError;
            fn try_from(value: &str) -> ::core::result::Result<Self, Self::Error> { value.parse() }
        }
        impl ::core::convert::TryFrom<::std::string::String> for $name {
            type Error = $crate::ParseError;
            fn try_from(value: ::std::string::String) -> ::core::result::Result<Self, Self::Error> {
                $crate::ObjectId::<Self>::try_from(value).map(Self)
            }
        }
        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result { ::core::fmt::Display::fmt(&self.0, formatter) }
        }
        $crate::__impl_serde!($name);
    };
    (@attrs $($invalid:tt)*) => {
        ::core::compile_error!("invalid object_id declaration");
    };
    ($($input:tt)*) => {
        $crate::object_id! { @attrs [] $($input)* }
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
    (@attrs [$($kept:tt)*] #[cfg($($condition:tt)*)] $($rest:tt)*) => {
        #[cfg($($condition)*)]
        $crate::union_object_id! { @attrs [$($kept)*] $($rest)* }
    };
    (@attrs [$($kept:tt)*] #[cfg_attr($condition:meta, $($attribute:tt)*)] $($rest:tt)*) => {
        #[cfg($condition)]
        $crate::union_object_id! { @split_attrs [$($kept)*] [$($rest)*] [] [] $($attribute)* }
        #[cfg(not($condition))]
        $crate::union_object_id! { @attrs [$($kept)*] $($rest)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [] ) => {
        $crate::union_object_id! { @attrs [$($kept)*] $($attrs)* $($rest)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [$($current:tt)+] ) => {
        $crate::union_object_id! { @attrs [$($kept)*] $($attrs)* #[$($current)+] $($rest)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [$($current:tt)+] , $($tail:tt)*) => {
        $crate::union_object_id! { @split_attrs [$($kept)*] [$($rest)*] [$($attrs)* #[$($current)+]] [] $($tail)* }
    };
    (@split_attrs [$($kept:tt)*] [$($rest:tt)*] [$($attrs:tt)*] [$($current:tt)*] $next:tt $($tail:tt)*) => {
        $crate::union_object_id! { @split_attrs [$($kept)*] [$($rest)*] [$($attrs)*] [$($current)* $next] $($tail)* }
    };
    (@attrs [$($kept:tt)*] #[$attr:meta] $($rest:tt)*) => {
        $crate::union_object_id! { @attrs [$($kept)* #[$attr]] $($rest)* }
    };

    (@attrs [$($attr:tt)*] $vis:vis enum $name:ident { $($variant:ident($member:ty)),+ $(,)? }) => {
        const _: () = $crate::assert_disjoint(&[$(<$member as $crate::Identifier>::DEFINITION),+]);
        $($attr)*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name { $($variant($member)),+ }
        $(impl ::core::convert::From<$member> for $name {
            fn from(value: $member) -> Self { Self::$variant(value) }
        })+
        impl ::core::str::FromStr for $name {
            type Err = $crate::ParseError;
            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> {
                $(if <$member as $crate::Identifier>::DEFINITION.strip_prefix(value).is_some() {
                    return value.parse::<$member>().map(Self::$variant);
                })+
                Err($crate::ParseError::UnknownPrefix)
            }
        }
        impl ::core::convert::TryFrom<&str> for $name {
            type Error = $crate::ParseError;
            fn try_from(value: &str) -> ::core::result::Result<Self, Self::Error> { value.parse() }
        }
        impl ::core::convert::TryFrom<::std::string::String> for $name {
            type Error = $crate::ParseError;
            fn try_from(mut value: ::std::string::String) -> ::core::result::Result<Self, Self::Error> {
                $(if let ::core::option::Option::Some(raw) = <$member as $crate::Identifier>::DEFINITION.strip_prefix(&value) {
                    let prefix_len = value.len() - raw.len();
                    value.drain(..prefix_len);
                    return <$member as $crate::Identifier>::try_from_owned(value).map(Self::$variant);
                })+
                Err($crate::ParseError::UnknownPrefix)
            }
        }
        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                match self { $(Self::$variant(value) => ::core::fmt::Display::fmt(value, formatter)),+ }
            }
        }
        $crate::__impl_serde!($name);
    };
    (@attrs $($invalid:tt)*) => {
        ::core::compile_error!("invalid union_object_id declaration");
    };
    ($($input:tt)*) => {
        $crate::union_object_id! { @attrs [] $($input)* }
    };
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
    pub fn serialize<T: Identifier, S: serde::Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value.as_raw())
    }
    pub fn deserialize<'de, T: Identifier, D: serde::Deserializer<'de>>(deserializer: D) -> Result<T, D::Error> {
        crate::__private::deserialize_owned(deserializer, T::try_from_raw, T::try_from_owned)
    }
}
