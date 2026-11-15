// edition:2024
// compile-flags: --test
// rustc-env:CARGO_CRATE_NAME=custom_library
#![allow(test_module_naming, inline_module_block, reason = "this fixture models Cargo's custom library test invocation")]

pub fn exported() -> bool { true }

#[cfg(test)]
mod tests {
    #[test]
    fn public_contract() { assert!(super::exported()); }
}
