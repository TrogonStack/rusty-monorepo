// ignore-test: compiled as the child body of main.rs

use super::private_value;

#[test]
fn accesses_private_binary_root_item() {
    assert_eq!(private_value(), 8);
}
