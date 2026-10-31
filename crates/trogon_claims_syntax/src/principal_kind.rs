use crate::constants::PRINCIPAL_KIND_MAX_BYTES;
use crate::segment::validate_segment;
use crate::violation::{SyntaxRule, SyntaxViolation};

pub const fn validate_principal_kind(value: &str) -> Result<(), SyntaxViolation> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err(SyntaxViolation::at(SyntaxRule::NonEmpty, 0, 0));
    }
    validate_segment(bytes, 0, bytes.len(), PRINCIPAL_KIND_MAX_BYTES)
}
