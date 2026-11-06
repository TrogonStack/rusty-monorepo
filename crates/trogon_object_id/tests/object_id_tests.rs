use trogon_object_id::ObjectId;

#[test]
fn accepts_a_nonempty_raw_identifier() {
    let id = ObjectId::try_from_raw("abc-123").expect("valid raw identifier");
    assert_eq!(id.as_raw(), "abc-123", "raw suffix is preserved");
}
