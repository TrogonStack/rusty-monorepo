#[test]
fn literal_macros_diagnose_bad_literals_on_the_literal() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/pass/*.rs");
    cases.compile_fail("tests/ui/fail/*.rs");
}
