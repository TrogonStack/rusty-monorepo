use trogon_uuid_build::{generate, Template};

const DESCRIPTORS: &[u8] = include_bytes!("fixtures/recipes.binpb");

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
