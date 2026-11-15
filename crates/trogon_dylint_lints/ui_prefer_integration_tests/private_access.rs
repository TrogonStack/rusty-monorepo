// edition:2024
// compile-flags: --test --crate-type=lib
#![allow(dead_code, test_module_naming, inline_module_block, reason = "this fixture isolates private-access exceptions")]

fn private_function() -> u8 { 7 }
pub(crate) fn crate_function() -> u8 { 7 }
struct PrivateType;
mod hidden {
    pub fn function() -> u8 { 7 }
}
pub struct Api {
    private: u8,
    pub visible: u8,
}
impl Api {
    pub fn from_value(value: u8) -> Self { Self { private: value, visible: value } }
    fn private_method(&self) -> u8 { self.private }
}

#[cfg(test)]
mod tests {
    fn helper() -> u8 { super::private_function() }
    fn second_helper() -> u8 { helper() }
    fn private_type_helper() -> Option<super::PrivateType> { None }

    #[test]
    fn private_function() { assert_eq!(super::private_function(), 7); }
    #[test]
    fn crate_visibility() { assert_eq!(super::crate_function(), 7); }
    #[test]
    fn inaccessible_module() { assert_eq!(super::hidden::function(), 7); }
    #[test]
    fn helper_chain() { assert_eq!(second_helper(), 7); }
    #[test]
    fn type_annotation() {
        let value: Option<super::PrivateType> = None;
        assert!(value.is_none());
    }
    #[test]
    fn helper_signature() { assert!(private_type_helper().is_none()); }
    #[test]
    fn method() { assert_eq!(super::Api::from_value(7).private_method(), 7); }
    #[test]
    fn field_read() { assert_eq!(super::Api::from_value(7).private, 7); }
    #[test]
    fn field_construction() {
        let value = super::Api { private: 7, visible: 7 };
        assert_eq!(value.visible, 7);
    }
    #[test]
    fn field_pattern() {
        let super::Api { private, .. } = super::Api::from_value(7);
        assert_eq!(private, 7);
    }
    #[test]
    fn closure() {
        let call = || super::private_function();
        assert_eq!(call(), 7);
    }
}
