// edition:2024
// compile-flags: --test --crate-type=lib
// rustc-env:CARGO_TARGET_TMPDIR=target/tmp

#[test]
fn integration_test_is_already_placed() {
    assert_eq!(String::from("hello").len(), 5);
}
