#![forbid(unsafe_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid identifier")]
pub struct ParseError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn try_from_raw(value: &str) -> Result<Self, ParseError> {
        Ok(Self(value.to_owned()))
    }

    pub fn as_raw(&self) -> &str {
        &self.0
    }
}
