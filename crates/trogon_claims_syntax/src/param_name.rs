use crate::constants::PARAM_NAME_MAX_BYTES;
use crate::violation::{SyntaxRule, SyntaxViolation};

pub const fn validate_param_name(value: &str) -> Result<(), SyntaxViolation> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err(SyntaxViolation::at(SyntaxRule::NonEmpty, 0, 0));
    }
    if bytes.len() > PARAM_NAME_MAX_BYTES {
        return Err(SyntaxViolation::at(
            SyntaxRule::MaxBytes(PARAM_NAME_MAX_BYTES),
            PARAM_NAME_MAX_BYTES,
            bytes[PARAM_NAME_MAX_BYTES],
        ));
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return Err(SyntaxViolation::at(SyntaxRule::IdentifierStart, 0, first));
    }
    let mut index = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if !(byte.is_ascii_alphanumeric() || byte == b'_') {
            return Err(SyntaxViolation::at(SyntaxRule::IdentifierAlphabet, index, byte));
        }
        index += 1;
    }
    Ok(())
}
