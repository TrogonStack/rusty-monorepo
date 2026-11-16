use std::fmt;

use crate::errors::Unsigned63Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Unsigned63(u64);

impl Unsigned63 {
    pub const MAX: u64 = i64::MAX as u64;

    pub const fn try_new(value: u64) -> Result<Self, Unsigned63Error> {
        if value > Self::MAX {
            Err(Unsigned63Error { value, max: Self::MAX })
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for Unsigned63 {
    type Error = Unsigned63Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<u32> for Unsigned63 {
    fn from(value: u32) -> Self {
        Self(u64::from(value))
    }
}

impl From<Unsigned63> for u64 {
    fn from(value: Unsigned63) -> Self {
        value.0
    }
}

impl fmt::Display for Unsigned63 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
