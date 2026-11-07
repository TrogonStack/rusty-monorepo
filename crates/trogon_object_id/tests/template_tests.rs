#![cfg(all(feature = "macros", feature = "template"))]

use trogon_object_id::{Definition, Domain, ObjectId, TemplateDomain, Uuid, UuidTemplate};

struct ItemIdentityV1;
struct ItemIdentityV1Input<'a> {
    external_key: &'a str,
}
impl UuidTemplate for ItemIdentityV1 {
    type Input<'a> = ItemIdentityV1Input<'a>;
    const NAMESPACE: Uuid = Uuid::NAMESPACE_DNS;
    fn uuid5(input: Self::Input<'_>) -> Uuid {
        Uuid::new_v5(&Self::NAMESPACE, input.external_key.as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ObjectId)]
#[object_id(prefix = "item", template = ItemIdentityV1)]
#[repr(transparent)]
struct ItemId(Uuid);

struct ConfiguredDomain;
impl Domain for ConfiguredDomain {
    const DEFINITION: Definition = Definition::new("configured", "_");
    type Payload = Uuid;
}
impl TemplateDomain for ConfiguredDomain {
    type Template = ItemIdentityV1;
}
type ConfiguredId = ObjectId<ConfiguredDomain>;

#[test]
fn direct_identifier_uses_the_bound_typed_recipe() {
    let id = ItemId::from_identity(ItemIdentityV1Input {
        external_key: "example.org",
    })
    .expect("generated identity");
    let expected = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"example.org");
    assert_eq!(*id.payload(), expected, "the selected recipe receives the typed input");
}

#[test]
fn generic_identifier_uses_a_handwritten_template_binding() {
    let id = ConfiguredId::from_identity(ItemIdentityV1Input {
        external_key: "example.org",
    })
    .expect("generated identity");
    let expected = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"example.org");
    assert_eq!(
        *id.payload(),
        expected,
        "handwritten domains share the same recipe contract"
    );
    assert_eq!(
        id.to_string(),
        format!("configured_{expected}"),
        "generic binding retains its prefix"
    );
}
