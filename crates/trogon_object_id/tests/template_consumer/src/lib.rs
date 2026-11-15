#![no_std]

use example::generated::{SkuIdentityV1, SkuIdentityV1Input};

#[derive(ids::ObjectId)]
#[object_id(prefix = "direct", template = SkuIdentityV1)]
#[repr(transparent)]
struct Direct(ids::Uuid);

#[derive(ids::ObjectId)]
enum Either {
    Direct(Direct),
}

pub fn exercise() -> bool {
    let id = Direct::from_identity(SkuIdentityV1Input {
        warehouse_id: "warehouse-17",
        sku_code: "ABC-123",
    })
    .expect("typed identity");
    let example = example::SkuId::from_identity(SkuIdentityV1Input {
        warehouse_id: "warehouse-17",
        sku_code: "ABC-123",
    })
    .expect("example identity");
    let typed = Direct::from(*id.payload());
    let expected = 0x1724f7cd15b85e76ba0070d188c9b209;
    assert_eq!(typed.as_raw().as_u128(), expected);
    assert_eq!(example.as_raw().as_u128(), expected);
    assert_eq!(core::mem::size_of_val(&id), 16);
    let _ = Either::from(id);
    assert!(Either::try_from("direct_1724f7cd-15b8-5e76-ba00-70d188c9b209").is_ok());
    #[cfg(feature = "encoding")]
    {
        use serde::Deserialize;
        let input = serde::de::value::StrDeserializer::<serde::de::value::Error>::new(
            "direct_1724f7cd-15b8-5e76-ba00-70d188c9b209",
        );
        assert!(Either::deserialize(input).is_ok());
    }
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_templates() {
        assert!(super::exercise(), "public typed recipe integration");
    }
}
