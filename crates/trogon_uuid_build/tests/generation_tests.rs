use trogon_uuid_build::{generate, Template};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

const DESCRIPTORS: &[u8] = include_bytes!("fixtures/recipes.binpb");

fn alter_field(message: &mut DynamicMessage, parent: &str, field_name: &str, property: &str, value: &Value) -> bool {
    let is_parent = message.get_field_by_name("name").is_some_and(|name| name.as_ref() == &Value::String(parent.to_owned()));
    if is_parent {
        if let Some(Value::List(fields)) = message.get_field_by_name_mut("field") {
            for field in fields {
                if let Value::Message(field) = field {
                    if field.get_field_by_name("name").is_some_and(|name| name.as_ref() == &Value::String(field_name.to_owned())) {
                        field.set_field_by_name(property, value.clone());
                        return true;
                    }
                }
            }
        }
    }
    for (_, nested) in message.fields_mut() {
        match nested {
            Value::Message(nested) => {
                if alter_field(nested, parent, field_name, property, value) { return true; }
            }
            Value::List(items) => {
                for item in items {
                    if let Value::Message(nested) = item {
                        if alter_field(nested, parent, field_name, property, value) { return true; }
                    }
                }
            }
            _ => {}
        }
    }
    false
}

fn changed_schema(parent: &str, field: &str, property: &str, value: Value) -> Vec<u8> {
    let pool = DescriptorPool::decode(DESCRIPTORS).unwrap();
    let descriptor = pool.get_message_by_name("google.protobuf.FileDescriptorSet").unwrap();
    let mut message = DynamicMessage::decode(descriptor, DESCRIPTORS).unwrap();
    assert!(alter_field(&mut message, parent, field, property, &value));
    message.encode_to_vec()
}

#[test]
fn rejects_schema_with_changed_inner_wire_contract() {
    let descriptors = changed_schema("Format", "template", "number", Value::I32(3));
    let result = generate(&descriptors, &[Template::new(
        "example.catalog.v1.SkuIdentityVersion", "SKU_IDENTITY_VERSION_V1", "SkuIdentityV1",
    )]);
    assert!(result.is_err(), "changed field tag must not silently change generated identity");
}

#[test]
fn emits_a_typed_recipe_for_a_selected_identity_version() {
    let source = generate(DESCRIPTORS, &[Template::new(
        "example.catalog.v1.SkuIdentityVersion", "SKU_IDENTITY_VERSION_V1", "SkuIdentityV1",
    )]).expect("public descriptor options are valid");
    assert!(source.contains("pub struct SkuIdentityV1"), "a selected recipe has a Rust marker");
    assert!(source.contains("warehouse_id"), "template inputs become Rust fields");
}

#[test]
fn rejects_invalid_descriptor_recipe_and_target_configuration() {
    let select = |value| [Template::new("example.catalog.v1.SkuIdentityVersion", value, "Chosen")];
    for value in ["MISSING_FORMAT", "BAD_UUID", "BAD_TEMPLATE", "EMPTY_NAMESPACE", "NOT_A_VERSION"] {
        assert!(generate(DESCRIPTORS, &select(value)).is_err(), "invalid recipe should fail: {value}");
    }
    assert!(generate(&[0xff], &select("SKU_IDENTITY_VERSION_V1")).is_err(), "malformed descriptor data fails");
    assert!(generate(DESCRIPTORS, &[]).is_err(), "empty target selection fails");
    assert!(generate(DESCRIPTORS, &[Template::new("missing.Enum", "V1", "Chosen")]).is_err(), "missing enum fails");
    assert!(generate(DESCRIPTORS, &[Template::new("example.catalog.v1.MissingNamespaceVersion", "MISSING_NAMESPACE_V1", "Chosen")]).is_err(), "missing namespace fails");
    assert!(generate(DESCRIPTORS, &[Template::new("example.catalog.v1.SkuIdentityVersion", "SKU_IDENTITY_VERSION_V1", "bad name")]).is_err(), "invalid generated name fails");
    let duplicates = [
        Template::new("example.catalog.v1.SkuIdentityVersion", "SKU_IDENTITY_VERSION_V1", "Chosen"),
        Template::new("example.catalog.v1.SkuIdentityVersion", "SKU_IDENTITY_VERSION_V2", "ChosenInput"),
    ];
    assert!(generate(DESCRIPTORS, &duplicates).is_err(), "marker and input names cannot collide");
    assert!(trogon_uuid_build::Config::new().runtime_path("::bad; injected").generate(DESCRIPTORS, &select("SKU_IDENTITY_VERSION_V1")).is_err(), "invalid runtime path fails");
}
