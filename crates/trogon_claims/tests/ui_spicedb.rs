#![cfg(feature = "spicedb")]

#[test]
fn authorization_inputs_refuse_what_must_never_authorize() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui_spicedb/fail/*.rs");
}

#[cfg(feature = "macros")]
#[test]
fn spicedb_literal_macros_diagnose_bad_literals_on_the_literal() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui_spicedb/pass/*.rs");
    cases.compile_fail("tests/ui/fail/object_id_*.rs");
    cases.compile_fail("tests/ui/fail/object_type_*.rs");
}
