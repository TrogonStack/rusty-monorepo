// ignore-test: compiled as the child body of lib.rs

use super::private_value;

#[test]
fn accesses_private_crate_root_item() {
    assert_eq!(private_value(), 5);
}
