//! How many opinions one assertion is graded on.
//!
//! A model asked whether an assertion holds does not answer the same way every time, and
//! an assertion whose material sits near the edge of the judge's decision flips between
//! passes for reasons that have nothing to do with the run being graded. A single opinion
//! cannot tell that case apart from one the judge is sure about, so both arrive as a flat
//! `passed: false` and a reader chases a regression that was never there. Asking more than
//! once and taking the majority narrows that spread, and recording how the panel split
//! says which assertions are worth writing more precisely.

use std::fmt;
use std::ops::RangeInclusive;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The number of opinions taken on every assertion an LLM judge decides.
///
/// Always odd, because a panel that ties has decided nothing, and reporting a tie as
/// either answer would invent the confidence the flag exists to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct JudgeVotes(u32);

impl JudgeVotes {
    /// One opinion, which is a judgement rather than a panel.
    pub const fn single() -> Self {
        Self(1)
    }

    pub fn parse(votes: u32) -> Result<Self, String> {
        if votes == 0 {
            return Err("grader votes must be at least 1: an assertion nobody judges has no result".to_string());
        }
        if votes.is_multiple_of(2) {
            return Err(format!(
                "grader votes must be odd, so the panel cannot tie: {votes} splits evenly with no majority to report"
            ));
        }
        Ok(Self(votes))
    }

    pub fn count(self) -> u32 {
        self.0
    }

    /// Whether one opinion decides, which reports a judgement and not a panel.
    pub fn is_single(self) -> bool {
        self.0 <= 1
    }

    /// The opinion numbers to take, `1..=N`.
    pub fn ballots(self) -> RangeInclusive<u32> {
        1..=self.0
    }
}

impl Default for JudgeVotes {
    fn default() -> Self {
        Self::single()
    }
}

impl fmt::Display for JudgeVotes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for JudgeVotes {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let votes: u32 = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not a vote count"))?;
        Self::parse(votes)
    }
}

/// How a panel of judges split on one assertion.
///
/// Recorded so a reader can tell an assertion the judge was sure about from one it was
/// divided on. A divided panel is a statement about the assertion, not about the run: it
/// says the material admits both readings, which is a prompt to write the assertion more
/// precisely rather than to go looking at the skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct JudgeVoteTally {
    pub passed: u32,
    pub failed: u32,
}

impl JudgeVoteTally {
    pub fn total(self) -> u32 {
        self.passed + self.failed
    }

    pub fn majority_passed(self) -> bool {
        self.passed > self.failed
    }

    pub fn is_unanimous(self) -> bool {
        self.passed == 0 || self.failed == 0
    }
}

/// What a panel decided, and the opinion that speaks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgePanelVerdict<T> {
    pub opinion: T,
    pub tally: JudgeVoteTally,
}

/// Fold the opinions taken on one assertion into the answer the majority gave.
///
/// The opinion returned is the first one on the winning side, so the evidence a reader
/// sees is evidence for the answer they are reading rather than for the side that lost.
pub fn tally_opinions<T>(opinions: impl IntoIterator<Item = (bool, T)>) -> Option<JudgePanelVerdict<T>> {
    let opinions: Vec<(bool, T)> = opinions.into_iter().collect();
    if opinions.is_empty() {
        return None;
    }

    let passed = opinions.iter().filter(|(passed, _)| *passed).count() as u32;
    let tally = JudgeVoteTally {
        passed,
        failed: opinions.len() as u32 - passed,
    };
    let winning_side = tally.majority_passed();
    let opinion = opinions
        .into_iter()
        .find(|(passed, _)| *passed == winning_side)
        .map(|(_, opinion)| opinion)?;

    Some(JudgePanelVerdict { opinion, tally })
}

#[cfg(test)]
mod tests {
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
}
