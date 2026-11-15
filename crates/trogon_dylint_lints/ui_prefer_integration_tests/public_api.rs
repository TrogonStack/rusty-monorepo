// edition:2024
// compile-flags: --test --crate-type=lib
#![allow(test_module_naming, inline_module_block, reason = "this fixture isolates integration-test preference")]

pub fn parse(value: &str) -> usize {
    value.len()
}

#[cfg(test)]
mod tests {
    #[test]
    fn public_api() {
        assert_eq!(super::parse("abc"), 3);
    }
}
