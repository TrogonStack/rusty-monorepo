use crate::constants::{OBJECT_ID_MAX_BYTES, WILDCARD_BYTE};
use crate::violation::{SyntaxRule, SyntaxViolation};

const fn in_alphabet(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'/' | b'_' | b'|' | b'-' | b'=' | b'+')
}

pub const fn validate_object_id(value: &str) -> Result<(), SyntaxViolation> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err(SyntaxViolation::at(SyntaxRule::NonEmpty, 0, 0));
    }
    if bytes.len() == 1 && bytes[0] == WILDCARD_BYTE {
        return Err(SyntaxViolation::at(SyntaxRule::NotWildcard, 0, WILDCARD_BYTE));
    }
    let mut index = 0;
    while index < bytes.len() {
        if !in_alphabet(bytes[index]) {
            return Err(SyntaxViolation::at(SyntaxRule::ObjectIdAlphabet, index, bytes[index]));
        }
        index += 1;
    }
    if bytes.len() > OBJECT_ID_MAX_BYTES {
        return Err(SyntaxViolation::at(
            SyntaxRule::MaxBytes(OBJECT_ID_MAX_BYTES),
            OBJECT_ID_MAX_BYTES,
            bytes[OBJECT_ID_MAX_BYTES],
        ));
    }
    Ok(())
}
