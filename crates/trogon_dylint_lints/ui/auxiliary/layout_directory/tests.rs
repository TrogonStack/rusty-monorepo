use super::private_value;

mod parse_tests;

#[test]
fn reads_owner() {
    assert_eq!(private_value(), 3);
}
