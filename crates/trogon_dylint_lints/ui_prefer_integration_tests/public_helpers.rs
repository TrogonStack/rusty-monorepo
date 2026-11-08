// edition:2024
// compile-flags: --test --crate-type=lib
#![allow(dead_code, test_module_naming, inline_module_block, reason = "this fixture isolates public contracts reached through test helpers")]

fn private_detail() -> u8 { 7 }
mod hidden {
    pub fn exported() -> u8 { super::private_detail() }
}
pub use hidden::exported;

pub struct Api {
    pub value: u8,
}
pub trait Contract {
    fn value(&self) -> u8;
}
impl Contract for Api {
    fn value(&self) -> u8 { private_detail() }
}

#[cfg(test)]
mod tests {
    use super::Contract;

    fn public_helper() -> u8 { super::exported() }
    fn chained_helper() -> u8 { public_helper() }
    fn private_helper() -> u8 { super::private_detail() }
    struct Fixture { value: u8 }
    impl Fixture {
        fn run(&self) -> u8 { self.value + public_helper() }
    }

    #[test]
    fn reexported_api() { assert_eq!(super::hidden::exported(), 7); }
    #[test]
    fn helper_chain() { assert_eq!(chained_helper(), 7); }
    #[test]
    fn private_sibling_does_not_exempt_suite() { assert_eq!(private_helper(), 7); }
    #[test]
    fn trait_contract() { assert_eq!(super::Api { value: 7 }.value(), 7); }
    #[test]
    fn public_field_pattern() {
        let super::Api { value } = super::Api { value: 7 };
        assert_eq!(value, 7);
    }
    #[test]
    fn test_fixture() { assert_eq!(Fixture { value: 0 }.run(), 7); }
    #[test]
    fn closure_public_contract() {
        let call = || chained_helper();
        assert_eq!(call(), 7);
    }
    #[test]
    fn recursive_helper_without_private_dependency() {
        fn count(value: u8) -> u8 {
            if value == 0 { super::exported() } else { count(value - 1) }
        }
        assert_eq!(count(1), 7);
    }
}
