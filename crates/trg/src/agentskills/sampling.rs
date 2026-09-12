//! How many times one cell of the matrix is drawn.
//!
//! An agent asked the same question twice does not answer it the same way twice, so a
//! single run of a case is one draw from a distribution rather than a measurement of the
//! skill. A score built from single draws moves between passes for reasons that have
//! nothing to do with the skill, and a comparison between two arms built from single
//! draws reports a difference that is mostly the spread of each arm.

use std::fmt;
use std::ops::RangeInclusive;
use std::str::FromStr;

/// The number of draws taken of every (case, scenario) cell.
///
/// Three by default. One draw reports no spread at all, and two cannot say which of the
/// pair was the outlier; three is the smallest count that shows a cell is unstable while
/// still costing an amount an operator will accept for every pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

impl fmt::Display for AttemptCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for AttemptCount {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let draws: u32 = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not an attempt count"))?;
        Self::parse(draws)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default is the whole point of the type: a pass has to sample by default,
    /// because an operator who never passes the flag is exactly the one reading a single
    /// draw as if it were a measurement.
    #[test]
    fn a_pass_samples_a_cell_more_than_once_by_default() {
        assert_eq!(AttemptCount::default().count(), 3);
        assert!(!AttemptCount::default().is_single());
    }

    #[test]
    fn a_cell_nobody_draws_is_refused() {
        assert!(AttemptCount::parse(0).is_err());
        assert_eq!(AttemptCount::parse(1).unwrap(), AttemptCount::single());
    }

    #[test]
    fn an_attempt_count_is_parsed_from_the_command_line_form() {
        assert_eq!("5".parse::<AttemptCount>().unwrap().count(), 5);
        assert!("0".parse::<AttemptCount>().is_err());
        assert!("some".parse::<AttemptCount>().is_err());
    }

    #[test]
    fn every_draw_of_a_cell_is_numbered_and_counted() {
        let three = AttemptCount::recommended();
        assert_eq!(three.draws().collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!(three.runs_for(4), 12);
        assert_eq!(AttemptCount::single().runs_for(4), 4);
    }
}
