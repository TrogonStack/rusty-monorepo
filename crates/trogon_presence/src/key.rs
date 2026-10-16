use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::codec::{self, CodecError};
use crate::constants::{KEY_MAX_ESCAPED_BYTES, KEY_MAX_RAW_BYTES};
use crate::error_code::ErrorCode;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PresenceKey {
    raw: String,
    token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("presence key is empty")]
    Empty,
    #[error("presence key is {len} bytes, the limit is {max}")]
    TooLong { len: usize, max: usize },
    #[error("presence key token is not canonical: {0}")]
    InvalidToken(#[from] CodecError),
    #[error("presence key token does not decode to UTF-8")]
    NotUtf8,
}

impl KeyError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::TooLong { .. } => ErrorCode::KeyTooLong,
            Self::Empty | Self::InvalidToken(_) | Self::NotUtf8 => ErrorCode::InvalidKey,
        }
    }
}

impl PresenceKey {
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn from_token(token: &str) -> Result<Self, KeyError> {
        if token.len() > KEY_MAX_ESCAPED_BYTES {
            return Err(KeyError::TooLong {
                len: token.len(),
                max: KEY_MAX_ESCAPED_BYTES,
            });
        }
        let raw = String::from_utf8(codec::decode(token)?).map_err(|_| KeyError::NotUtf8)?;
        Self::try_from(raw)
    }
}

impl TryFrom<String> for PresenceKey {
    type Error = KeyError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() {
            return Err(KeyError::Empty);
        }
        if raw.len() > KEY_MAX_RAW_BYTES {
            return Err(KeyError::TooLong {
                len: raw.len(),
                max: KEY_MAX_RAW_BYTES,
            });
        }
        let token = codec::encode(raw.as_bytes()).into_owned();
        if token.len() > KEY_MAX_ESCAPED_BYTES {
            return Err(KeyError::TooLong {
                len: token.len(),
                max: KEY_MAX_ESCAPED_BYTES,
            });
        }
        Ok(Self { raw, token })
    }
}

impl FromStr for PresenceKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<PresenceKey> for String {
    fn from(key: PresenceKey) -> Self {
        key.raw
    }
}

impl fmt::Display for PresenceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_design_example() -> Result<(), KeyError> {
        let key: PresenceKey = "ana@x.io".parse()?;
        assert_eq!(key.token(), "ana=40x=2Eio");
        assert_eq!(PresenceKey::from_token(key.token())?, key);
        Ok(())
    }

    #[test]
    fn enforces_limits() {
        assert_eq!(
            "".parse::<PresenceKey>().map_err(|e| e.code()),
            Err(ErrorCode::InvalidKey)
        );
        assert!("a".repeat(KEY_MAX_RAW_BYTES).parse::<PresenceKey>().is_ok());
        assert!("@".repeat(KEY_MAX_RAW_BYTES).parse::<PresenceKey>().is_ok());
        assert_eq!(
            "a".repeat(KEY_MAX_RAW_BYTES + 1)
                .parse::<PresenceKey>()
                .map_err(|e| e.code()),
            Err(ErrorCode::KeyTooLong)
        );
    }

    #[test]
    fn rejects_bad_tokens() {
        assert_eq!(
            PresenceKey::from_token("=").map_err(|e| e.code()),
            Err(ErrorCode::InvalidKey)
        );
        assert_eq!(PresenceKey::from_token("=C3"), Err(KeyError::NotUtf8));
        assert!(PresenceKey::from_token("ana=40x=2eio").is_err());
    }
}
