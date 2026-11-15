//! How many times one cell of the matrix is drawn.
//!
//! An agent asked the same question twice does not answer it the same way twice, so a
//! single run of a case is one draw from a distribution rather than a measurement of the
//! skill. A score built from single draws moves between passes for reasons that have
//! nothing to do with the skill, and a comparison between two arms built from single
//! draws reports a difference that is mostly the spread of each arm.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::RangeInclusive;
use std::str::FromStr;

/// The number of draws taken of every (case, scenario) cell.
///
/// Three by default. One draw reports no spread at all, and two cannot say which of the
/// pair was the outlier; three is the smallest count that shows a cell is unstable while
/// still costing an amount an operator will accept for every pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct AttemptCount(u32);

impl AttemptCount {
    /// What a pass draws unless the operator says otherwise.
    pub const fn recommended() -> Self {
        Self(3)
    }

    /// One draw, which is a run rather than a sample.
    pub const fn single() -> Self {
        Self(1)
    }

    pub fn parse(draws: u32) -> Result<Self, String> {
        if draws == 0 {
            return Err("attempts must be at least 1: a cell nobody draws is not reported".to_string());
        }
        Ok(Self(draws))
    }

    pub fn count(self) -> u32 {
        self.0
    }

    /// Whether the pass takes a single draw, which reports a run and not a distribution.
    pub fn is_single(self) -> bool {
        self.0 <= 1
    }

    /// The attempt numbers to record, `1..=N`.
    pub fn draws(self) -> RangeInclusive<u32> {
        1..=self.0
    }

    /// How many runs a given number of cells expands into.
    pub fn runs_for(self, cells: usize) -> usize {
        cells.saturating_mul(self.0 as usize)
    }
}

impl Default for AttemptCount {
    fn default() -> Self {
        Self::recommended()
    }
}

impl<'de> Deserialize<'de> for AttemptCount {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = u32::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for AttemptCount {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AttemptCount".into()
    }

    /// Written by hand rather than derived, and kept as strict as `Deserialize`, because a
    /// schema looser than its own parser lets `eval verify --mode strict` call a suite
    /// conformant that `eval run` then refuses to read.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "How many draws are taken of this cell. At least 1: a cell nobody draws is not reported.",
            "type": "integer",
            "minimum": 1,
            "maximum": 4294967295u32
        })
    }
}

impl fmt::Display for AttemptCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for AttemptCount {
    type Err = String;

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The established command-line parser contract returns a field-specific String validation message instead of a source-bearing error"
        )
    )]
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let draws: u32 = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not an attempt count"))?;
        Self::parse(draws)
    }
}

#[cfg(test)]
mod tests;
