#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use trogon_claims::{Base64UrlUnpadded, SubjectId, SubjectMapping, SubjectMappingError, Verbatim};

fn subject(value: &str) -> SubjectId {
    SubjectId::try_new(value.to_owned()).unwrap()
}

#[test]
fn verbatim_keeps_the_subject_and_rejects_the_wildcard() {
    assert_eq!(Verbatim::map(&subject("user-1")).unwrap().as_str(), "user-1");
    assert_eq!(Verbatim::map(&subject("*")).unwrap_err(), SubjectMappingError::Wildcard);
}

#[test]
fn verbatim_surfaces_object_id_violations() {
    assert!(matches!(
        Verbatim::map(&subject("user@example.com#frag")).unwrap_err(),
        SubjectMappingError::Invalid(_)
    ));
}

#[test]
fn base64url_makes_any_subject_a_valid_object_id() {
    assert_eq!(
        Base64UrlUnpadded::map(&subject("user@example.com")).unwrap().as_str(),
        "dXNlckBleGFtcGxlLmNvbQ"
    );
    assert!(Base64UrlUnpadded::map(&subject("*")).is_ok());
}

#[test]
fn base64url_bounds_its_source() {
    let at_limit = "a".repeat(768);
    assert!(Base64UrlUnpadded::map(&subject(&at_limit)).is_ok());
    let over = "a".repeat(769);
    assert_eq!(
        Base64UrlUnpadded::map(&subject(&over)).unwrap_err(),
        SubjectMappingError::TooLong { bytes: 769, max: 768 }
    );
}
