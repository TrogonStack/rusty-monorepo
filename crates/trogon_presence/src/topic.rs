use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::codec::{self, CodecError};
use crate::constants::{
    TOKEN_SEPARATOR, TOPIC_MAX_ESCAPED_BYTES, TOPIC_MAX_RAW_BYTES, TOPIC_SEGMENT_MAX_ESCAPED_BYTES,
    TOPIC_SEGMENT_MAX_RAW_BYTES, TOPIC_SEPARATOR,
};
use crate::error_code::ErrorCode;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Topic {
    raw: String,
    tokens: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopicError {
    #[error("topic is empty")]
    Empty,
    #[error("topic is {len} bytes, the limit is {max}")]
    TooLong { len: usize, max: usize },
    #[error("topic segment {index} is {len} bytes, the limit is {max}")]
    SegmentTooLong { index: usize, len: usize, max: usize },
    #[error("topic token {index} is not canonical: {source}")]
    InvalidToken { index: usize, source: CodecError },
    #[error("topic token {index} decodes to a segment containing the separator")]
    SeparatorInToken { index: usize },
    #[error("topic tokens do not decode to UTF-8")]
    NotUtf8,
}

impl TopicError {
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::TooLong { .. } | Self::SegmentTooLong { .. } => ErrorCode::TopicTooLong,
            Self::Empty | Self::InvalidToken { .. } | Self::SeparatorInToken { .. } | Self::NotUtf8 => {
                ErrorCode::InvalidTopic
            }
        }
    }
}

impl Topic {
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn tokens(&self) -> &str {
        &self.tokens
    }

    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.raw.split(TOPIC_SEPARATOR)
    }

    pub fn from_tokens(tokens: &str) -> Result<Self, TopicError> {
        if tokens.len() > TOPIC_MAX_ESCAPED_BYTES {
            return Err(TopicError::TooLong {
                len: tokens.len(),
                max: TOPIC_MAX_ESCAPED_BYTES,
            });
        }
        let mut raw = Vec::with_capacity(tokens.len());
        for (index, token) in tokens.split(TOKEN_SEPARATOR).enumerate() {
            let segment = codec::decode(token).map_err(|source| TopicError::InvalidToken { index, source })?;
            if segment.contains(&(TOPIC_SEPARATOR as u8)) {
                return Err(TopicError::SeparatorInToken { index });
            }
            if index > 0 {
                raw.push(TOPIC_SEPARATOR as u8);
            }
            raw.extend_from_slice(&segment);
        }
        let raw = String::from_utf8(raw).map_err(|_| TopicError::NotUtf8)?;
        Self::try_from(raw)
    }
}

impl TryFrom<String> for Topic {
    type Error = TopicError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() {
            return Err(TopicError::Empty);
        }
        if raw.len() > TOPIC_MAX_RAW_BYTES {
            return Err(TopicError::TooLong {
                len: raw.len(),
                max: TOPIC_MAX_RAW_BYTES,
            });
        }
        let mut tokens = String::with_capacity(raw.len());
        for (index, segment) in raw.split(TOPIC_SEPARATOR).enumerate() {
            if segment.len() > TOPIC_SEGMENT_MAX_RAW_BYTES {
                return Err(TopicError::SegmentTooLong {
                    index,
                    len: segment.len(),
                    max: TOPIC_SEGMENT_MAX_RAW_BYTES,
                });
            }
            let token = codec::encode(segment.as_bytes());
            if token.len() > TOPIC_SEGMENT_MAX_ESCAPED_BYTES {
                return Err(TopicError::SegmentTooLong {
                    index,
                    len: token.len(),
                    max: TOPIC_SEGMENT_MAX_ESCAPED_BYTES,
                });
            }
            if index > 0 {
                tokens.push(TOKEN_SEPARATOR);
            }
            tokens.push_str(&token);
        }
        if tokens.len() > TOPIC_MAX_ESCAPED_BYTES {
            return Err(TopicError::TooLong {
                len: tokens.len(),
                max: TOPIC_MAX_ESCAPED_BYTES,
            });
        }
        Ok(Self { raw, tokens })
    }
}

impl FromStr for Topic {
    type Err = TopicError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl From<Topic> for String {
    fn from(topic: Topic) -> Self {
        topic.raw
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(raw: &str) -> Result<Topic, TopicError> {
        raw.parse()
    }

    #[test]
    fn maps_segments_to_tokens() -> Result<(), TopicError> {
        assert_eq!(topic("room:lobby")?.tokens(), "room.lobby");
        assert_eq!(topic("room:")?.tokens(), "room.=");
        assert_eq!(topic("room:jos\u{e9}")?.tokens(), "room.jos=C3=A9");
        assert_eq!(topic(":")?.tokens(), "=.=");
        assert_eq!(topic("a.b:c")?.tokens(), "a=2Eb.c");
        Ok(())
    }

    #[test]
    fn exposes_raw_segments() -> Result<(), TopicError> {
        let t = topic("room:lobby:")?;
        assert_eq!(t.segments().collect::<Vec<_>>(), ["room", "lobby", ""]);
        Ok(())
    }

    #[test]
    fn round_trips_through_tokens() -> Result<(), TopicError> {
        for raw in ["room:lobby", "room:", ":", "a.b*>:c d", "room:jos\u{e9}"] {
            let t = topic(raw)?;
            assert_eq!(Topic::from_tokens(t.tokens())?, t);
        }
        Ok(())
    }

    #[test]
    fn rejects_bad_tokens() {
        assert_eq!(
            Topic::from_tokens("room.=3A").map_err(|e| e.code()),
            Err(ErrorCode::InvalidTopic)
        );
        assert_eq!(
            Topic::from_tokens("room..lobby").map_err(|e| e.code()),
            Err(ErrorCode::InvalidTopic)
        );
        assert_eq!(Topic::from_tokens("=FF"), Err(TopicError::NotUtf8));
    }

    #[test]
    fn enforces_limits() {
        assert_eq!(topic("").map_err(|e| e.code()), Err(ErrorCode::InvalidTopic));
        let long_segment = "a".repeat(TOPIC_SEGMENT_MAX_RAW_BYTES + 1);
        assert_eq!(topic(&long_segment).map_err(|e| e.code()), Err(ErrorCode::TopicTooLong));
        let at_limit = ["a".repeat(127), "b".repeat(128)].join(":");
        assert!(topic(&at_limit).is_ok());
        assert_eq!(
            topic(&format!("{at_limit}x")).map_err(|e| e.code()),
            Err(ErrorCode::TopicTooLong)
        );
    }

    #[test]
    fn serializes_as_raw_string() -> Result<(), serde_json::Error> {
        let parsed: Topic = serde_json::from_str("\"room:lobby\"")?;
        assert_eq!(parsed.tokens(), "room.lobby");
        assert_eq!(serde_json::to_string(&parsed)?, "\"room:lobby\"");
        assert!(serde_json::from_str::<Topic>("\"\"").is_err());
        Ok(())
    }
}
