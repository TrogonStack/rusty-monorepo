use crate::constants::SEGMENT_MIN_BYTES;
use crate::violation::{SyntaxRule, SyntaxViolation};

const fn byte_at(bytes: &[u8], index: usize) -> u8 {
    if index < bytes.len() {
        bytes[index]
    } else {
        0
    }
}

pub(crate) const fn validate_segment(
    bytes: &[u8],
    start: usize,
    end: usize,
    max: usize,
) -> Result<(), SyntaxViolation> {
    let length = end - start;
    let length_rule = SyntaxRule::SegmentLength {
        min: SEGMENT_MIN_BYTES,
        max,
    };
    if length < SEGMENT_MIN_BYTES {
        return Err(SyntaxViolation::at(length_rule, start, byte_at(bytes, start)));
    }
    if length > max {
        return Err(SyntaxViolation::at(
            length_rule,
            start + max,
            byte_at(bytes, start + max),
        ));
    }
    if !bytes[start].is_ascii_lowercase() {
        return Err(SyntaxViolation::at(SyntaxRule::SegmentStart, start, bytes[start]));
    }
    let mut index = start;
    while index < end {
        let byte = bytes[index];
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
            return Err(SyntaxViolation::at(SyntaxRule::SegmentAlphabet, index, byte));
        }
        index += 1;
    }
    let last = bytes[end - 1];
    if last == b'_' {
        return Err(SyntaxViolation::at(SyntaxRule::SegmentEnd, end - 1, last));
    }
    Ok(())
}
