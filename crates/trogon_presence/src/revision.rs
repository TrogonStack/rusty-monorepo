use std::fmt;

use serde::{Deserialize, Serialize};

use crate::position::{decimal_u64, PositionOverflow};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryRevision(#[serde(with = "decimal_u64")] u64);

pub type Revision = EntryRevision;

impl EntryRevision {
    pub fn get(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, delta: u64) -> Result<Self, PositionOverflow> {
        self.0
            .checked_add(delta)
            .map(Self)
            .ok_or(PositionOverflow("entry revision"))
    }

    pub fn checked_sub(self, delta: u64) -> Result<Self, PositionOverflow> {
        self.0
            .checked_sub(delta)
            .map(Self)
            .ok_or(PositionOverflow("entry revision"))
    }
}

impl From<u64> for EntryRevision {
    fn from(sequence: u64) -> Self {
        Self(sequence)
    }
}

impl fmt::Display for EntryRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
