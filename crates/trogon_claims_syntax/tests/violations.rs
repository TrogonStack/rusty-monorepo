#![allow(clippy::unwrap_used, clippy::expect_used)]

use trogon_claims_syntax::{validate_client_id, validate_non_empty, validate_subject_id, SyntaxRule};

#[test]
fn empty_values_are_rejected_by_every_validator_that_requires_content() {
    for result in [validate_non_empty(""), validate_subject_id(""), validate_client_id("")] {
        assert_eq!(result.unwrap_err().rule(), SyntaxRule::NonEmpty);
    }
}

#[test]
fn client_id_accepts_printable_ascii_only() {
    assert!(validate_client_id("client id!~").is_ok());
    assert_eq!(validate_client_id("a\tb").unwrap_err().offset(), 1);
    assert_eq!(validate_client_id("aé").unwrap_err().rule(), SyntaxRule::PrintableAscii);
}

#[test]
fn subject_id_accepts_any_non_empty_text() {
    assert!(validate_subject_id("user@example.com:42 é").is_ok());
}
