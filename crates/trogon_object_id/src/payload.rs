use std::fmt;
use std::str::FromStr;

use crate::{ParseError, MAX_VALUE_BYTES};

/// A trusted payload codec and its immutable raw representation.
///
/// Implementations must reject invalid values on every construction path and
/// keep validation deterministic. Delegate parsing to standard `TryFrom`
/// implementations on immutable payload newtypes.
/// Payloads with interior mutation can invalidate an identifier's guarantees.
pub trait Payload: Sized + fmt::Display {
    /// The borrowed representation used when storage already establishes the domain.
    type Raw: ?Sized + fmt::Display;

    /// Parses unprefixed text and returns only values satisfying this codec's rules.
    ///
    /// An implementation may normalize text, but its [`fmt::Display`] output
    /// must remain a valid representation accepted by this codec.
    fn parse(value: &str) -> Result<Self, ParseError>;
    /// Parses owned unprefixed text, with the same validation as [`Self::parse`].
    ///
    /// The default borrows the input and calls `parse`; codecs that store text
    /// can override it to retain the existing allocation.
    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Self::parse(&value)
    }
    /// Rejects invalid typed or generated values before an identifier wraps them.
    ///
    /// Immutable payloads whose constructors establish the invariant may rely
    /// on that invariant instead of repeating validation.
    fn validate(&self) -> Result<(), ParseError>;
    /// Borrows the raw value without adding the identifier's prefix or separator.
    fn as_raw(&self) -> &Self::Raw;
}

/// Validated, spelling-preserving owned text.
///
/// Construction through `TryFrom<&str>` or `TryFrom<String>` requires nonempty
/// text, at most [`MAX_VALUE_BYTES`] UTF-8 bytes and no Unicode control characters.
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

fn check_text(value: &str) -> Result<(), ParseError> {
    if value.is_empty() {
        return Err(ParseError::EmptyValue);
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err(ParseError::ValueTooLong);
    }
    if value.chars().any(char::is_control) {
        return Err(ParseError::ControlCharacter);
    }
    Ok(())
}

fn check_integer(value: &str) -> Result<(), ParseError> {
    check_text(value)?;
    check_integer_syntax(value)
}

fn check_integer_syntax(value: &str) -> Result<(), ParseError> {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ParseError::InvalidValue)
    }
}

#[cfg(feature = "uuid")]
fn parse_uuid(value: &str) -> Result<uuid::Uuid, ParseError> {
    check_text(value)?;
    parse_uuid_syntax(value)
}

#[cfg(feature = "uuid")]
fn parse_uuid_syntax(value: &str) -> Result<uuid::Uuid, ParseError> {
    if value.len() != 36 {
        return Err(ParseError::InvalidValue);
    }
    uuid::Uuid::try_from(value).map_err(|_| ParseError::InvalidValue)
}

#[cfg(feature = "uuid")]
impl Payload for uuid::Uuid {
    type Raw = Self;

    fn parse(value: &str) -> Result<Self, ParseError> {
        parse_uuid(value)
    }

    fn validate(&self) -> Result<(), ParseError> {
        Ok(())
    }

    fn as_raw(&self) -> &Self {
        self
    }
}
