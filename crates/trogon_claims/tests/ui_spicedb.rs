#![cfg(feature = "spicedb")]

#[test]
fn authorization_inputs_refuse_what_must_never_authorize() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui_spicedb/fail/*.rs");
}
