use std::fmt;

use serde::{Deserialize, Serialize};

use crate::bucket::WriterMode;
use crate::constants::{
    CONTROL_DIRECT_TOKEN, CONTROL_PREFIX, CONTROL_RECEIPT_TOKEN, CONTROL_WRITER_TOKEN, KV_KEY_MAX_BYTES,
    TOKEN_SEPARATOR,
};
use crate::holder::{HolderId, HolderIdError};
use crate::key::{KeyError, PresenceKey};
use crate::position::OperationId;
use crate::shard::{ShardCount, ShardError, ViewShard};
use crate::topic::{Topic, TopicError};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KvKey(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryKey {
    topic: Topic,
    key: PresenceKey,
    holder: HolderId,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KvKeyError {
    #[error("kv key is {len} bytes, the limit is {max}")]
    TooLong { len: usize, max: usize },
    #[error("kv key does not have shard, key, holder and topic tokens")]
    MissingTokens,
    #[error(transparent)]
    Shard(#[from] ShardError),
    #[error(transparent)]
    Key(#[from] KeyError),
    #[error(transparent)]
    Holder(#[from] HolderIdError),
    #[error(transparent)]
    Topic(#[from] TopicError),
    #[error("kv key shard {found} does not match topic shard {expected}")]
    ShardMismatch { found: u16, expected: u16 },
}

impl KvKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for KvKey {
    type Error = KvKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > KV_KEY_MAX_BYTES {
            return Err(KvKeyError::TooLong {
                len: value.len(),
                max: KV_KEY_MAX_BYTES,
            });
        }
        Ok(Self(value))
    }
}

impl From<KvKey> for String {
    fn from(key: KvKey) -> Self {
        key.0
    }
}

impl fmt::Display for KvKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl EntryKey {
    pub fn new(topic: Topic, key: PresenceKey, holder: HolderId) -> Self {
        Self { topic, key, holder }
    }

    pub fn topic(&self) -> &Topic {
        &self.topic
    }

    pub fn key(&self) -> &PresenceKey {
        &self.key
    }

    pub fn holder(&self) -> &HolderId {
        &self.holder
    }

    pub fn shard(&self, count: ShardCount) -> ViewShard {
        ViewShard::of(&self.topic, count)
    }

    pub fn encode(&self, count: ShardCount) -> Result<KvKey, KvKeyError> {
        let shard = count.token(self.shard(count));
        let holder = self.holder.to_string();
        let parts = [shard.as_str(), self.key.token(), holder.as_str(), self.topic.tokens()];
        let mut out = String::with_capacity(parts.iter().map(|p| p.len() + 1).sum());
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                out.push(TOKEN_SEPARATOR);
            }
            out.push_str(part);
        }
        KvKey::try_from(out)
    }

    pub fn decode(kv_key: &KvKey, count: ShardCount) -> Result<Self, KvKeyError> {
        let mut parts = kv_key.as_str().splitn(4, TOKEN_SEPARATOR);
        let (Some(shard), Some(key), Some(holder), Some(topic)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(KvKeyError::MissingTokens);
        };
        let shard = ViewShard::from(count.parse_token(shard)?);
        let entry = Self {
            topic: Topic::from_tokens(topic)?,
            key: PresenceKey::from_token(key)?,
            holder: holder.parse()?,
        };
        let expected = entry.shard(count);
        if shard != expected {
            return Err(KvKeyError::ShardMismatch {
                found: shard.shard().index(),
                expected: expected.shard().index(),
            });
        }
        Ok(entry)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuthorityId {
    Direct(HolderId),
    Managed(PresenceKey),
}

impl AuthorityId {
    pub fn mode(&self) -> WriterMode {
        match self {
            Self::Direct(_) => WriterMode::Direct,
            Self::Managed(_) => WriterMode::Managed,
        }
    }

    fn token(&self) -> String {
        match self {
            Self::Direct(holder) => holder.to_string(),
            Self::Managed(key) => key.token().to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ControlKey {
    WriterGuard(PresenceKey),
    DirectGuard(HolderId),
    Receipt(AuthorityId, OperationId),
}

impl ControlKey {
    pub fn encode(&self) -> Result<KvKey, KvKeyError> {
        let tokens = match self {
            Self::WriterGuard(key) => vec![CONTROL_WRITER_TOKEN.to_owned(), key.token().to_owned()],
            Self::DirectGuard(holder) => vec![CONTROL_DIRECT_TOKEN.to_owned(), holder.to_string()],
            Self::Receipt(authority, operation) => vec![
                CONTROL_RECEIPT_TOKEN.to_owned(),
                authority.mode().as_str().to_owned(),
                authority.token(),
                operation.to_string(),
            ],
        };
        let mut out = String::from(CONTROL_PREFIX);
        for token in tokens {
            out.push(TOKEN_SEPARATOR);
            out.push_str(&token);
        }
        KvKey::try_from(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{KEY_MAX_RAW_BYTES, TOPIC_SEGMENT_MAX_RAW_BYTES};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn entry(topic: &str, key: &str) -> Result<EntryKey, Box<dyn std::error::Error>> {
        Ok(EntryKey::new(
            topic.parse()?,
            key.parse()?,
            "q3V9hX0bS2mWf1ZkR8aT1A".parse()?,
        ))
    }

    #[test]
    fn encodes_design_example() -> TestResult {
        let kv = entry("room:lobby", "ana")?.encode(ShardCount::DEFAULT)?;
        assert_eq!(kv.as_str(), "s56.ana.q3V9hX0bS2mWf1ZkR8aT1A.room.lobby");
        Ok(())
    }

    #[test]
    fn round_trips() -> TestResult {
        for (topic, key) in [("room:lobby", "ana@x.io"), ("room:", "="), ("a:b:c", "j.d")] {
            let original = entry(topic, key)?;
            let kv = original.encode(ShardCount::DEFAULT)?;
            assert_eq!(EntryKey::decode(&kv, ShardCount::DEFAULT)?, original);
        }
        Ok(())
    }

    #[test]
    fn worst_case_fits_limit() -> TestResult {
        let segment = "@".repeat(TOPIC_SEGMENT_MAX_RAW_BYTES);
        let topic = [segment.as_str(), &"@".repeat(TOPIC_SEGMENT_MAX_RAW_BYTES - 1)].join(":");
        let kv = entry(&topic, &"@".repeat(KEY_MAX_RAW_BYTES))?.encode(ShardCount::try_from(1024)?)?;
        assert!(kv.as_str().len() <= KV_KEY_MAX_BYTES);
        Ok(())
    }

    #[test]
    fn encodes_control_keys_outside_every_shard() -> TestResult {
        let holder: HolderId = "q3V9hX0bS2mWf1ZkR8aT1A".parse()?;
        let operation: OperationId = "AAAAAAAAAAAAAAAAAAAAAA".parse()?;
        let key: PresenceKey = "ana@x.io".parse()?;
        let cases = [
            (ControlKey::WriterGuard(key.clone()), "ctl.writer.ana=40x=2Eio"),
            (ControlKey::DirectGuard(holder), "ctl.direct.q3V9hX0bS2mWf1ZkR8aT1A"),
            (
                ControlKey::Receipt(AuthorityId::Direct(holder), operation),
                "ctl.receipt.direct.q3V9hX0bS2mWf1ZkR8aT1A.AAAAAAAAAAAAAAAAAAAAAA",
            ),
            (
                ControlKey::Receipt(AuthorityId::Managed(key), operation),
                "ctl.receipt.managed.ana=40x=2Eio.AAAAAAAAAAAAAAAAAAAAAA",
            ),
        ];
        for (control, expected) in cases {
            let kv = control.encode()?;
            assert_eq!(kv.as_str(), expected);
            assert!(EntryKey::decode(&kv, ShardCount::DEFAULT).is_err());
        }
        Ok(())
    }

    #[test]
    fn rejects_mismatched_shard() -> TestResult {
        let kv = KvKey::try_from("s01.ana.q3V9hX0bS2mWf1ZkR8aT1A.room.lobby".to_owned())?;
        assert_eq!(
            EntryKey::decode(&kv, ShardCount::DEFAULT),
            Err(KvKeyError::ShardMismatch { found: 1, expected: 56 })
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_keys() -> TestResult {
        for bad in [
            "s56.ana.q3V9hX0bS2mWf1ZkR8aT1A",
            "s56.ana.short.room.lobby",
            "s56.a=2e.q3V9hX0bS2mWf1ZkR8aT1A.room.lobby",
            "s56..q3V9hX0bS2mWf1ZkR8aT1A.room.lobby",
            "s056.ana.q3V9hX0bS2mWf1ZkR8aT1A.room.lobby",
        ] {
            let kv = KvKey::try_from(bad.to_owned())?;
            assert!(EntryKey::decode(&kv, ShardCount::DEFAULT).is_err(), "{bad}");
        }
        assert!(KvKey::try_from("x".repeat(KV_KEY_MAX_BYTES + 1)).is_err());
        Ok(())
    }
}
