use super::*;

#[test]
fn a_blank_name_is_refused_because_it_would_read_as_a_model_that_was_never_asked_for() {
    assert!(ModelName::parse("").is_err());
    assert!(ModelName::parse("\t ").is_err());
}

#[test]
fn an_unrecognized_name_is_admitted_because_the_harness_owns_that_list() {
    assert_eq!(
        ModelName::parse("some-model-we-have-never-heard-of").unwrap().as_str(),
        "some-model-we-have-never-heard-of"
    );
}

#[cfg(test)]
mod schema_agrees_with_the_parser_tests;
