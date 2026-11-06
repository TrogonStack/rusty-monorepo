#![cfg(feature = "derive")]

use trogon_object_id::{ObjectId, Text};

#[derive(Debug, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "item")]
#[repr(transparent)]
struct ItemId(Text);

#[test]
fn direct_identifier_uses_its_declared_prefix() {
    let id = ItemId::try_from_raw("abc-123").expect("valid raw identifier");
    assert_eq!(
        id.to_string(),
        "item_abc-123",
        "the direct newtype retains its domain prefix"
    );
}

#[derive(Debug, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "invoice", separator = ":")]
#[repr(transparent)]
struct InvoiceId(Text<trogon_object_id::validators::Integer>);

#[derive(Debug, PartialEq, Eq, trogon_object_id::ObjectIdUnion)]
enum PrincipalId {
    Item(ItemId),
    Invoice(InvoiceId),
}

#[test]
fn direct_newtypes_reuse_owned_payloads_and_preserve_validation() {
    let raw = String::from("owned-value");
    let pointer = raw.as_ptr();
    let id = ItemId::try_from_owned(raw).expect("owned raw identifier");
    assert_eq!(id.as_raw().as_ptr(), pointer, "raw input allocation is reused");
    let full = String::from("item_owned-value");
    let pointer = full.as_ptr();
    let id = ItemId::try_from(full).expect("owned full identifier");
    assert_eq!(id.as_raw().as_ptr(), pointer, "full input allocation is reused");
    assert_eq!(
        id.into_payload().as_str(),
        "owned-value",
        "payload ownership is recoverable"
    );
    for value in ["", "item_", "wrong_123", "item_bad\nvalue"] {
        assert!(value.parse::<ItemId>().is_err(), "invalid full identifier is rejected");
    }
    for value in ["", "a\0b", &"x".repeat(1025)] {
        assert!(
            ItemId::try_from_owned(value.to_owned()).is_err(),
            "owned raw input stays validated"
        );
    }
    assert!(
        InvoiceId::try_from_raw("bad").is_err(),
        "custom validator remains active"
    );
    let invoice = InvoiceId::try_from_raw("+00042").expect("signed decimal input");
    assert_eq!(
        invoice.to_string(),
        "invoice:+00042",
        "custom separator and spelling are preserved"
    );
    assert_eq!(invoice.payload().as_str(), "+00042", "payload access is read only");
    assert_eq!(
        InvoiceId::try_from_payload(Text::try_from_raw("42").expect("valid payload"))
            .expect("checked typed input")
            .as_raw(),
        "42",
        "typed payload wrapping reuses the core constructor"
    );
}

#[test]
fn derived_union_accepts_direct_identifier_types() {
    let principal = "invoice:42".parse::<PrincipalId>().expect("known union variant");
    assert!(
        matches!(principal, PrincipalId::Invoice(_)),
        "full prefix selects the direct type"
    );
    let item = ItemId::try_from_raw("abc").expect("valid item");
    assert_eq!(
        PrincipalId::from(item).to_string(),
        "item_abc",
        "conversion preserves its domain"
    );
    assert!(
        "invoice:bad".parse::<PrincipalId>().is_err(),
        "union cannot bypass the custom payload validator"
    );
}

#[cfg(feature = "uuid")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "session")]
#[repr(transparent)]
struct SessionId(trogon_object_id::Uuid);

#[cfg(feature = "uuid")]
#[test]
fn direct_uuid_identifiers_keep_binary_layout_and_checked_generation() {
    fn copy<T: Copy>(value: T) -> (T, T) {
        (value, value)
    }
    let generated = SessionId::generate::<trogon_object_id::Uuid7>().expect("UUID7 generation");
    let (first, second) = copy(generated);
    assert_eq!(first, second, "copy preserves the identifier");
    assert_eq!(
        first.payload().get_version_num(),
        7,
        "the generator keeps its UUID version"
    );
    assert_eq!(
        std::mem::size_of::<SessionId>(),
        16,
        "direct UUID wrapper stores no extra data"
    );
    assert_eq!(
        generated.to_string().parse::<SessionId>().expect("UUID round trip"),
        generated,
        "full parse round trip"
    );
}

#[cfg(feature = "serde")]
#[test]
fn direct_identifiers_serialize_through_checked_full_form() {
    let id = ItemId::try_from_raw("abc").expect("valid item");
    let json = serde_json::to_string(&id).expect("serialize direct ID");
    assert_eq!(json, "\"item_abc\"", "full form is serialized");
    assert_eq!(
        serde_json::from_str::<ItemId>(&json).expect("deserialize direct ID"),
        id,
        "direct serde round trip"
    );
    for invalid in ["\"abc\"", "\"item_\"", "\"item_bad\\nvalue\"", "null", "{}"] {
        assert!(
            serde_json::from_str::<ItemId>(invalid).is_err(),
            "serde cannot bypass input checks"
        );
    }

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Record {
        #[serde(with = "trogon_object_id::serde_raw")]
        invoice: InvoiceId,
    }
    let record = Record {
        invoice: InvoiceId::try_from_raw("+00042").expect("valid direct integer"),
    };
    let json = serde_json::to_string(&record).expect("serialize direct raw field");
    assert_eq!(json, "{\"invoice\":\"+00042\"}", "raw adapter works with direct IDs");
    assert_eq!(
        serde_json::from_str::<Record>(&json).expect("deserialize direct raw field"),
        record,
        "direct raw field round trip"
    );
    assert!(
        serde_json::from_str::<Record>("{\"invoice\":\"bad\"}").is_err(),
        "direct raw adapter retains custom validation"
    );
    let principal = PrincipalId::from(record.invoice);
    let json = serde_json::to_string(&principal).expect("serialize direct union");
    assert_eq!(
        serde_json::from_str::<PrincipalId>(&json).expect("deserialize direct union"),
        principal,
        "direct union serde round trip"
    );
}
