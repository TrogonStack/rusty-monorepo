#![allow(clippy::unwrap_used, clippy::expect_used)]

use trogon_claims_syntax::{
    validate_client_id, validate_non_empty, validate_object_id, validate_object_type, validate_param_name,
    validate_relation_name, validate_subject_id, SyntaxRule,
};

#[test]
fn object_id_reports_the_offending_byte_and_offset() {
    let violation = validate_object_id("has a space").unwrap_err();
    assert_eq!(violation.offset(), 3);
    assert_eq!(violation.byte(), b' ');
    assert_eq!(violation.rule(), SyntaxRule::ObjectIdAlphabet);
    assert_eq!(
        violation.to_string(),
        "invalid character ' ' at byte 3 (allowed: [a-zA-Z0-9/_|\\-=+])"
    );
}

#[test]
fn object_id_rejects_the_bare_wildcard_only_by_name() {
    assert_eq!(validate_object_id("*").unwrap_err().rule(), SyntaxRule::NotWildcard);
    assert_eq!(
        validate_object_id("a*").unwrap_err().rule(),
        SyntaxRule::ObjectIdAlphabet
    );
}

#[test]
fn empty_values_are_rejected_by_every_validator_that_requires_content() {
    for result in [
        validate_non_empty(""),
        validate_subject_id(""),
        validate_client_id(""),
        validate_object_id(""),
        validate_object_type(""),
    ] {
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

#[test]
fn non_ascii_offender_is_shown_as_a_byte() {
    let violation = validate_object_id("é").unwrap_err();
    assert_eq!(
        violation.to_string(),
        "invalid character byte 0xc3 at byte 0 (allowed: [a-zA-Z0-9/_|\\-=+])"
    );
}

#[test]
fn object_type_segment_rules_name_the_segment_fault() {
    assert_eq!(
        validate_object_type("ab").unwrap_err().rule(),
        SyntaxRule::SegmentLength { min: 3, max: 64 }
    );
    assert_eq!(
        validate_object_type("Abc").unwrap_err().rule(),
        SyntaxRule::SegmentStart
    );
    assert_eq!(validate_object_type("ab_").unwrap_err().rule(), SyntaxRule::SegmentEnd);
    assert_eq!(
        validate_object_type("a-bc").unwrap_err().rule(),
        SyntaxRule::SegmentAlphabet
    );
    assert!(validate_object_type("myapp/document").is_ok());
}

#[test]
fn relation_name_is_one_segment_of_three_to_sixty_four_bytes() {
    assert!(validate_relation_name("view").is_ok());
    assert!(validate_relation_name("can_edit_2").is_ok());
    assert_eq!(
        validate_relation_name("ab").unwrap_err().rule(),
        SyntaxRule::SegmentLength { min: 3, max: 64 }
    );
    assert_eq!(validate_relation_name("").unwrap_err().rule(), SyntaxRule::NonEmpty);
    assert_eq!(
        validate_relation_name("a/b_c").unwrap_err().rule(),
        SyntaxRule::SegmentAlphabet
    );
    assert_eq!(
        validate_relation_name("Edit").unwrap_err().rule(),
        SyntaxRule::SegmentStart
    );
    assert_eq!(
        validate_relation_name("edit_").unwrap_err().rule(),
        SyntaxRule::SegmentEnd
    );
}

#[test]
fn param_name_is_a_cel_identifier() {
    assert!(validate_param_name("user_ip").is_ok());
    assert!(validate_param_name("_x1").is_ok());
    assert!(validate_param_name("X").is_ok());
    assert_eq!(validate_param_name("").unwrap_err().rule(), SyntaxRule::NonEmpty);
    assert_eq!(
        validate_param_name("1x").unwrap_err().rule(),
        SyntaxRule::IdentifierStart
    );
    let violation = validate_param_name("user-ip").unwrap_err();
    assert_eq!(violation.rule(), SyntaxRule::IdentifierAlphabet);
    assert_eq!(violation.offset(), 4);
    assert_eq!(
        validate_param_name(&"a".repeat(65)).unwrap_err().rule(),
        SyntaxRule::MaxBytes(64)
    );
}
