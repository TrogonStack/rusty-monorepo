use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;

use crate::validators;
use crate::{ParseError, Validator, MAX_VALUE_BYTES};

/// A trusted payload codec and its immutable raw representation.
///
/// Implementations must reject invalid values on every construction path and
/// keep validation deterministic. Prefer `Text<V>` for custom text rules.
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
/// Text must be nonempty, contain at most [`MAX_VALUE_BYTES`] UTF-8 bytes and
/// contain no Unicode control characters before the custom validator runs.
/// Spelling is never normalized, including case and signed decimal padding.
#[repr(transparent)]
pub struct Text<V: Validator = validators::Any> {
    value: String,
    validator: PhantomData<fn() -> V>,
}

impl<V: Validator> Text<V> {
    /// Validates borrowed text, then allocates an owned copy without normalization.
    ///
    /// Returns [`ParseError::EmptyValue`], [`ParseError::ValueTooLong`] or
    /// [`ParseError::ControlCharacter`] for the shared constraints, or the error
    /// returned by `V` for an application-specific rule.
    pub fn try_from_raw(value: &str) -> Result<Self, ParseError> {
        validate_text::<V>(value)?;
        Ok(Self {
            value: value.to_owned(),
            validator: PhantomData,
        })
    }

    /// Applies the same checks as [`Self::try_from_raw`] while retaining the allocation.
    pub fn try_from_owned(value: String) -> Result<Self, ParseError> {
        validate_text::<V>(&value)?;
        Ok(Self {
            value,
            validator: PhantomData,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.value
    }
    pub fn into_string(self) -> String {
        self.value
    }
}

fn validate_text<V: Validator>(value: &str) -> Result<(), ParseError> {
    if value.is_empty() {
        return Err(ParseError::EmptyValue);
    }
    if value.len() > MAX_VALUE_BYTES {
        return Err(ParseError::ValueTooLong);
    }
    if value.chars().any(char::is_control) {
        return Err(ParseError::ControlCharacter);
    }
    V::validate(value)
}

impl<V: Validator> Payload for Text<V> {
    type Raw = str;
    fn parse(value: &str) -> Result<Self, ParseError> {
        Self::try_from_raw(value)
    }
    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Self::try_from_owned(value)
    }
    fn validate(&self) -> Result<(), ParseError> {
        // Checked constructors and immutable storage preserve the validator's guarantee.
        Ok(())
    }
    fn as_raw(&self) -> &str {
        self.as_str()
    }
}

impl<V: Validator> fmt::Display for Text<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.value)
    }
}
impl<V: Validator> fmt::Debug for Text<V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(formatter)
    }
}
impl<V: Validator> Clone for Text<V> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            validator: PhantomData,
        }
    }
}
impl<V: Validator> PartialEq for Text<V> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}
impl<V: Validator> Eq for Text<V> {}
impl<V: Validator> PartialOrd for Text<V> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<V: Validator> Ord for Text<V> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.value.cmp(&other.value)
    }
}
impl<V: Validator> Hash for Text<V> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

#[cfg(feature = "uuid")]
impl Payload for uuid::Uuid {
    type Raw = Self;
    fn parse(value: &str) -> Result<Self, ParseError> {
        if value.is_empty() {
            return Err(ParseError::EmptyValue);
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(ParseError::ValueTooLong);
        }
        if value.chars().any(char::is_control) {
            return Err(ParseError::ControlCharacter);
        }
        if value.len() != 36 {
            return Err(ParseError::InvalidValue);
        }
        uuid::Uuid::parse_str(value).map_err(|_| ParseError::InvalidValue)
    }
    fn validate(&self) -> Result<(), ParseError> {
        Ok(())
    }
    fn as_raw(&self) -> &Self {
        self
    }
}
