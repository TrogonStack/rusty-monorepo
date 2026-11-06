use trogon_object_id::ObjectId;

#[test]
fn accepts_a_nonempty_raw_identifier() {
    let id = ObjectId::try_from_raw("abc-123").expect("valid raw identifier");
    assert_eq!(id.as_raw(), "abc-123", "raw suffix is preserved");
}

#[test]
fn rejects_empty_control_and_oversized_values() {
    for value in ["", "abc\nxyz", "abc\0xyz", "abc\u{0085}xyz", &"x".repeat(1025)] {
        assert!(ObjectId::try_from_raw(value).is_err(), "invalid suffix must be rejected");
    }
}
