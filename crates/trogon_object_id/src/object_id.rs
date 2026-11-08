#[cfg(feature = "alloc")]
use alloc::string::String;
use core::fmt;
use core::hash::Hash;
use core::marker::PhantomData;
use core::str::FromStr;

use crate::{Domain, Generator, Identifier, ParseError, Payload};

/// An immutable identifier with the layout of its domain's payload.
///
/// [`core::str::FromStr`] and `TryFrom` parse the complete prefixed form.
/// The `try_from_raw` and `try_from_owned` constructors interpret the input as
/// payload text, without stripping or inferring a prefix. [`fmt::Display`]
/// always emits the domain prefix and separator.
#[repr(transparent)]
pub struct ObjectId<D: Domain> {
    payload: D::Payload,
    domain: PhantomData<fn() -> D>,
}

impl<D: Domain> ObjectId<D> {
    /// Parses unprefixed text using the domain's payload codec.
    ///
    /// Validation and normalization follow [`Payload::parse`]. For `Text`,
    /// successful parsing allocates an owned copy; compact UUID parsing does not.
    pub fn try_from_raw(value: &str) -> Result<Self, ParseError> {
        let _definition = D::DEFINITION;
        D::Payload::parse(value).map(Self::from_validated)
    }

    /// Takes ownership of unprefixed text and applies the payload's validation.
    ///
    /// `Text` preserves the spelling and reuses the input allocation.
    /// To consume a complete prefixed identifier, use `TryFrom<String>` instead.
    #[cfg(feature = "alloc")]
    pub fn try_from_owned(value: String) -> Result<Self, ParseError> {
        let _definition = D::DEFINITION;
        D::Payload::parse_owned(value).map(Self::from_validated)
    }

    /// Checks an existing typed payload before transferring it into this domain.
    ///
    /// Custom payloads must reject invalid state in [`Payload::validate`]; using
    /// a typed input does not bypass that check.
    pub fn try_from_payload(payload: D::Payload) -> Result<Self, ParseError> {
        let _definition = D::DEFINITION;
        payload.validate()?;
        Ok(Self::from_validated(payload))
    }

    /// Generates a typed payload, then validates it before constructing the ID.
    ///
    /// Errors from either the generator or payload validation are preserved.
    pub fn generate<G: Generator<Payload = D::Payload>>() -> Result<Self, ParseError> {
        Self::try_from_payload(G::generate()?)
    }

    pub fn payload(&self) -> &D::Payload {
        &self.payload
    }
    pub fn into_payload(self) -> D::Payload {
        self.payload
    }
    /// Borrows the storage representation without the domain prefix.
    ///
    /// This is `&str` for text payloads and `&Uuid` for compact UUID payloads.
    /// Use [`fmt::Display`] when the domain must travel with the value.
    pub fn as_raw(&self) -> &<D::Payload as Payload>::Raw {
        self.payload.as_raw()
    }
    pub fn as_str(&self) -> &str
    where
        D::Payload: Payload<Raw = str>,
    {
        self.as_raw()
    }

    fn from_validated(payload: D::Payload) -> Self {
        Self {
            payload,
            domain: PhantomData,
        }
    }
}

#[cfg(feature = "template")]
impl<D: crate::TemplateDomain> ObjectId<D> {
    /// Generates the bound recipe's UUID and validates it for this identifier.
    pub fn from_identity(
        input: <<D as crate::TemplateDomain>::Template as crate::UuidTemplate>::Input<'_>,
    ) -> Result<Self, ParseError> {
        Self::try_from_payload(<D::Template as crate::UuidTemplate>::uuid5(input))
    }
}

impl<D: Domain> FromStr for ObjectId<D> {
    type Err = ParseError;
    fn from_str(value: &str) -> Result<Self, ParseError> {
        let raw = D::DEFINITION.strip_prefix(value).ok_or(ParseError::InvalidPrefix)?;
        Self::try_from_raw(raw)
    }
}
impl<D: Domain> TryFrom<&str> for ObjectId<D> {
    type Error = ParseError;
    fn try_from(value: &str) -> Result<Self, ParseError> {
        value.parse()
    }
}
#[cfg(feature = "alloc")]
impl<D: Domain> TryFrom<String> for ObjectId<D> {
    type Error = ParseError;
    fn try_from(mut value: String) -> Result<Self, ParseError> {
        let raw = D::DEFINITION.strip_prefix(&value).ok_or(ParseError::InvalidPrefix)?;
        let prefix_len = value.len() - raw.len();
        value.drain(..prefix_len);
        Self::try_from_owned(value)
    }
}
impl<D: Domain> Identifier for ObjectId<D> {
    const DEFINITION: crate::Definition = D::DEFINITION;
    type Raw = <D::Payload as Payload>::Raw;
    fn try_from_raw(value: &str) -> Result<Self, ParseError> {
        Self::try_from_raw(value)
    }
    #[cfg(feature = "alloc")]
    fn try_from_owned(value: String) -> Result<Self, ParseError> {
        Self::try_from_owned(value)
    }
    fn as_raw(&self) -> &Self::Raw {
        self.as_raw()
    }
}
impl<D: Domain> fmt::Display for ObjectId<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let definition = D::DEFINITION;
        formatter.write_str(definition.object_type())?;
        formatter.write_str(definition.separator())?;
        self.payload.fmt(formatter)
    }
}
impl<D: Domain> fmt::Debug for ObjectId<D>
where
    D::Payload: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObjectId")
            .field("domain", &D::DEFINITION)
            .field("payload", &self.payload)
            .finish()
    }
}
impl<D: Domain> Clone for ObjectId<D>
where
    D::Payload: Clone,
{
    fn clone(&self) -> Self {
        Self::from_validated(self.payload.clone())
    }
}
impl<D: Domain> Copy for ObjectId<D> where D::Payload: Copy {}
impl<D: Domain> PartialEq for ObjectId<D>
where
    D::Payload: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.payload == other.payload
    }
}
impl<D: Domain> Eq for ObjectId<D> where D::Payload: Eq {}
impl<D: Domain> PartialOrd for ObjectId<D>
where
    D::Payload: PartialOrd,
{
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.payload.partial_cmp(&other.payload)
    }
}
impl<D: Domain> Ord for ObjectId<D>
where
    D::Payload: Ord,
{
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.payload.cmp(&other.payload)
    }
}
impl<D: Domain> Hash for ObjectId<D>
where
    D::Payload: Hash,
{
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.payload.hash(state);
    }
}

#[cfg(feature = "serde")]
impl<D: Domain> serde::Serialize for ObjectId<D> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}
#[cfg(feature = "serde")]
impl<'de, D: Domain> serde::Deserialize<'de> for ObjectId<D> {
    fn deserialize<S: serde::Deserializer<'de>>(deserializer: S) -> Result<Self, S::Error> {
        crate::__deserialize!(deserializer, Self::from_str, Self::try_from)
    }
}

impl<D: Domain> TryFrom<crate::RawPayload<D::Payload>> for ObjectId<D> {
    type Error = ParseError;
    fn try_from(value: crate::RawPayload<D::Payload>) -> Result<Self, Self::Error> {
        Self::try_from_payload(value.0)
    }
}

#[cfg(any(feature = "alloc", feature = "uuid"))]
macro_rules! infallible_payload {
    ($payload:ty) => {
        impl<D: Domain<Payload = $payload>> From<$payload> for ObjectId<D> {
            fn from(payload: $payload) -> Self {
                let _definition = D::DEFINITION;
                Self::from_validated(payload)
            }
        }
    };
}
#[cfg(feature = "alloc")]
infallible_payload!(crate::Text);
#[cfg(feature = "alloc")]
infallible_payload!(crate::IntegerText);
#[cfg(all(feature = "alloc", feature = "uuid"))]
infallible_payload!(crate::UuidText);
#[cfg(feature = "uuid")]
infallible_payload!(crate::Uuid);
