use crate::constants::{CLIENT_ID_MAX_BYTE, CLIENT_ID_MIN_BYTE};
use crate::violation::{SyntaxRule, SyntaxViolation};

pub const fn validate_non_empty(value: &str) -> Result<(), SyntaxViolation> {
    if value.is_empty() {
        Err(SyntaxViolation::at(SyntaxRule::NonEmpty, 0, 0))
    } else {
        Ok(())
    }
}

pub const fn validate_subject_id(value: &str) -> Result<(), SyntaxViolation> {
    validate_non_empty(value)
}

pub const fn validate_client_id(value: &str) -> Result<(), SyntaxViolation> {
    if let Err(violation) = validate_non_empty(value) {
        return Err(violation);
    }
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < CLIENT_ID_MIN_BYTE || byte > CLIENT_ID_MAX_BYTE {
            return Err(SyntaxViolation::at(SyntaxRule::PrintableAscii, index, byte));
        }
        index += 1;
    }
    Ok(())
}
