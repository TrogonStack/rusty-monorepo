#![cfg(feature = "derive")]

use trogon_object_id::{ObjectId, Text};

#[derive(Debug, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "item")]
#[repr(transparent)]
struct ItemId(Text);

#[test]
fn direct_identifier_uses_its_declared_prefix() {
    let id = ItemId::try_from_raw("abc-123").expect("valid raw identifier");
    assert_eq!(id.to_string(), "item_abc-123", "the direct newtype retains its domain prefix");
}
