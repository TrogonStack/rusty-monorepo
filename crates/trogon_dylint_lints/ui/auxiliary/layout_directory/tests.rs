use super::private_value;

#[test]
fn reads_owner() {
    assert_eq!(private_value(), 3);
}
