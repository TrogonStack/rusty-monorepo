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
    type Raw: ?Sized + fmt::Display;

    fn parse(value: &str) -> Result<Self, ParseError>;
    fn parse_owned(value: String) -> Result<Self, ParseError> {
        Self::parse(&value)
    }
    fn validate(&self) -> Result<(), ParseError>;
    fn as_raw(&self) -> &Self::Raw;
}

/// Validated, spelling-preserving owned text.
#[repr(transparent)]
pub struct Text<V: Validator = validators::Any> {
    value: String,
    validator: PhantomData<fn() -> V>,
}

impl<V: Validator> Text<V> {
    pub fn try_from_raw(value: &str) -> Result<Self, ParseError> {
        validate_text::<V>(value)?;
        Ok(Self {
            value: value.to_owned(),
            validator: PhantomData,
        })
    }

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
        validate_text::<V>(&self.value)
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
