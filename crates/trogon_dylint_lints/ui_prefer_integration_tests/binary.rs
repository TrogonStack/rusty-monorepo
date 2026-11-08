// edition:2024
// compile-flags: --test
// rustc-env:CARGO_BIN_NAME=custom_binary
#![allow(test_module_naming, inline_module_block, reason = "a binary's Rust API cannot be imported by integration tests")]

pub fn exported() -> bool { true }

#[cfg(test)]
mod tests {
    #[test]
    fn binary_contract() { assert!(super::exported()); }
}
