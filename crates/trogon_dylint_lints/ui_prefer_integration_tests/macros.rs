// edition:2024
// compile-flags: --test --crate-type=lib
#![allow(test_module_naming, inline_module_block, reason = "this fixture isolates macro visibility in test placement")]

macro_rules! private_production_macro {
    () => { 7 };
}

#[macro_export]
macro_rules! exported_macro {
    () => { 7 };
}

#[cfg(test)]
mod tests {
    macro_rules! test_helper {
        () => { 7 };
    }

    #[test]
    fn private_macro_stays_here() {
        assert_eq!(private_production_macro!(), 7);
    }

    #[test]
    fn exported_macro_can_move() {
        assert_eq!(exported_macro!(), 7);
    }

    #[test]
    fn test_helper_macro_can_move() {
        assert_eq!(test_helper!(), 7);
    }
}
