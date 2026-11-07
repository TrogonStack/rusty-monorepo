use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyntaxRule {
    NonEmpty,
    PrintableAscii,
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
            SyntaxRule::PrintableAscii => write!(
                f,
                "invalid character {offender} at byte {offset} (allowed: printable ASCII 0x20 to 0x7E)"
            ),
        }
    }
}
