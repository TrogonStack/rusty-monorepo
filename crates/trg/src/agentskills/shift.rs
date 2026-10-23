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
mod tests {
    use super::*;

    fn shift(left: &[f64], right: &[f64]) -> Shift {
        Shift::between(left, right).expect("both arms clear the floor")
    }

    #[test]
    fn an_arm_below_the_floor_has_no_shift_at_all() {
        let enough = [1.0, 2.0, 3.0, 4.0];
        let three = [1.0, 2.0, 3.0];

        assert!(Shift::between(&three, &enough).is_none());
        assert!(Shift::between(&enough, &three).is_none());
        assert!(Shift::between(&[], &enough).is_none());
    }

    #[test]
    fn a_reading_that_is_not_a_number_is_refused_rather_than_sorted() {
        let left = [1.0, 2.0, f64::NAN, 4.0];
        let right = [1.0, 2.0, 3.0, 4.0];

        assert!(Shift::between(&left, &right).is_none());
    }

    #[test]
    fn the_smallest_admitted_arms_reach_the_outermost_differences() {
        assert_eq!(exact_lower_rank(4, 4), 1);
        assert_eq!(exact_lower_rank(3, 3), 0);
        assert_eq!(lower_rank(3, 3), None);
    }

    #[test]
    fn the_exact_ranks_match_the_published_table() {
        assert_eq!(exact_lower_rank(5, 5), 3);
        assert_eq!(exact_lower_rank(6, 6), 6);
        assert_eq!(exact_lower_rank(8, 8), 14);
        assert_eq!(exact_lower_rank(10, 10), 24);
        assert_eq!(exact_lower_rank(4, 10), 6);
    }

    #[test]
    fn the_approximation_lands_within_a_rank_of_the_exact_count_where_both_apply() {
        for (left, right) in [(40, 40), (60, 80), (100, 100)] {
            let exact = exact_lower_rank(left, right) as i64;
            let approximate = approximate_lower_rank(left, right) as i64;
            assert!(
                (exact - approximate).abs() <= 1,
                "{left}x{right}: exact {exact}, approximate {approximate}"
            );
        }
    }

    #[test]
    fn a_constant_offset_is_recovered_exactly() {
        let right = [100.0, 140.0, 90.0, 300.0, 120.0];
        let left: Vec<f64> = right.iter().map(|value| value + 50.0).collect();

        let measured = shift(&left, &right);

        assert_eq!(measured.estimate(), 50.0);
        assert!(measured.interval().low() <= 50.0 && measured.interval().high() >= 50.0);
    }

    #[test]
    fn one_runaway_run_does_not_drag_the_estimate() {
        let right = [100.0, 110.0, 120.0, 130.0];
        let left = [100.0, 110.0, 120.0, 100_000.0];

        let measured = shift(&left, &right);

        assert!(
            measured.estimate().abs() <= 10.0,
            "estimate was {}",
            measured.estimate()
        );
    }

    #[test]
    fn arms_that_never_overlap_are_told_apart() {
        let right = [100.0, 110.0, 120.0, 130.0, 105.0, 115.0];
        let left = [200.0, 210.0, 220.0, 230.0, 205.0, 215.0];

        let measured = shift(&left, &right);

        assert!(
            measured.interval().low() > 0.0,
            "interval was {:?}",
            measured.interval()
        );
    }

    #[test]
    fn arms_drawn_from_the_same_runs_are_not_told_apart() {
        let runs = [100.0, 250.0, 130.0, 400.0, 90.0];

        let measured = shift(&runs, &runs);

        assert_eq!(measured.estimate(), 0.0);
        assert!(!measured.interval().separates_the_arms());
    }

    #[test]
    fn readings_that_all_tie_give_a_point_interval_at_their_difference() {
        let left = [5.0, 5.0, 5.0, 5.0];
        let right = [3.0, 3.0, 3.0, 3.0];

        let measured = shift(&left, &right);

        assert_eq!(measured.estimate(), 2.0);
        assert_eq!(measured.interval().low(), 2.0);
        assert_eq!(measured.interval().high(), 2.0);
    }

    #[test]
    fn heavy_ties_keep_the_interval_closed_around_the_tied_value() {
        let left = [10.0, 10.0, 10.0, 11.0, 10.0];
        let right = [10.0, 10.0, 9.0, 10.0, 10.0];

        let measured = shift(&left, &right);

        assert_eq!(measured.estimate(), 0.0);
        assert!(measured.interval().low() <= 0.0 && measured.interval().high() >= 0.0);
    }

    #[test]
    fn the_same_runs_in_any_order_give_the_same_answer() {
        let left = [7.0, 3.0, 9.0, 1.0, 4.0];
        let right = [2.0, 8.0, 6.0, 5.0];
        let mut reversed_left = left;
        reversed_left.reverse();

        assert_eq!(shift(&left, &right), shift(&reversed_left, &right));
    }

    #[test]
    fn a_large_arm_takes_the_approximate_path_and_still_separates_a_real_shift() {
        let right: Vec<f64> = (0..120).map(|run| (run % 17) as f64).collect();
        let left: Vec<f64> = right.iter().map(|value| value + 5.0).collect();

        let measured = shift(&left, &right);

        assert!(left.len() * right.len() > Shift::EXACT_PAIR_LIMIT);
        assert!(measured.interval().low() > 0.0);
        assert_eq!(measured.estimate(), 5.0);
    }
}
