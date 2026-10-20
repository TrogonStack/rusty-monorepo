use std::fmt;

use crate::constants::MAX_UTC_OFFSET_MINUTES;
use crate::errors::UtcOffsetError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UtcOffset {
    minutes: i16,
}

impl UtcOffset {
    pub const UTC: Self = Self { minutes: 0 };

    pub const fn try_from_minutes(minutes: i32) -> Result<Self, UtcOffsetError> {
        if minutes < -MAX_UTC_OFFSET_MINUTES || minutes > MAX_UTC_OFFSET_MINUTES {
            return Err(UtcOffsetError::OutOfRange { minutes });
        }
        Ok(Self {
            minutes: minutes as i16,
        })
    }

    pub const fn minutes(self) -> i32 {
        self.minutes as i32
    }

    pub const fn seconds(self) -> i64 {
        self.minutes as i64 * 60
    }
}

impl fmt::Display for UtcOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.minutes == 0 {
            return f.write_str("Z");
        }
        let sign = if self.minutes < 0 { '-' } else { '+' };
        let magnitude = self.minutes.unsigned_abs();
        write!(f, "{sign}{:02}:{:02}", magnitude / 60, magnitude % 60)
    }
}
