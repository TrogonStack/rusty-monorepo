//! How many harnesses one invocation may have running at once.
//!
//! A full pass costs a model call per case per scenario per attempt, and one harness at a
//! time makes the wall clock the product of all three. That is what makes sampling a case
//! more than once expensive, and sampling is the only way a score from a non-deterministic
//! agent means anything.

use std::fmt;
use std::str::FromStr;

/// A lane count between one and [`RunConcurrency::MAX`].
///
/// Bounded on purpose. Every lane still pays for its own model calls, so concurrency buys
/// wall clock and nothing else, and the lanes share one account's rate limit: past a
/// handful the provider starts refusing work, and a refused run is a transient failure
/// that costs a retry rather than time saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RunConcurrency(usize);

impl RunConcurrency {
    pub const MAX: usize = 8;

    /// One harness at a time.
    pub const fn serial() -> Self {
        Self(1)
    }

    pub fn parse(lanes: usize) -> Result<Self, String> {
        if lanes == 0 {
            return Err("concurrency must be at least 1: a run needs a lane to run in".to_string());
        }
        if lanes > Self::MAX {
            return Err(format!(
                "concurrency must be at most {max}: lanes share one account's rate limit, and past {max} the provider refuses work faster than the lanes finish it",
                max = Self::MAX
            ));
        }
        Ok(Self(lanes))
    }

    pub fn lanes(self) -> usize {
        self.0
    }

    /// Whether the work has to be done one item at a time.
    pub fn is_serial(self) -> bool {
        self.0 <= 1
    }

    /// Lanes worth starting for a given amount of work.
    ///
    /// A lane with nothing to take is a thread spawned to observe that the queue is empty.
    pub fn lanes_for(self, items: usize) -> usize {
        self.0.min(items)
    }
}

impl Default for RunConcurrency {
    fn default() -> Self {
        Self::serial()
    }
}

impl fmt::Display for RunConcurrency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for RunConcurrency {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let lanes: usize = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not a lane count"))?;
        Self::parse(lanes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_one_harness_at_a_time() {
        assert!(RunConcurrency::default().is_serial());
        assert_eq!(RunConcurrency::default().lanes(), 1);
    }

    #[test]
    fn a_lane_count_outside_the_supported_range_is_refused() {
        assert!(RunConcurrency::parse(0).is_err(), "a run needs a lane");
        assert!(RunConcurrency::parse(RunConcurrency::MAX).is_ok());
        assert!(RunConcurrency::parse(RunConcurrency::MAX + 1).is_err());
    }

    #[test]
    fn a_lane_count_is_parsed_from_the_command_line_form() {
        assert_eq!("4".parse::<RunConcurrency>().unwrap().lanes(), 4);
        assert!("many".parse::<RunConcurrency>().is_err());
        assert!("0".parse::<RunConcurrency>().is_err());
    }

    /// A lane with nothing to take is a thread spawned to find the queue empty.
    #[test]
    fn no_more_lanes_are_started_than_there_is_work_for() {
        let four = RunConcurrency::parse(4).unwrap();
        assert_eq!(four.lanes_for(2), 2);
        assert_eq!(four.lanes_for(9), 4);
        assert_eq!(four.lanes_for(0), 0);
    }
}
