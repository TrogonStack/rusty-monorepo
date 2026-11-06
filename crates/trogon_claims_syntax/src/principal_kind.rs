use crate::text::validate_non_empty;
use crate::violation::SyntaxViolation;

pub const fn validate_principal_kind(value: &str) -> Result<(), SyntaxViolation> {
    validate_non_empty(value)
}
