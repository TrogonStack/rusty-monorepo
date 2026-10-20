use crate::constants::{MAX_SECOND_DIGITS, MAX_UNIX_SECONDS, MIN_UNIX_SECONDS, NANOS_PER_SECOND, NANO_DIGITS};
use crate::errors::{NumericDateError, TimestampError};
use crate::timestamp::Timestamp;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NumericDate {
    secs: i64,
    nanos: u32,
}

struct Decimal {
    negative: bool,
    digits: Vec<u8>,
    point: i64,
}

impl Decimal {
    fn parse(text: &str) -> Result<Self, NumericDateError> {
        if text.is_empty() {
            return Err(NumericDateError::Empty);
        }
        let bytes = text.as_bytes();
        let mut index = 0;
        let negative = bytes[0] == b'-';
        if negative {
            index += 1;
        }
        let mut digits = Vec::new();
        let integer_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            digits.push(bytes[index] - b'0');
            index += 1;
        }
        let integer_len = index - integer_start;
        if integer_len == 0 || (integer_len > 1 && bytes[integer_start] == b'0') {
            return Err(NumericDateError::NotANumber);
        }
        if index < bytes.len() && bytes[index] == b'.' {
            index += 1;
            let fraction_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                digits.push(bytes[index] - b'0');
                index += 1;
            }
            if index == fraction_start {
                return Err(NumericDateError::NotANumber);
            }
        }
        let mut exponent: i64 = 0;
        if index < bytes.len() && (bytes[index] == b'e' || bytes[index] == b'E') {
            index += 1;
            let mut exponent_negative = false;
            if index < bytes.len() && (bytes[index] == b'+' || bytes[index] == b'-') {
                exponent_negative = bytes[index] == b'-';
                index += 1;
            }
            let exponent_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                exponent = exponent
                    .saturating_mul(10)
                    .saturating_add(i64::from(bytes[index] - b'0'));
                index += 1;
            }
            if index == exponent_start {
                return Err(NumericDateError::NotANumber);
            }
            if exponent_negative {
                exponent = -exponent;
            }
        }
        if index != bytes.len() {
            return Err(NumericDateError::NotANumber);
        }
        let point = (integer_len as i64).saturating_add(exponent);
        Ok(Self {
            negative,
            digits,
            point,
        })
    }

    fn digit(&self, position: i64) -> u8 {
        usize::try_from(position)
            .ok()
            .and_then(|index| self.digits.get(index))
            .copied()
            .unwrap_or(0)
    }

    fn split(mut self) -> Result<(bool, i64, u32), NumericDateError> {
        let leading_zeros = self.digits.iter().take_while(|digit| **digit == 0).count();
        if leading_zeros == self.digits.len() {
            return Ok((false, 0, 0));
        }
        self.digits.drain(..leading_zeros);
        self.point = self.point.saturating_sub(leading_zeros as i64);
        if self.point > MAX_SECOND_DIGITS {
            return Err(NumericDateError::OutOfRange);
        }
        let mut secs: i64 = 0;
        for position in 0..self.point {
            secs = secs * 10 + i64::from(self.digit(position));
        }
        let mut nanos: u32 = 0;
        for offset in 0..NANO_DIGITS as i64 {
            nanos = nanos * 10 + u32::from(self.digit(self.point.saturating_add(offset)));
        }
        Ok((self.negative, secs, nanos))
    }
}

impl NumericDate {
    pub const fn try_new(secs: i64, nanos: u32) -> Result<Self, TimestampError> {
        if nanos >= NANOS_PER_SECOND {
            return Err(TimestampError::NanosOutOfRange { nanos });
        }
        if secs < MIN_UNIX_SECONDS || secs > MAX_UNIX_SECONDS {
            return Err(TimestampError::SecondsOutOfRange { secs });
        }
        Ok(Self { secs, nanos })
    }

    pub fn try_from_lexical(text: &str) -> Result<Self, NumericDateError> {
        let (negative, magnitude, fraction) = Decimal::parse(text)?.split()?;
        let (secs, nanos) = match (negative, fraction) {
            (true, 0) => (-magnitude, 0),
            (true, fraction) => (-magnitude - 1, NANOS_PER_SECOND - fraction),
            (false, fraction) => (magnitude, fraction),
        };
        Self::try_new(secs, nanos).map_err(|_| NumericDateError::OutOfRange)
    }

    pub const fn secs(&self) -> i64 {
        self.secs
    }

    pub const fn nanos(&self) -> u32 {
        self.nanos
    }

    pub const fn to_timestamp(self) -> Timestamp {
        Timestamp::from_validated_utc(self.secs, self.nanos)
    }
}
