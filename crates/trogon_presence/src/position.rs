use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::constants::RANDOM_ID_BYTES;
use crate::entropy::{decode_id, encode_id, random_id_bytes, EntropyError};
use crate::revision::EntryRevision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{0} would leave the u64 range")]
pub struct PositionOverflow(pub(crate) &'static str);

pub(crate) mod decimal_u64 {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let text = String::deserialize(deserializer)?;
        parse(&text).ok_or_else(|| D::Error::custom(format!("{text:?} is not a canonical decimal u64")))
    }

    pub(crate) fn parse(text: &str) -> Option<u64> {
        let canonical =
            !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) && (text == "0" || !text.starts_with('0'));
        if canonical {
            text.parse().ok()
        } else {
            None
        }
    }
}

macro_rules! sequence {
    ($name:ident, $what:literal) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(#[serde(with = "decimal_u64")] u64);

        impl $name {
            pub const FIRST: Self = Self(1);

            pub fn get(self) -> u64 {
                self.0
            }

            pub fn checked_add(self, delta: u64) -> Result<Self, PositionOverflow> {
                self.0.checked_add(delta).map(Self).ok_or(PositionOverflow($what))
            }

            pub fn next(self) -> Result<Self, PositionOverflow> {
                self.checked_add(1)
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

sequence!(MutationSequence, "mutation sequence");
sequence!(DiffSequence, "diff sequence");
sequence!(ViewIncarnation, "view incarnation");

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0} must be 16 bytes encoded as 22 characters of unpadded base64url")]
pub struct OpaqueIdError(&'static str);

macro_rules! opaque_id {
    ($name:ident, $what:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name([u8; RANDOM_ID_BYTES]);

        impl $name {
            pub fn generate() -> Result<Self, EntropyError> {
                random_id_bytes().map(Self)
            }

            pub fn as_bytes(&self) -> &[u8; RANDOM_ID_BYTES] {
                &self.0
            }
        }

        impl From<[u8; RANDOM_ID_BYTES]> for $name {
            fn from(bytes: [u8; RANDOM_ID_BYTES]) -> Self {
                Self(bytes)
            }
        }

        impl FromStr for $name {
            type Err = OpaqueIdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                decode_id(s).map(Self).ok_or(OpaqueIdError($what))
            }
        }

        impl TryFrom<String> for $name {
            type Error = OpaqueIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                value.parse()
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                encode_id(&id.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&encode_id(&self.0))
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self)
            }
        }
    };
}

opaque_id!(LifetimeId, "lifetime id");
opaque_id!(OperationId, "operation id");
opaque_id!(OwnerId, "owner id");
opaque_id!(StreamGeneration, "stream generation");
opaque_id!(BatchId, "batch id");
opaque_id!(LocalViewId, "local view id");
opaque_id!(ConnectionId, "connection id");
opaque_id!(SnapshotId, "snapshot id");
opaque_id!(RequestId, "request id");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OwnerEpoch {
    acquired: EntryRevision,
    owner: OwnerId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochOrder {
    Older,
    Same,
    Newer,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("owner epochs from generation {left} and generation {right} are not comparable")]
pub struct CrossGenerationEpochs {
    left: StreamGeneration,
    right: StreamGeneration,
}

impl OwnerEpoch {
    pub fn new(acquired: EntryRevision, owner: OwnerId) -> Self {
        Self { acquired, owner }
    }

    pub fn acquired(self) -> EntryRevision {
        self.acquired
    }

    pub fn owner(self) -> OwnerId {
        self.owner
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenerationEpoch {
    generation: StreamGeneration,
    epoch: OwnerEpoch,
}

impl GenerationEpoch {
    pub fn new(generation: StreamGeneration, epoch: OwnerEpoch) -> Self {
        Self { generation, epoch }
    }

    pub fn generation(self) -> StreamGeneration {
        self.generation
    }

    pub fn epoch(self) -> OwnerEpoch {
        self.epoch
    }

    pub fn compare(self, other: Self) -> Result<EpochOrder, CrossGenerationEpochs> {
        if self.generation != other.generation {
            return Err(CrossGenerationEpochs {
                left: self.generation,
                right: other.generation,
            });
        }
        Ok(match self.epoch.acquired.cmp(&other.epoch.acquired) {
            Ordering::Less => EpochOrder::Older,
            Ordering::Greater => EpochOrder::Newer,
            Ordering::Equal if self.epoch.owner == other.epoch.owner => EpochOrder::Same,
            Ordering::Equal => EpochOrder::Conflict,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn round_trip<T>(value: T, expected: &str) -> TestResult
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + fmt::Debug,
    {
        let json = serde_json::to_string(&value)?;
        assert_eq!(json, format!("\"{expected}\""));
        let back: T = serde_json::from_str(&json)?;
        assert_eq!(back, value);
        Ok(())
    }

    #[test]
    fn every_u64_position_round_trips_as_a_decimal_string() -> TestResult {
        for raw in [0, 1, 42, u64::MAX - 1, u64::MAX] {
            let text = raw.to_string();
            round_trip(EntryRevision::from(raw), &text)?;
            round_trip(MutationSequence::from(raw), &text)?;
            round_trip(DiffSequence::from(raw), &text)?;
        }
        Ok(())
    }

    #[test]
    fn decimal_positions_reject_numbers_and_non_canonical_text() {
        for bad in [
            "1",
            "\"\"",
            "\"01\"",
            "\"-1\"",
            "\"+1\"",
            "\"1.0\"",
            "\"18446744073709551616\"",
        ] {
            assert!(serde_json::from_str::<EntryRevision>(bad).is_err(), "{bad}");
            assert!(serde_json::from_str::<MutationSequence>(bad).is_err(), "{bad}");
            assert!(serde_json::from_str::<DiffSequence>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn checked_add_overflow_is_an_error() {
        assert_eq!(
            EntryRevision::from(u64::MAX - 1).checked_add(1),
            Ok(EntryRevision::from(u64::MAX))
        );
        assert!(EntryRevision::from(u64::MAX).checked_add(1).is_err());
        assert!(MutationSequence::from(u64::MAX).next().is_err());
        assert!(DiffSequence::from(u64::MAX).checked_add(1).is_err());
        assert_eq!(DiffSequence::FIRST.next(), Ok(DiffSequence::from(2)));
    }

    #[test]
    fn opaque_ids_round_trip_as_base64url() -> TestResult {
        let owner = OwnerId::from([7u8; RANDOM_ID_BYTES]);
        let json = serde_json::to_string(&owner)?;
        assert_eq!(json, "\"BwcHBwcHBwcHBwcHBwcHBw\"");
        assert_eq!(serde_json::from_str::<OwnerId>(&json)?, owner);
        assert!("short".parse::<LifetimeId>().is_err());
        assert_ne!(OperationId::generate()?, OperationId::generate()?);
        Ok(())
    }

    #[test]
    fn equal_acquisition_with_a_different_owner_conflicts() {
        let generation = StreamGeneration::from([1u8; RANDOM_ID_BYTES]);
        let first = OwnerId::from([2u8; RANDOM_ID_BYTES]);
        let second = OwnerId::from([3u8; RANDOM_ID_BYTES]);
        let at = |revision: u64, owner| {
            GenerationEpoch::new(generation, OwnerEpoch::new(EntryRevision::from(revision), owner))
        };
        assert_eq!(at(5, first).compare(at(5, second)), Ok(EpochOrder::Conflict));
        assert_eq!(at(5, first).compare(at(5, first)), Ok(EpochOrder::Same));
        assert_eq!(at(4, first).compare(at(5, second)), Ok(EpochOrder::Older));
        assert_eq!(at(6, first).compare(at(5, second)), Ok(EpochOrder::Newer));
    }

    #[test]
    fn epochs_from_different_generations_are_not_compared() {
        let owner = OwnerId::from([2u8; RANDOM_ID_BYTES]);
        let epoch = OwnerEpoch::new(EntryRevision::from(5), owner);
        let old = GenerationEpoch::new(StreamGeneration::from([1u8; RANDOM_ID_BYTES]), epoch);
        let new = GenerationEpoch::new(StreamGeneration::from([9u8; RANDOM_ID_BYTES]), epoch);
        assert!(old.compare(new).is_err());
    }
}
