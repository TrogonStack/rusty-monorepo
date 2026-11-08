use super::*;

#[test]
fn one_opinion_decides_unless_the_operator_asks_for_a_panel() {
    assert_eq!(JudgeVotes::default().count(), 1);
    assert!(JudgeVotes::default().is_single());
}

#[test]
fn an_assertion_nobody_judges_is_refused() {
    assert!(JudgeVotes::parse(0).is_err());
}

#[test]
fn a_panel_that_could_tie_is_refused() {
    assert!(JudgeVotes::parse(2).is_err());
    assert!(JudgeVotes::parse(4).is_err());
    assert_eq!(JudgeVotes::parse(3).unwrap().count(), 3);
    assert_eq!(JudgeVotes::parse(5).unwrap().count(), 5);
}

#[test]
fn a_vote_count_is_parsed_from_the_command_line_form() {
    assert_eq!("3".parse::<JudgeVotes>().unwrap().count(), 3);
    assert!("2".parse::<JudgeVotes>().is_err());
    assert!("some".parse::<JudgeVotes>().is_err());
}

#[test]
fn every_opinion_is_numbered() {
    assert_eq!(
        JudgeVotes::parse(3).unwrap().ballots().collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn the_majority_answer_is_what_the_panel_reports() {
    let verdict = tally_opinions([(true, "a"), (false, "b"), (true, "c")]).unwrap();
    assert!(verdict.tally.majority_passed());
    assert_eq!(verdict.tally, JudgeVoteTally { passed: 2, failed: 1 });
    assert_eq!(verdict.tally.total(), 3);
    assert!(!verdict.tally.is_unanimous());
}

/// The losing side's evidence argues for the answer the panel did not give, so
/// showing it would leave a reader with a `passed` value its own evidence contradicts.
#[test]
fn the_evidence_shown_argues_for_the_answer_reported() {
    let verdict = tally_opinions([(true, "for"), (false, "against"), (false, "against-too")]).unwrap();
    assert!(!verdict.tally.majority_passed());
    assert_eq!(verdict.opinion, "against");
}

#[test]
fn a_unanimous_panel_says_so() {
    let verdict = tally_opinions([(true, "a"), (true, "b"), (true, "c")]).unwrap();
    assert!(verdict.tally.is_unanimous());
    assert_eq!(verdict.tally, JudgeVoteTally { passed: 3, failed: 0 });
}

#[test]
fn a_panel_that_was_never_asked_decides_nothing() {
    assert!(tally_opinions(Vec::<(bool, &str)>::new()).is_none());
}

#[cfg(test)]
mod schema_agrees_with_the_parser_tests;
