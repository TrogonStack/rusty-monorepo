use super::*;

/// A blank appendix still puts the flag on the command line, so the run is recorded as
/// having been steered while nothing was added to steer it.
#[test]
fn a_blank_appendix_is_refused_rather_than_passed_through() {
    assert!(SystemPromptAppendix::parse("   ").is_err());
}

#[test]
fn an_appendix_reaches_the_harness_exactly_as_written() {
    let appendix = SystemPromptAppendix::parse("  Answer in British English.  ").unwrap();
    assert_eq!(appendix.as_str(), "  Answer in British English.  ");
}

#[cfg(test)]
mod schema_agrees_with_the_parser_tests;
