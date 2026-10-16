use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::constants::RANDOM_ID_BYTES;
use crate::entropy::{decode_id, encode_id, random_id_bytes, EntropyError};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HolderId([u8; RANDOM_ID_BYTES]);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("holder id must be 16 bytes encoded as 22 characters of unpadded base64url")]
pub struct HolderIdError;

impl HolderId {
    pub fn generate() -> Result<Self, EntropyError> {
        random_id_bytes().map(Self)
    }

    pub fn as_bytes(&self) -> &[u8; RANDOM_ID_BYTES] {
        &self.0
    }
}

impl From<[u8; RANDOM_ID_BYTES]> for HolderId {
    fn from(bytes: [u8; RANDOM_ID_BYTES]) -> Self {
        Self(bytes)
    }
}

impl FromStr for HolderId {
    type Err = HolderIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        decode_id(s).map(Self).ok_or(HolderIdError)
    }
}

impl TryFrom<String> for HolderId {
    type Error = HolderIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<HolderId> for String {
    fn from(holder: HolderId) -> Self {
        encode_id(&holder.0)
    }
}

impl fmt::Display for HolderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&encode_id(&self.0))
    }
}

impl fmt::Debug for HolderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HolderId(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_text() {
        let holder = HolderId::from([0xab; RANDOM_ID_BYTES]);
        let text = holder.to_string();
        assert_eq!(text.len(), 22);
        assert_eq!(text.parse::<HolderId>(), Ok(holder));
    }

    #[test]
    fn parses_design_example() {
        assert!("q3V9hX0bS2mWf1ZkR8aT1A".parse::<HolderId>().is_ok());
    }

    #[test]
    fn rejects_malformed_text() {
        for bad in [
            "",
            "q3V9hX0bS2mWf1ZkR8aT1",
            "q3V9hX0bS2mWf1ZkR8aT1A=",
            "q3V9hX0bS2mWf1ZkR8aT1B",
            "q3V9hX0bS2mWf1ZkR8aT1+",
            "q3V9hX0bS2mWf1ZkR8a.1A",
        ] {
            assert_eq!(bad.parse::<HolderId>(), Err(HolderIdError), "{bad}");
        }
    }

    #[test]
    fn generated_ids_differ() -> Result<(), EntropyError> {
        assert_ne!(HolderId::generate()?, HolderId::generate()?);
        Ok(())
    }
}
