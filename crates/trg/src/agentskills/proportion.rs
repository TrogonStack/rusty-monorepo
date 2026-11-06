//! How much a pass rate measured from a handful of draws is entitled to claim.
//!
//! A scenario arm is a few draws, not a survey. Subtracting one arm's rate from
//! another's produces a number that reads like a finding, and published on its own it
//! invites a reader to act on a difference the same skill would have produced again by
//! chance. The interval says how wide the answer actually is, so a difference that has
//! not separated the arms looks like one.

use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

/// The two-sided normal deviate for 95% coverage.
pub(crate) const DEVIATE: f64 = 1.959_963_985;

/// A rate together with the draws it was measured from.
///
/// The draws are kept rather than divided away because the rate alone cannot say how
/// much it is worth: two thirds out of three and two thousand out of three thousand are
/// the same number and not the same claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proportion {
    successes: usize,
    trials: usize,
}

impl Proportion {
    pub fn parse(successes: usize, trials: usize) -> Result<Self, String> {
        if trials == 0 {
            return Err("a proportion of nothing has no rate to report".to_string());
        }
        if successes > trials {
            return Err(format!(
                "{successes} successes out of {trials} draws counts a draw twice"
            ));
        }
        Ok(Self { successes, trials })
    }

    /// The proportion behind a passed/failed tally, or nothing when nothing was scored.
    pub fn observed(passed: usize, failed: usize) -> Option<Self> {
        Self::parse(passed, passed + failed).ok()
    }

    pub fn successes(self) -> usize {
        self.successes
    }

    pub fn trials(self) -> usize {
        self.trials
    }

    pub fn rate(self) -> f64 {
        self.successes as f64 / self.trials as f64
    }

    /// The Wilson score interval, which is asymmetric and stays inside `0.0..=1.0`.
    ///
    /// The textbook normal interval is neither: three passes out of three puts it at
    /// `1.0 ± 0.0`, reporting a certainty three draws cannot support, and it runs past
    /// the ends of the scale at rates near either one.
    pub fn interval(self) -> Interval {
        let n = self.trials as f64;
        let p = self.rate();
        let z2 = DEVIATE * DEVIATE;
        let denominator = 1.0 + z2 / n;
        let centre = (p + z2 / (2.0 * n)) / denominator;
        let half_width = DEVIATE / denominator * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
        Interval::parse((centre - half_width).max(0.0), (centre + half_width).min(1.0))
            .expect("a Wilson interval of a parsed proportion is finite and ordered")
    }
}

/// The range a measurement is consistent with.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Interval {
    low: f64,
    high: f64,
}

impl Interval {
    pub fn parse(low: f64, high: f64) -> Result<Self, String> {
        if !low.is_finite() || !high.is_finite() {
            return Err("an interval needs finite ends".to_string());
        }
        if high < low {
            return Err(format!("an interval from {low} to {high} runs backwards"));
        }
        Ok(Self { low, high })
    }

    pub fn low(self) -> f64 {
        self.low
    }

    pub fn high(self) -> f64 {
        self.high
    }

    pub fn width(self) -> f64 {
        self.high - self.low
    }

    /// Whether the interval rules out "the two arms are the same".
    pub fn separates_the_arms(self) -> bool {
        self.low > 0.0 || self.high < 0.0
    }
}

/// The interval around `left - right`, by Newcombe's hybrid score method.
///
/// The difference is built from each arm's own Wilson interval rather than from a pooled
/// normal approximation, so it keeps working at the rates an eval arm actually lands on:
/// all-pass and all-fail arms are ordinary results here, and are exactly where the
/// normal approximation reports a width of zero.
pub fn difference(left: Proportion, right: Proportion) -> Interval {
    let (l, r) = (left.interval(), right.interval());
    let difference = left.rate() - right.rate();
    let below = ((left.rate() - l.low()).powi(2) + (r.high() - right.rate()).powi(2)).sqrt();
    let above = ((l.high() - left.rate()).powi(2) + (right.rate() - r.low()).powi(2)).sqrt();
    Interval::parse(difference - below, difference + above)
        .expect("a difference interval built from two finite intervals is finite and ordered")
}

/// Whether at least one attempt passed, and whether every attempt passed, among one
/// case's own attempts. `None` when the case drew no graded attempt at all, so an
/// ungraded case does not silently count as a loss under either reading.
pub fn case_k_outcome(attempts_passed: impl IntoIterator<Item = bool>) -> Option<(bool, bool)> {
    let mut any = false;
    let mut all = true;
    let mut drew_one = false;
    for passed in attempts_passed {
        drew_one = true;
        any = any || passed;
        all = all && passed;
    }
    drew_one.then_some((any, all))
}

/// How many cases, out of how many drew a graded attempt, had at least one attempt pass
/// and how many had every attempt pass: the two counts [`case_k_outcome`] rolls up across
/// a scenario's cases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KCaseCounts {
    pub any_pass: usize,
    pub all_pass: usize,
    pub trials: usize,
}

pub fn k_case_counts(outcomes: impl IntoIterator<Item = (bool, bool)>) -> KCaseCounts {
    let mut counts = KCaseCounts::default();
    for (any, all) in outcomes {
        counts.trials += 1;
        counts.any_pass += any as usize;
        counts.all_pass += all as usize;
    }
    counts
}

/// A rate over cases together with the Wilson interval around it, serializable for a
/// report rather than kept as a [`Proportion`]: a reader of the report has no way to
/// recompute the interval from `rate` alone, so it rides alongside rather than being
/// dropped at the serialization boundary.
///
/// `rate` and `interval` are derived from `successes`/`trials` rather than independent
/// fields, so deserializing goes through [`Proportion::parse`] to keep a decoded value
/// as internally consistent as one built by [`ScoredRate::from_counts`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, JsonSchema)]
pub struct ScoredRate {
    pub successes: usize,
    pub trials: usize,
    /// Absent when `trials` is zero: nothing was measured.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub rate: Option<f64>,
    pub interval: Option<Interval>,
}

impl ScoredRate {
    pub fn from_counts(successes: usize, trials: usize) -> Self {
        let proportion = Proportion::parse(successes, trials).ok();
        Self {
            successes,
            trials,
            rate: proportion.map(Proportion::rate),
            interval: proportion.map(Proportion::interval),
        }
    }
}

impl<'de> Deserialize<'de> for ScoredRate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            successes: usize,
            trials: usize,
        }

        let raw = Raw::deserialize(deserializer)?;
        if raw.successes == 0 && raw.trials == 0 {
            return Ok(Self::default());
        }
        Proportion::parse(raw.successes, raw.trials)
            .map(|_| Self::from_counts(raw.successes, raw.trials))
            .map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_nobody_drew_is_not_a_proportion() {
        assert!(Proportion::parse(0, 0).is_err());
        assert!(Proportion::observed(0, 0).is_none());
    }

    #[test]
    fn more_successes_than_draws_is_refused_rather_than_reported() {
        assert!(Proportion::parse(4, 3).is_err());
    }

    #[test]
    fn three_passes_out_of_three_does_not_claim_certainty() {
        let interval = Proportion::parse(3, 3).expect("three of three").interval();

        assert!(interval.low() < 0.5, "a floor of {} reads as proof", interval.low());
        assert!((interval.high() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn an_interval_never_runs_past_the_ends_of_the_scale() {
        for trials in 1..=20 {
            for successes in 0..=trials {
                let interval = Proportion::parse(successes, trials)
                    .expect("a drawn proportion")
                    .interval();
                assert!(
                    interval.low() >= 0.0 && interval.high() <= 1.0,
                    "{successes}/{trials} gave {interval:?}"
                );
            }
        }
    }

    #[test]
    fn a_known_interval_matches_the_published_value() {
        let interval = Proportion::parse(81, 263).expect("a drawn proportion").interval();

        assert!((interval.low() - 0.2553).abs() < 5e-4, "low was {}", interval.low());
        assert!((interval.high() - 0.3662).abs() < 5e-4, "high was {}", interval.high());
    }

    #[test]
    fn more_draws_narrow_the_same_rate() {
        let few = Proportion::parse(2, 3).expect("a drawn proportion").interval();
        let many = Proportion::parse(200, 300).expect("a drawn proportion").interval();

        assert!(many.width() < few.width());
    }

    #[test]
    fn a_two_thirds_lead_over_three_draws_has_not_separated_the_arms() {
        let left = Proportion::parse(2, 3).expect("a drawn proportion");
        let right = Proportion::parse(0, 3).expect("a drawn proportion");

        let interval = difference(left, right);

        assert!(
            (left.rate() - right.rate() - 0.6667).abs() < 5e-4,
            "the subtraction moved"
        );
        assert!(
            !interval.separates_the_arms(),
            "a lead of two thirds over three draws read as a finding at {interval:?}"
        );
    }

    #[test]
    fn a_whole_sweep_of_enough_draws_does_separate_the_arms() {
        let left = Proportion::parse(40, 40).expect("a drawn proportion");
        let right = Proportion::parse(0, 40).expect("a drawn proportion");

        assert!(difference(left, right).separates_the_arms());
    }

    #[test]
    fn two_arms_that_drew_the_same_rate_straddle_no_difference() {
        let arm = Proportion::parse(5, 10).expect("a drawn proportion");

        let interval = difference(arm, arm);

        assert!(interval.low() < 0.0 && interval.high() > 0.0);
        assert!(!interval.separates_the_arms());
    }

    #[test]
    fn an_interval_that_runs_backwards_is_refused() {
        assert!(Interval::parse(0.4, 0.1).is_err());
        assert!(Interval::parse(f64::NAN, 0.1).is_err());
    }

    #[test]
    fn a_case_with_no_graded_attempt_has_no_k_outcome() {
        assert_eq!(case_k_outcome(std::iter::empty()), None);
    }

    #[test]
    fn one_pass_among_several_attempts_is_a_pass_at_k_but_not_a_pass_pow_k() {
        assert_eq!(case_k_outcome([false, true, false]), Some((true, false)));
    }

    #[test]
    fn every_attempt_passing_is_both_a_pass_at_k_and_a_pass_pow_k() {
        assert_eq!(case_k_outcome([true, true]), Some((true, true)));
    }

    #[test]
    fn every_attempt_failing_is_neither() {
        assert_eq!(case_k_outcome([false, false]), Some((false, false)));
    }

    #[test]
    fn k_case_counts_pools_outcomes_across_cases() {
        let counts = k_case_counts([(true, false), (true, true), (false, false)]);

        assert_eq!(counts.trials, 3);
        assert_eq!(counts.any_pass, 2);
        assert_eq!(counts.all_pass, 1);
    }

    #[test]
    fn best_of_n_reads_higher_than_the_matched_aggregate_over_the_same_cases() {
        let cases = [(true, false), (true, false), (true, true)];
        let counts = k_case_counts(cases);

        let pass_at_k = ScoredRate::from_counts(counts.any_pass, counts.trials);
        let pass_pow_k = ScoredRate::from_counts(counts.all_pass, counts.trials);

        assert_eq!(pass_at_k.rate, Some(1.0));
        assert_eq!(pass_pow_k.rate, Some(1.0 / 3.0));
        assert!(pass_pow_k.rate.unwrap() < pass_at_k.rate.unwrap());
    }

    #[test]
    fn a_scored_rate_over_nothing_reports_no_rate_rather_than_dividing_by_zero() {
        let rate = ScoredRate::from_counts(0, 0);

        assert_eq!(rate.rate, None);
        assert_eq!(rate.interval, None);
        assert_eq!(rate.trials, 0);
    }

    #[test]
    fn a_scored_rate_deserializes_through_proportion_parse_rather_than_trusting_its_own_rate() {
        let decoded: ScoredRate =
            serde_json::from_str(r#"{"successes":2,"trials":4,"rate":0.9,"interval":null}"#).expect("valid counts");

        assert_eq!(decoded, ScoredRate::from_counts(2, 4));
        assert_eq!(decoded.rate, Some(0.5));
    }

    #[test]
    fn a_scored_rate_with_more_successes_than_trials_is_refused_on_decode() {
        let result: Result<ScoredRate, _> =
            serde_json::from_str(r#"{"successes":5,"trials":3,"rate":null,"interval":null}"#);

        assert!(result.is_err());
    }

    #[test]
    fn a_scored_rate_over_nothing_decodes_back_to_the_same_default() {
        let decoded: ScoredRate =
            serde_json::from_str(r#"{"successes":0,"trials":0,"rate":null,"interval":null}"#).expect("empty counts");

        assert_eq!(decoded, ScoredRate::default());
    }
}
