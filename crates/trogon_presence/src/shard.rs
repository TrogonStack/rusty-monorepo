use std::fmt;

use serde::{Deserialize, Serialize};

use crate::constants::{FNV1A64_OFFSET_BASIS, FNV1A64_PRIME, SHARD_COUNT_MAX, SHARD_COUNT_MIN, SHARD_TOKEN_PREFIX};
use crate::key::PresenceKey;
use crate::topic::Topic;

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV1A64_OFFSET_BASIS, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV1A64_PRIME)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Shard(u16);

impl Shard {
    pub fn index(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ViewShard(Shard);

impl ViewShard {
    pub fn of(topic: &Topic, count: ShardCount) -> Self {
        Self(count.masked(topic.as_str().as_bytes()))
    }

    pub fn shard(self) -> Shard {
        self.0
    }
}

impl From<Shard> for ViewShard {
    fn from(shard: Shard) -> Self {
        Self(shard)
    }
}

impl From<ViewShard> for Shard {
    fn from(shard: ViewShard) -> Self {
        shard.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WriterShard(Shard);

impl WriterShard {
    pub fn of(key: &PresenceKey, count: ShardCount) -> Self {
        Self(count.masked(key.as_str().as_bytes()))
    }

    pub fn shard(self) -> Shard {
        self.0
    }
}

impl From<Shard> for WriterShard {
    fn from(shard: Shard) -> Self {
        Self(shard)
    }
}

impl From<WriterShard> for Shard {
    fn from(shard: WriterShard) -> Self {
        shard.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct ShardCount(u16);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShardError {
    #[error("shard count {0} must be a power of two between 64 and 1024")]
    InvalidCount(u16),
    #[error("shard {index} is out of range for {count} shards")]
    OutOfRange { index: u16, count: u16 },
    #[error("shard token {0:?} is malformed for this shard count")]
    MalformedToken(String),
}

impl ShardCount {
    pub const DEFAULT: Self = Self(SHARD_COUNT_MIN);

    pub fn get(self) -> u16 {
        self.0
    }

    pub fn token_width(self) -> usize {
        (self.0 - 1).to_string().len()
    }

    pub fn shard(self, index: u16) -> Result<Shard, ShardError> {
        if index < self.0 {
            Ok(Shard(index))
        } else {
            Err(ShardError::OutOfRange { index, count: self.0 })
        }
    }

    pub fn shards(self) -> impl Iterator<Item = Shard> {
        (0..self.0).map(Shard)
    }

    fn masked(self, bytes: &[u8]) -> Shard {
        let index = fnv1a64(bytes) & u64::from(self.0 - 1);
        Shard(index as u16)
    }

    pub fn token(self, shard: impl Into<Shard>) -> String {
        format!(
            "{SHARD_TOKEN_PREFIX}{:0width$}",
            shard.into().0,
            width = self.token_width()
        )
    }

    pub fn parse_token(self, token: &str) -> Result<Shard, ShardError> {
        let malformed = || ShardError::MalformedToken(token.to_owned());
        let digits = token.strip_prefix(SHARD_TOKEN_PREFIX).ok_or_else(malformed)?;
        if digits.len() != self.token_width() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(malformed());
        }
        let index = digits.parse::<u16>().map_err(|_| malformed())?;
        self.shard(index)
    }
}

impl Default for ShardCount {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u16> for ShardCount {
    type Error = ShardError;

    fn try_from(count: u16) -> Result<Self, Self::Error> {
        if count.is_power_of_two() && (SHARD_COUNT_MIN..=SHARD_COUNT_MAX).contains(&count) {
            Ok(Self(count))
        } else {
            Err(ShardError::InvalidCount(count))
        }
    }
}

impl From<ShardCount> for u16 {
    fn from(count: ShardCount) -> Self {
        count.0
    }
}

impl fmt::Display for ShardCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"room:lobby"), 0x138b_b1f5_6175_8538);
        assert_eq!(fnv1a64("room:jos\u{e9}".as_bytes()), 0x2fe7_e9fc_55b3_4e80);
    }

    #[test]
    fn shards_topic_on_raw_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let topic: Topic = "room:lobby".parse()?;
        let default = ShardCount::DEFAULT;
        assert_eq!(default.token(ViewShard::of(&topic, default)), "s56");
        let large = ShardCount::try_from(1024)?;
        assert_eq!(large.token(ViewShard::of(&topic, large)), "s0312");
        Ok(())
    }

    #[test]
    fn writer_shard_hashes_the_raw_key() -> Result<(), Box<dyn std::error::Error>> {
        let count = ShardCount::DEFAULT;
        let key: PresenceKey = "room:lobby".parse()?;
        let topic: Topic = "room:lobby".parse()?;
        assert_eq!(
            WriterShard::of(&key, count).shard(),
            ViewShard::of(&topic, count).shard()
        );
        let ana: PresenceKey = "ana".parse()?;
        let expected = (fnv1a64(b"ana") & 63) as u16;
        assert_eq!(WriterShard::of(&ana, count).shard().index(), expected);
        Ok(())
    }

    #[test]
    fn writer_and_view_shards_differ_for_the_same_entry() -> Result<(), Box<dyn std::error::Error>> {
        let count = ShardCount::DEFAULT;
        let key: PresenceKey = "ana".parse()?;
        let topic: Topic = "room:lobby".parse()?;
        let writer = WriterShard::of(&key, count);
        let view = ViewShard::of(&topic, count);
        assert_ne!(writer.shard(), view.shard());
        assert_eq!(count.token(view), "s56");
        assert_eq!(count.token(writer), format!("s{:02}", fnv1a64(b"ana") & 63));
        Ok(())
    }

    #[test]
    fn validates_count() {
        for bad in [0, 1, 32, 63, 65, 96, 2048] {
            assert_eq!(ShardCount::try_from(bad), Err(ShardError::InvalidCount(bad)));
        }
        for good in [64, 128, 256, 512, 1024] {
            assert!(ShardCount::try_from(good).is_ok());
        }
    }

    #[test]
    fn token_width_follows_count() -> Result<(), ShardError> {
        assert_eq!(ShardCount::try_from(64)?.token_width(), 2);
        assert_eq!(ShardCount::try_from(128)?.token_width(), 3);
        assert_eq!(ShardCount::try_from(1024)?.token_width(), 4);
        let count = ShardCount::try_from(128)?;
        assert_eq!(count.token(count.shard(7)?), "s007");
        Ok(())
    }

    #[test]
    fn parses_tokens_strictly() -> Result<(), ShardError> {
        let count = ShardCount::DEFAULT;
        assert_eq!(count.parse_token("s07")?, count.shard(7)?);
        for bad in ["s7", "s007", "07", "t07", "s+7", "s64", "s0x"] {
            assert!(count.parse_token(bad).is_err(), "{bad}");
        }
        Ok(())
    }
}
