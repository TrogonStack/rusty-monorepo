use super::private_value;

#[path = "tests/parse_tests.rs"]
#[allow(redundant_module_path, reason = "a `#[path]`-loaded file resolves `mod parse_tests;` beside itself")]
mod parse_tests;

#[test]
fn reads_owner() {
    assert_eq!(private_value(), 4);
}
