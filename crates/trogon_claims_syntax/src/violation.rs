use core::fmt;

use crate::constants::OBJECT_ID_ALPHABET;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxRule {
    NonEmpty,
    MaxBytes(usize),
    ObjectIdAlphabet,
    NotWildcard,
    PrintableAscii,
    SegmentAlphabet,
    SegmentStart,
    SegmentEnd,
    SegmentLength { min: usize, max: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyntaxViolation {
    offset: usize,
    byte: u8,
    rule: SyntaxRule,
}

impl SyntaxViolation {
    pub(crate) const fn at(rule: SyntaxRule, offset: usize, byte: u8) -> Self {
        Self { offset, byte, rule }
    }

    pub const fn offset(&self) -> usize {
        self.offset
    }

    pub const fn byte(&self) -> u8 {
        self.byte
    }

    pub const fn rule(&self) -> SyntaxRule {
        self.rule
    }
}

struct Offender(u8);

impl fmt::Display for Offender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_ascii_graphic() || self.0 == b' ' {
            write!(f, "'{}'", char::from(self.0))
        } else {
            write!(f, "byte {:#04x}", self.0)
        }
    }
}

impl fmt::Display for SyntaxViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let offender = Offender(self.byte);
        let offset = self.offset;
        match self.rule {
            SyntaxRule::NonEmpty => f.write_str("must not be empty"),
            SyntaxRule::MaxBytes(max) => write!(f, "exceeds {max} bytes at byte {offset}"),
            SyntaxRule::ObjectIdAlphabet => write!(
                f,
                "invalid character {offender} at byte {offset} (allowed: {OBJECT_ID_ALPHABET})"
            ),
            SyntaxRule::NotWildcard => f.write_str("the wildcard '*' is not an object id"),
            SyntaxRule::PrintableAscii => write!(
                f,
                "invalid character {offender} at byte {offset} (allowed: printable ASCII 0x20 to 0x7E)"
            ),
            SyntaxRule::SegmentAlphabet => write!(
                f,
                "invalid character {offender} at byte {offset} (allowed in a segment: [a-z0-9_])"
            ),
            SyntaxRule::SegmentStart => write!(
                f,
                "a segment must start with a lowercase ASCII letter, found {offender} at byte {offset}"
            ),
            SyntaxRule::SegmentEnd => write!(
                f,
                "a segment must end with a lowercase ASCII letter or digit, found {offender} at byte {offset}"
            ),
            SyntaxRule::SegmentLength { min, max } => {
                write!(f, "a segment must be {min} to {max} bytes, violated at byte {offset}")
            }
        }
    }
}
