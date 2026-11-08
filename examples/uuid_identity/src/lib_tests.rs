use super::generated::*;
use super::SkuId;

#[test]
fn generated_recipes_match_independent_uuid_v5_vectors() {
    let v1 = SkuIdentityV1::uuid5(SkuIdentityV1Input {
        warehouse_id: "warehouse-17",
        sku_code: "ABC-123",
    });
    let v2 = SkuIdentityV2::uuid5(SkuIdentityV2Input {
        warehouse_id: "warehouse-17",
        sku_code: "ABC-123",
    });
    assert_eq!(
        SkuIdentityV1::NAMESPACE.to_string(),
        "b5f931ec-3233-5e4d-b390-eebe1ecc75ad"
    );
    assert_eq!(v1.to_string(), "1724f7cd-15b8-5e76-ba00-70d188c9b209");
    assert_eq!(v2.to_string(), "3d79f078-c578-5f7f-95ef-c517a6fcfda4");
    assert_ne!(v1, v2, "identity versions derive distinct namespaces");
    assert_eq!(
        DirectIdentity::uuid5(DirectIdentityInput { sku_code: "ABC-123" }).to_string(),
        "c8f7f751-fc62-50d3-b7c4-491d1fb478bf"
    );
    assert_eq!(
        UrlIdentity::uuid5(UrlIdentityInput { sku_code: "ABC-123" }).to_string(),
        "bf09cc97-960a-58e6-b1f8-8a2255155910"
    );
    assert_eq!(
        OverrideIdentity::uuid5(OverrideIdentityInput { sku_code: "ABC-123" }).to_string(),
        "b263c245-fa81-535a-8bca-7acb23e9342d"
    );
    assert_eq!(
        RepeatedIdentity::uuid5(RepeatedIdentityInput { sku_code: "🦀" }).to_string(),
        "bcf56975-c58b-50a5-8b05-3361c99fb958"
    );
}

#[test]
fn static_and_empty_templates_embed_the_final_uuid() {
    const STATIC: trogon_uuid::Uuid = StaticIdentity::VALUE;
    const EMPTY: trogon_uuid::Uuid = EmptyIdentity::VALUE;
    assert_eq!(STATIC.to_string(), "e241525a-825f-5088-ba4b-73946b79c8d0");
    assert_eq!(EMPTY.to_string(), "941868af-9b87-5e64-90c5-a6a21bb4603e");
    assert_eq!(StaticIdentity::uuid5(()), STATIC);
    assert_eq!(EmptyIdentity::uuid5(()), EMPTY);
}

#[test]
fn object_id_wraps_the_recipe_uuid_without_extra_storage() {
    let id = SkuId::from_identity(SkuIdentityV1Input {
        warehouse_id: "warehouse-17",
        sku_code: "ABC-123",
    })
    .expect("valid generated UUID");
    assert_eq!(id.to_string(), "sku_1724f7cd-15b8-5e76-ba00-70d188c9b209");
    assert_eq!(id.to_string().parse::<SkuId>().expect("full object ID"), id);
    assert_eq!(std::mem::size_of::<SkuId>(), std::mem::size_of::<trogon_uuid::Uuid>());
}
