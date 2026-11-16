use crate::constants::{OBJECT_TYPE_MAX_BYTES, TYPE_FINAL_SEGMENT_MAX_BYTES, TYPE_INNER_SEGMENT_MAX_BYTES};
use crate::segment::validate_segment;
use crate::violation::{SyntaxRule, SyntaxViolation};

pub const fn validate_object_type(value: &str) -> Result<(), SyntaxViolation> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err(SyntaxViolation::at(SyntaxRule::NonEmpty, 0, 0));
    }
    if bytes.len() > OBJECT_TYPE_MAX_BYTES {
        return Err(SyntaxViolation::at(
            SyntaxRule::MaxBytes(OBJECT_TYPE_MAX_BYTES),
            OBJECT_TYPE_MAX_BYTES,
            bytes[OBJECT_TYPE_MAX_BYTES],
        ));
    }
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'/' {
            if let Err(violation) = validate_segment(bytes, start, index, TYPE_INNER_SEGMENT_MAX_BYTES) {
                return Err(violation);
            }
            start = index + 1;
        }
        index += 1;
    }
    validate_segment(bytes, start, bytes.len(), TYPE_FINAL_SEGMENT_MAX_BYTES)
}
