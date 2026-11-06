#[test]
fn core_claims_reject_invalid_type_transitions() -> Result<(), Box<dyn std::error::Error>> {
    let cases = trybuild::TestCases::new();
    let mut fixtures = std::fs::read_dir("tests/ui/fail")?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    fixtures.sort();
    let mut selected = false;
    for path in fixtures {
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        let literal = ["object_id_", "object_type_", "principal_kind_"]
            .iter()
            .any(|prefix| name.starts_with(prefix));
        if path.extension().is_some_and(|extension| extension == "rs") && !literal {
            selected = true;
            cases.compile_fail(path);
        }
    }
    assert!(selected, "no core compile-fail fixtures found");
    Ok(())
}

#[cfg(feature = "macros")]
#[test]
fn literal_macros_diagnose_bad_literals_on_the_literal() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/pass/*.rs");
    cases.compile_fail("tests/ui/fail/object_id_*.rs");
    cases.compile_fail("tests/ui/fail/object_type_*.rs");
    cases.compile_fail("tests/ui/fail/principal_kind_*.rs");
}
