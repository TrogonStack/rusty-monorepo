use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::civil::CivilDateTime;
use crate::constants::{MAX_UNIX_SECONDS, MIN_UNIX_SECONDS, NANOS_PER_SECOND};
use crate::errors::TimestampError;
use crate::utc_offset::UtcOffset;

#[derive(Clone, Copy, Debug)]
pub struct Timestamp {
    secs: i64,
    nanos: u32,
    offset: UtcOffset,
}

const fn within_range(secs: i64) -> bool {
    secs >= MIN_UNIX_SECONDS && secs <= MAX_UNIX_SECONDS
}

impl Timestamp {
    pub const fn try_new(secs: i64, nanos: u32, offset: UtcOffset) -> Result<Self, TimestampError> {
        if nanos >= NANOS_PER_SECOND {
            return Err(TimestampError::NanosOutOfRange { nanos });
        }
        if !within_range(secs) || !within_range(secs + offset.seconds()) {
            return Err(TimestampError::SecondsOutOfRange { secs });
        }
        Ok(Self { secs, nanos, offset })
    }

    pub(crate) const fn from_validated_utc(secs: i64, nanos: u32) -> Self {
        Self {
            secs,
            nanos,
            offset: UtcOffset::UTC,
        }
    }

    pub const fn secs(&self) -> i64 {
        self.secs
    }

    pub const fn nanos(&self) -> u32 {
        self.nanos
    }

    pub const fn offset(&self) -> UtcOffset {
        self.offset
    }
}

impl PartialEq for Timestamp {
    fn eq(&self, other: &Self) -> bool {
        self.secs == other.secs && self.nanos == other.nanos
    }
}

impl Eq for Timestamp {}

impl Hash for Timestamp {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.secs.hash(state);
        self.nanos.hash(state);
    }
}

impl PartialOrd for Timestamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Timestamp {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.secs, self.nanos).cmp(&(other.secs, other.nanos))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let local = CivilDateTime::from_unix_seconds(self.secs + self.offset.seconds());
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            local.year, local.month, local.day, local.hour, local.minute, local.second
        )?;
        if self.nanos != 0 {
            let fraction = format!("{:09}", self.nanos);
            write!(f, ".{}", fraction.trim_end_matches('0'))?;
        }
        write!(f, "{}", self.offset)
    }
}
