// edition:2024
// compile-flags: --test --crate-type=lib
#![allow(dead_code, test_module_naming, inline_module_block, reason = "this fixture isolates crate-only construction and patterns")]

#[non_exhaustive]
pub struct Extensible {
    pub value: u8,
}
#[non_exhaustive]
pub struct Tuple(pub u8);
#[non_exhaustive]
pub struct Unit;
#[non_exhaustive]
pub enum ExtensibleEnum {
    Known,
    #[non_exhaustive]
    Record { value: u8 },
    #[non_exhaustive]
    Tuple(u8),
    #[non_exhaustive]
    Unit,
}

pub fn factory() -> Extensible {
    Extensible { value: 7 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_only_struct_construction() {
        assert_eq!(super::Extensible { value: 7 }.value, 7);
    }

    #[test]
    fn crate_only_tuple_construction() {
        assert_eq!(super::Tuple(7).0, 7);
    }

    #[test]
    fn crate_only_unit_construction() {
        let _ = super::Unit;
    }

    #[test]
    fn crate_only_variant_construction() {
        let _ = super::ExtensibleEnum::Record { value: 7 };
        let _ = super::ExtensibleEnum::Tuple(7);
        let _ = super::ExtensibleEnum::Unit;
    }

    #[test]
    fn crate_only_exhaustive_pattern() {
        let super::Extensible { value } = super::factory();
        assert_eq!(value, 7);
    }

    #[test]
    fn public_factory_and_field_can_move() {
        assert_eq!(super::factory().value, 7);
    }

    #[test]
    fn public_open_pattern_can_move() {
        let super::Extensible { value, .. } = super::factory();
        assert_eq!(value, 7);
    }

    #[test]
    fn ordinary_variant_can_move() {
        assert!(matches!(super::ExtensibleEnum::Known, super::ExtensibleEnum::Known));
    }
}
