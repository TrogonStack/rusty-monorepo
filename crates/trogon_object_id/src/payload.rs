use core::fmt;

use crate::ParseError;
#[cfg(any(feature = "alloc", feature = "uuid"))]
use crate::MAX_VALUE_BYTES;

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
    #[cfg(feature = "alloc")]
    fn parse_owned(value: alloc::string::String) -> Result<Self, ParseError> {
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

#[cfg(feature = "alloc")]
mod owned;
#[cfg(all(feature = "alloc", feature = "uuid"))]
pub use owned::UuidText;
#[cfg(feature = "alloc")]
pub use owned::{IntegerText, Text};

#[cfg(any(feature = "alloc", feature = "uuid"))]
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

#[cfg(feature = "alloc")]
fn check_integer(value: &str) -> Result<(), ParseError> {
    check_text(value)?;
    check_integer_syntax(value)
}

#[cfg(feature = "alloc")]
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
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        discarded_error_source,
        reason = "UUID parse failures intentionally map to a Copy error that retains no rejected identifier text."
    )
)]
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
