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
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// The number of opinions taken on every assertion an LLM judge decides.
///
/// Always odd, because a panel that ties has decided nothing, and reporting a tie as
/// either answer would invent the confidence the flag exists to measure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[schemars(schema_with = "judge_votes_schema")]
pub struct JudgeVotes(u32);

/// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
/// schema looser than its own parser lets `eval verify --mode strict` call a report
/// conformant that `grade`, `benchmark` and `compare` then refuse to read.
fn judge_votes_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "integer",
        "minimum": 1,
        "maximum": 4294967295u32,
        "not": { "multipleOf": 2 }
    })
}

impl<'de> Deserialize<'de> for JudgeVotes {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

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

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The established command-line parser contract returns a field-specific String validation message instead of a source-bearing error"
        )
    )]
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
mod tests;
