#![no_std]

include!(concat!(env!("OUT_DIR"), "/recipes.rs"));

pub fn exercise() -> bool {
    let input = SkuIdentityV1Input {
        warehouse_id: "alpha",
        sku_code: "42",
    };
    let expected = recipes::uuid5(&SkuIdentityV1::NAMESPACE, b"warehouse/alpha/sku/42");
    SkuIdentityV1::uuid5(input) == expected
        && StaticIdentity::uuid5(()) == StaticIdentity::VALUE
        && EmptyIdentity::uuid5(()) == EmptyIdentity::VALUE
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_public_recipes() {
        assert!(super::exercise(), "generated recipe vectors");
    }
}
