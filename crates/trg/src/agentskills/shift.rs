//! How far one arm's per-run readings sit from another's, and how sure that is.
//!
//! A run's duration, tokens and cost are skewed (one slow tool call or one long retry
//! dominates the mean), tied (a harness reports whole seconds, a price list rounds), and
//! drawn a handful at a time. A difference of means, or of medians with a normal-theory
//! interval, assumes the opposite on all three counts. The Hodges-Lehmann shift and the
//! Moses interval around it assume only that the two arms have the same shape, read
//! every run without letting any one of them dominate, and come from rank counts that
//! are exact at the sizes an eval arm is drawn at, so the same runs always give the same
//! answer with no seed to record.
//!
//! The arms are compared unpaired. Pairing by case and attempt needs both runs of a pair
//! to have completed and been measured; a failure, a skip or a narrower case selection on
//! one side leaves the other side's run with no partner, and dropping it would quietly
//! shrink and bias whichever arm failed more. The unpaired interval keeps every measured
//! run and pays for it in width, since case-to-case variation stays in the noise.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::dispersion::SampleFloor;
use super::proportion::{Interval, DEVIATE};

/// The shift between two arms of per-run readings: `estimate` is how far the left arm
/// sits from the right one, and `interval` is the 95% range the runs leave around it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Shift {
    estimate: f64,
    interval: Interval,
}

impl Shift {
    /// Above this many pairwise differences the exact rank count is replaced by its normal
    /// approximation, which by then sits within one rank of it, so a very large arm costs
    /// a sort instead of a polynomial of its own size.
    const EXACT_PAIR_LIMIT: usize = 10_000;

    /// Nothing when either arm falls below the floor `Dispersion` already uses: an arm of
    /// three runs cannot say which of them was typical, and the interval built from it
    /// would describe the draw rather than the arm.
    pub fn between(left: &[f64], right: &[f64]) -> Option<Self> {
        let floor = SampleFloor::dispersion();
        if !floor.admits(left.len()) || !floor.admits(right.len()) {
            return None;
        }
        if left.iter().chain(right).any(|value| !value.is_finite()) {
            return None;
        }

        let mut differences: Vec<f64> = left
            .iter()
            .flat_map(|left_value| right.iter().map(move |right_value| left_value - right_value))
            .collect();
        differences.sort_by(f64::total_cmp);

        let rank = lower_rank(left.len(), right.len())?;
        let pairs = differences.len();
        let interval = Interval::parse(differences[rank - 1], differences[pairs - rank]).ok()?;
        Some(Self {
            estimate: median_of_sorted(&differences),
            interval,
        })
    }

    pub fn estimate(self) -> f64 {
        self.estimate
    }

    pub fn interval(self) -> Interval {
        self.interval
    }
}

fn median_of_sorted(sorted: &[f64]) -> f64 {
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

/// The 1-based rank, counted from each end of the sorted pairwise differences, of the
/// endpoints of a 95% interval: the largest `k` with `P(U <= k - 1) <= 0.025` under the
/// null distribution of the Mann-Whitney count `U`.
///
/// Nothing when even the outermost differences cannot reach 95%. Ties are not corrected
/// for: the interval is closed, so a tied endpoint widens it rather than narrowing it.
fn lower_rank(left_runs: usize, right_runs: usize) -> Option<usize> {
    let pairs = left_runs * right_runs;
    let rank = if pairs <= Shift::EXACT_PAIR_LIMIT {
        exact_lower_rank(left_runs.min(right_runs), left_runs.max(right_runs))
    } else {
        approximate_lower_rank(left_runs, right_runs)
    };
    (rank >= 1).then_some(rank)
}

/// The null distribution of `U` is the coefficient list of the Gaussian binomial
/// `[smaller + larger choose smaller]`, built here as a product of `(1 - q^(larger + i))
/// / (1 - q^i)`. Only the lower half is needed, since the distribution is symmetric and
/// a 2.5% tail never reaches its centre.
fn exact_lower_rank(smaller: usize, larger: usize) -> usize {
    let degree = smaller * larger / 2;
    let mut counts = vec![0.0_f64; degree + 1];
    counts[0] = 1.0;
    let mut arrangements = 1.0_f64;

    for step in 1..=smaller {
        for count in step..=degree {
            counts[count] += counts[count - step];
        }
        let removed = larger + step;
        for count in (removed..=degree).rev() {
            counts[count] -= counts[count - removed];
        }
        arrangements = arrangements * removed as f64 / step as f64;
    }

    let tail = 0.025;
    let mut cumulative = 0.0;
    let mut rank = 0;
    for count in counts {
        cumulative += count / arrangements;
        if cumulative > tail {
            break;
        }
        rank += 1;
    }
    rank
}

fn approximate_lower_rank(left_runs: usize, right_runs: usize) -> usize {
    let (left, right) = (left_runs as f64, right_runs as f64);
    let centre = left * right / 2.0;
    let spread = (left * right * (left + right + 1.0) / 12.0).sqrt();
    (centre + 0.5 - DEVIATE * spread).floor().max(0.0) as usize
}

#[cfg(test)]
#[path = "shift_tests.rs"]
mod tests;
