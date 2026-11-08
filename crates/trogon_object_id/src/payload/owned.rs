use super::{check_integer, check_integer_syntax, check_text};
#[cfg(feature = "uuid")]
use super::{parse_uuid, parse_uuid_syntax};
use crate::{ParseError, Payload};
use alloc::borrow::ToOwned;
use alloc::string::String;
use core::fmt;
use core::str::FromStr;
/// Validated, spelling-preserving owned text.
///
/// Construction through `TryFrom<&str>` or `TryFrom<String>` requires nonempty
/// text, at most [`crate::MAX_VALUE_BYTES`] UTF-8 bytes and no Unicode control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Text(String);

impl Text {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for Text {
    type Error = ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        check_text(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for Text {
    type Error = ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        check_text(&value)?;
        Ok(Self(value))
    }
}

impl FromStr for Text {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl From<Text> for String {
    fn from(value: Text) -> Self {
        value.0
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Payload for Text {
    type Raw = str;

    fn parse(value: &str) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    #[cfg(feature = "alloc")]
    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    fn validate(&self) -> Result<(), ParseError> {
        Ok(())
    }

    fn as_raw(&self) -> &str {
        self.as_str()
    }
}

/// Signed ASCII decimal text without a numeric range or normalization.
///
/// `TryFrom` preserves signs and leading zeros. An owned input keeps its allocation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct IntegerText(Text);

impl IntegerText {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl TryFrom<&str> for IntegerText {
    type Error = ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        check_integer(value)?;
        Ok(Self(Text(value.to_owned())))
    }
}

impl TryFrom<String> for IntegerText {
    type Error = ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        check_integer(&value)?;
        Ok(Self(Text(value)))
    }
}

impl TryFrom<Text> for IntegerText {
    type Error = ParseError;

    fn try_from(value: Text) -> Result<Self, Self::Error> {
        check_integer_syntax(value.as_str())?;
        Ok(Self(value))
    }
}

impl FromStr for IntegerText {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

impl From<IntegerText> for Text {
    fn from(value: IntegerText) -> Self {
        value.0
    }
}

impl From<IntegerText> for String {
    fn from(value: IntegerText) -> Self {
        value.0.into()
    }
}

impl AsRef<str> for IntegerText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for IntegerText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Payload for IntegerText {
    type Raw = str;

    fn parse(value: &str) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    #[cfg(feature = "alloc")]
    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    fn validate(&self) -> Result<(), ParseError> {
        Ok(())
    }

    fn as_raw(&self) -> &str {
        self.as_str()
    }
}

/// Hyphenated UUID text that retains its original capitalization.
///
/// Use [`crate::Uuid`] instead when a compact, canonical binary payload is desired.
#[cfg(feature = "uuid")]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct UuidText(Text);

#[cfg(feature = "uuid")]
impl UuidText {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[cfg(feature = "uuid")]
impl TryFrom<&str> for UuidText {
    type Error = ParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        parse_uuid(value)?;
        Ok(Self(Text(value.to_owned())))
    }
}

#[cfg(feature = "uuid")]
impl TryFrom<String> for UuidText {
    type Error = ParseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        parse_uuid(&value)?;
        Ok(Self(Text(value)))
    }
}

#[cfg(feature = "uuid")]
impl TryFrom<Text> for UuidText {
    type Error = ParseError;

    fn try_from(value: Text) -> Result<Self, Self::Error> {
        parse_uuid_syntax(value.as_str())?;
        Ok(Self(value))
    }
}

#[cfg(feature = "uuid")]
impl FromStr for UuidText {
    type Err = ParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value)
    }
}

#[cfg(feature = "uuid")]
impl From<UuidText> for Text {
    fn from(value: UuidText) -> Self {
        value.0
    }
}

#[cfg(feature = "uuid")]
impl From<UuidText> for String {
    fn from(value: UuidText) -> Self {
        value.0.into()
    }
}

#[cfg(feature = "uuid")]
impl AsRef<str> for UuidText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[cfg(feature = "uuid")]
impl fmt::Display for UuidText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(feature = "uuid")]
impl Payload for UuidText {
    type Raw = str;

    fn parse(value: &str) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    #[cfg(feature = "alloc")]
    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Self::try_from(value)
    }

    fn validate(&self) -> Result<(), ParseError> {
        Ok(())
    }

    fn as_raw(&self) -> &str {
        self.as_str()
    }
}
