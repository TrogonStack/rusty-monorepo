//! Whether a change made the with_skill arm cost more, run longer or spend more tokens
//! than the previous iteration's, by more than the operator agreed to pay for it.
//!
//! The keep-or-revert recommendation reads pass rates only, so a change that holds its
//! pass rate while doubling what every run costs reads as harmless there. A total cannot
//! settle it either: two iterations that drew a different number of runs, or that drew
//! one runaway run, differ in total for reasons that are not the change. These verdicts
//! compare the runs themselves, through the same per-run shift `benchmark.json` publishes.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::dispersion;
use super::evals::EvalSplit;
use super::shift::Shift;

/// How much more per run, as a proportion of the previous iteration's median, a change is
/// allowed to cost before its increase counts as a regression.
///
/// Zero, the default, calls any increase the runs can tell apart from noise a regression.
/// A negative tolerance would demand a saving and call an unchanged skill a regression,
/// so it is refused.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
#[serde(transparent)]
pub struct IncreaseTolerance(f64);

impl IncreaseTolerance {
    pub fn parse(value: f64) -> Result<Self, String> {
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "an increase tolerance of {value} is not a proportion of zero or more"
            ));
        }
        Ok(Self(value))
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for IncreaseTolerance {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = f64::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for IncreaseTolerance {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "IncreaseTolerance".into()
    }

    /// Kept as strict as `Deserialize`, so `eval verify --mode strict` never calls a
    /// document conformant that its own builder would have refused to produce.
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "The allowed per-run increase over the previous iteration's median, as a proportion of that median. Zero or more.",
            "type": "number",
            "minimum": 0.0
        })
    }
}

impl fmt::Display for IncreaseTolerance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for IncreaseTolerance {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let parsed: f64 = value
            .trim()
            .parse()
            .map_err(|_| format!("'{value}' is not an increase tolerance"))?;
        Self::parse(parsed)
    }
}

/// The tolerance each metric was judged against, published so a verdict can be read
/// without knowing which flags produced it.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct EfficiencyTolerances {
    pub cost: IncreaseTolerance,
    pub duration: IncreaseTolerance,
    pub tokens: IncreaseTolerance,
}

/// One metric's per-run shift from the previous iteration's with_skill runs to this
/// one's, and the previous median the tolerance was scaled by.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShiftComparison {
    /// The current run minus the previous run, in the metric's own unit, with its 95%
    /// interval.
    pub shift: Shift,
    pub previous_median: f64,
    /// Runs on each side that reported this metric.
    pub current_runs: usize,
    pub previous_runs: usize,
}

/// Why a metric with runs behind it on both sides still has no verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnmeasuredReason {
    /// One side reported this metric on fewer runs than the per-run shift needs.
    TooFewReadings,
    /// At least one run on one side carried no price, so its arm has no per-run cost to
    /// compare without reading the unpriced runs as free.
    NotEveryRunPriced,
}

/// Each verdict below reads the same way: the regressing status means the whole interval
/// sits above the tolerance, `within_tolerance` means it sits above zero but not wholly
/// above the tolerance, the improving status means it sits entirely below zero, and
/// `indistinguishable` means it reaches zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CostVerdict {
    Cheaper(ShiftComparison),
    Costlier(ShiftComparison),
    WithinTolerance(ShiftComparison),
    Indistinguishable(ShiftComparison),
    Unmeasured { reason: UnmeasuredReason },
    NoRuns,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DurationVerdict {
    Faster(ShiftComparison),
    Slower(ShiftComparison),
    WithinTolerance(ShiftComparison),
    Indistinguishable(ShiftComparison),
    Unmeasured { reason: UnmeasuredReason },
    NoRuns,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TokenVerdict {
    FewerTokens(ShiftComparison),
    MoreTokens(ShiftComparison),
    WithinTolerance(ShiftComparison),
    Indistinguishable(ShiftComparison),
    Unmeasured { reason: UnmeasuredReason },
    NoRuns,
}

/// One split's efficiency verdicts. `tokens` reads total tokens per run.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SplitEfficiency {
    pub cost: CostVerdict,
    pub duration: DurationVerdict,
    pub tokens: TokenVerdict,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EfficiencySection {
    pub tolerances: EfficiencyTolerances,
    /// Empty when the suites differ, for the same reason the pass-rate verdicts are.
    pub by_split: BTreeMap<EvalSplit, SplitEfficiency>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EfficiencyMetric {
    Cost,
    Duration,
    Tokens,
}

impl EfficiencyMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cost => "cost",
            Self::Duration => "duration",
            Self::Tokens => "tokens",
        }
    }
}

impl EfficiencySection {
    /// The splits on which `metric` regressed. Each split is its own comparison, so a gate
    /// that fires on any of them is the stricter reading, and the tolerances are what an
    /// operator loosens when that is too strict.
    pub fn regressed_splits(&self, metric: EfficiencyMetric) -> Vec<EvalSplit> {
        self.by_split
            .iter()
            .filter(|(_, verdicts)| verdicts.regressed(metric))
            .map(|(split, _)| *split)
            .collect()
    }
}

impl SplitEfficiency {
    pub fn regressed(&self, metric: EfficiencyMetric) -> bool {
        match metric {
            EfficiencyMetric::Cost => matches!(self.cost, CostVerdict::Costlier(_)),
            EfficiencyMetric::Duration => matches!(self.duration, DurationVerdict::Slower(_)),
            EfficiencyMetric::Tokens => matches!(self.tokens, TokenVerdict::MoreTokens(_)),
        }
    }
}

/// The per-run readings one iteration's completed with_skill runs on one split reported.
///
/// A failed run is left out: its duration is the ceiling it hit and its spend stops
/// wherever it stopped, so neither says what the skill costs to run.
#[derive(Debug, Clone, Default)]
pub struct ArmReadings {
    pub completed_runs: usize,
    pub duration_ms: Vec<f64>,
    pub total_tokens: Vec<f64>,
    /// `None` when any completed run carried no price.
    pub cost_usd: Option<Vec<f64>>,
}

pub fn judge_split(current: &ArmReadings, previous: &ArmReadings, tolerances: EfficiencyTolerances) -> SplitEfficiency {
    let no_runs = current.completed_runs == 0 || previous.completed_runs == 0;
    let cost = match (&current.cost_usd, &previous.cost_usd) {
        _ if no_runs => Judgement::NoRuns,
        (Some(current), Some(previous)) => judge(current, previous, tolerances.cost),
        _ => Judgement::Unmeasured(UnmeasuredReason::NotEveryRunPriced),
    };
    let duration = match no_runs {
        true => Judgement::NoRuns,
        false => judge(&current.duration_ms, &previous.duration_ms, tolerances.duration),
    };
    let tokens = match no_runs {
        true => Judgement::NoRuns,
        false => judge(&current.total_tokens, &previous.total_tokens, tolerances.tokens),
    };
    SplitEfficiency {
        cost: cost.into(),
        duration: duration.into(),
        tokens: tokens.into(),
    }
}

enum Judgement {
    Lower(ShiftComparison),
    Higher(ShiftComparison),
    HigherWithinTolerance(ShiftComparison),
    Straddling(ShiftComparison),
    Unmeasured(UnmeasuredReason),
    NoRuns,
}

fn judge(current: &[f64], previous: &[f64], tolerance: IncreaseTolerance) -> Judgement {
    let Some(shift) = Shift::between(current, previous) else {
        return Judgement::Unmeasured(UnmeasuredReason::TooFewReadings);
    };
    let Some(previous_median) = dispersion::median(&mut previous.to_vec()) else {
        return Judgement::Unmeasured(UnmeasuredReason::TooFewReadings);
    };
    let comparison = ShiftComparison {
        shift,
        previous_median,
        current_runs: current.len(),
        previous_runs: previous.len(),
    };
    let allowed_increase = tolerance.value() * previous_median;
    let interval = shift.interval();
    if interval.low() > allowed_increase {
        Judgement::Higher(comparison)
    } else if interval.low() > 0.0 {
        Judgement::HigherWithinTolerance(comparison)
    } else if interval.high() < 0.0 {
        Judgement::Lower(comparison)
    } else {
        Judgement::Straddling(comparison)
    }
}

impl From<Judgement> for CostVerdict {
    fn from(judgement: Judgement) -> Self {
        match judgement {
            Judgement::Lower(comparison) => Self::Cheaper(comparison),
            Judgement::Higher(comparison) => Self::Costlier(comparison),
            Judgement::HigherWithinTolerance(comparison) => Self::WithinTolerance(comparison),
            Judgement::Straddling(comparison) => Self::Indistinguishable(comparison),
            Judgement::Unmeasured(reason) => Self::Unmeasured { reason },
            Judgement::NoRuns => Self::NoRuns,
        }
    }
}

impl From<Judgement> for DurationVerdict {
    fn from(judgement: Judgement) -> Self {
        match judgement {
            Judgement::Lower(comparison) => Self::Faster(comparison),
            Judgement::Higher(comparison) => Self::Slower(comparison),
            Judgement::HigherWithinTolerance(comparison) => Self::WithinTolerance(comparison),
            Judgement::Straddling(comparison) => Self::Indistinguishable(comparison),
            Judgement::Unmeasured(reason) => Self::Unmeasured { reason },
            Judgement::NoRuns => Self::NoRuns,
        }
    }
}

impl From<Judgement> for TokenVerdict {
    fn from(judgement: Judgement) -> Self {
        match judgement {
            Judgement::Lower(comparison) => Self::FewerTokens(comparison),
            Judgement::Higher(comparison) => Self::MoreTokens(comparison),
            Judgement::HigherWithinTolerance(comparison) => Self::WithinTolerance(comparison),
            Judgement::Straddling(comparison) => Self::Indistinguishable(comparison),
            Judgement::Unmeasured(reason) => Self::Unmeasured { reason },
            Judgement::NoRuns => Self::NoRuns,
        }
    }
}

/// A one-line rendering of one split's verdicts for the human summary.
pub fn describe_split_efficiency(verdicts: &SplitEfficiency) -> String {
    let cost = match &verdicts.cost {
        CostVerdict::Cheaper(comparison) => describe("cheaper", comparison, "USD", 4),
        CostVerdict::Costlier(comparison) => describe("costlier", comparison, "USD", 4),
        CostVerdict::WithinTolerance(comparison) => describe("within tolerance", comparison, "USD", 4),
        CostVerdict::Indistinguishable(comparison) => describe("indistinguishable", comparison, "USD", 4),
        CostVerdict::Unmeasured { reason } => describe_unmeasured(*reason),
        CostVerdict::NoRuns => "no runs".to_string(),
    };
    let duration = match &verdicts.duration {
        DurationVerdict::Faster(comparison) => describe("faster", comparison, "ms", 0),
        DurationVerdict::Slower(comparison) => describe("slower", comparison, "ms", 0),
        DurationVerdict::WithinTolerance(comparison) => describe("within tolerance", comparison, "ms", 0),
        DurationVerdict::Indistinguishable(comparison) => describe("indistinguishable", comparison, "ms", 0),
        DurationVerdict::Unmeasured { reason } => describe_unmeasured(*reason),
        DurationVerdict::NoRuns => "no runs".to_string(),
    };
    let tokens = match &verdicts.tokens {
        TokenVerdict::FewerTokens(comparison) => describe("fewer tokens", comparison, "tokens", 0),
        TokenVerdict::MoreTokens(comparison) => describe("more tokens", comparison, "tokens", 0),
        TokenVerdict::WithinTolerance(comparison) => describe("within tolerance", comparison, "tokens", 0),
        TokenVerdict::Indistinguishable(comparison) => describe("indistinguishable", comparison, "tokens", 0),
        TokenVerdict::Unmeasured { reason } => describe_unmeasured(*reason),
        TokenVerdict::NoRuns => "no runs".to_string(),
    };
    format!("cost {cost}; duration {duration}; tokens {tokens}")
}

fn describe(label: &str, comparison: &ShiftComparison, unit: &str, precision: usize) -> String {
    let interval = comparison.shift.interval();
    format!(
        "{label} ({:+.precision$} {unit}/run, 95% CI [{:+.precision$}, {:+.precision$}], previous median {:.precision$})",
        comparison.shift.estimate(),
        interval.low(),
        interval.high(),
        comparison.previous_median
    )
}

fn describe_unmeasured(reason: UnmeasuredReason) -> String {
    match reason {
        UnmeasuredReason::TooFewReadings => "unmeasured (too few runs reported it)".to_string(),
        UnmeasuredReason::NotEveryRunPriced => "unmeasured (not every run was priced)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm(duration_ms: &[f64], total_tokens: &[f64], cost_usd: Option<&[f64]>) -> ArmReadings {
        ArmReadings {
            completed_runs: duration_ms.len(),
            duration_ms: duration_ms.to_vec(),
            total_tokens: total_tokens.to_vec(),
            cost_usd: cost_usd.map(<[f64]>::to_vec),
        }
    }

    fn tolerances(cost: f64, duration: f64, tokens: f64) -> EfficiencyTolerances {
        EfficiencyTolerances {
            cost: IncreaseTolerance::parse(cost).unwrap(),
            duration: IncreaseTolerance::parse(duration).unwrap(),
            tokens: IncreaseTolerance::parse(tokens).unwrap(),
        }
    }

    #[test]
    fn a_negative_or_unbounded_tolerance_is_refused() {
        assert!(IncreaseTolerance::parse(-0.1).is_err());
        assert!(IncreaseTolerance::parse(f64::INFINITY).is_err());
        assert!(IncreaseTolerance::parse(f64::NAN).is_err());
        assert!("abc".parse::<IncreaseTolerance>().is_err());
        assert_eq!("0.25".parse::<IncreaseTolerance>().unwrap().value(), 0.25);
        assert_eq!(IncreaseTolerance::default().value(), 0.0);
    }

    #[test]
    fn a_deserialized_tolerance_is_held_to_the_same_rule_as_a_flag() {
        assert!(serde_json::from_str::<IncreaseTolerance>("-1").is_err());
        assert_eq!(serde_json::from_str::<IncreaseTolerance>("0.5").unwrap().value(), 0.5);
    }

    #[test]
    fn runs_that_cost_clearly_more_are_costlier_and_gate() {
        let previous = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.10, 0.11, 0.10, 0.12, 0.10]));
        let current = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.30, 0.31, 0.29, 0.32, 0.30]));

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        assert!(matches!(verdicts.cost, CostVerdict::Costlier(_)), "{verdicts:?}");
        assert!(verdicts.regressed(EfficiencyMetric::Cost));
        assert!(!verdicts.regressed(EfficiencyMetric::Duration));
        assert!(matches!(verdicts.duration, DurationVerdict::Indistinguishable(_)));
    }

    #[test]
    fn runs_that_cost_clearly_less_are_cheaper_and_faster() {
        let previous = arm(&[2000.0, 2100.0, 2200.0, 2300.0], &[900.0; 4], Some(&[0.30; 4]));
        let current = arm(&[1000.0, 1100.0, 1200.0, 1300.0], &[400.0; 4], Some(&[0.10; 4]));

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        assert!(matches!(verdicts.cost, CostVerdict::Cheaper(_)));
        assert!(matches!(verdicts.duration, DurationVerdict::Faster(_)));
        assert!(matches!(verdicts.tokens, TokenVerdict::FewerTokens(_)));
    }

    #[test]
    fn an_increase_inside_the_tolerance_is_not_a_regression() {
        let previous = arm(&[1000.0, 1010.0, 990.0, 1005.0, 995.0], &[500.0; 5], None);
        let current = arm(&[1100.0, 1110.0, 1090.0, 1105.0, 1095.0], &[500.0; 5], None);

        let strict = judge_split(&current, &previous, EfficiencyTolerances::default());
        let lenient = judge_split(&current, &previous, tolerances(0.0, 0.2, 0.0));

        assert!(matches!(strict.duration, DurationVerdict::Slower(_)));
        assert!(strict.regressed(EfficiencyMetric::Duration));
        assert!(
            matches!(lenient.duration, DurationVerdict::WithinTolerance(_)),
            "{lenient:?}"
        );
        assert!(!lenient.regressed(EfficiencyMetric::Duration));
    }

    #[test]
    fn a_real_increase_inside_the_tolerance_is_named_as_one_for_every_metric() {
        let previous = arm(
            &[1000.0, 1010.0, 990.0, 1005.0],
            &[500.0, 505.0, 495.0, 500.0],
            Some(&[0.100; 4]),
        );
        let current = arm(
            &[1100.0, 1110.0, 1090.0, 1105.0],
            &[550.0, 555.0, 545.0, 550.0],
            Some(&[0.105; 4]),
        );

        let verdicts = judge_split(&current, &previous, tolerances(0.1, 0.2, 0.15));

        for (status, regressed) in [
            (
                serde_json::to_value(verdicts.cost).unwrap(),
                verdicts.regressed(EfficiencyMetric::Cost),
            ),
            (
                serde_json::to_value(verdicts.duration).unwrap(),
                verdicts.regressed(EfficiencyMetric::Duration),
            ),
            (
                serde_json::to_value(verdicts.tokens).unwrap(),
                verdicts.regressed(EfficiencyMetric::Tokens),
            ),
        ] {
            assert_eq!(status["status"], "within_tolerance", "{verdicts:?}");
            assert!(status["shift"]["interval"]["low"].as_f64().unwrap() > 0.0);
            assert!(!regressed);
        }
        assert!(describe_split_efficiency(&verdicts).contains("duration within tolerance (+100 ms/run"));
    }

    #[test]
    fn a_zero_tolerance_never_reads_an_increase_as_within_tolerance() {
        let previous = arm(&[1000.0, 1010.0, 990.0, 1005.0], &[500.0; 4], None);
        let current = arm(&[1001.0, 1011.0, 991.0, 1006.0], &[500.0; 4], None);

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        assert!(
            !matches!(verdicts.duration, DurationVerdict::WithinTolerance(_)),
            "{verdicts:?}"
        );
        assert!(
            !matches!(verdicts.tokens, TokenVerdict::WithinTolerance(_)),
            "{verdicts:?}"
        );
    }

    #[test]
    fn overlapping_runs_are_indistinguishable() {
        let previous = arm(&[1000.0, 3000.0, 1500.0, 2500.0, 2000.0], &[500.0; 5], None);
        let current = arm(&[1100.0, 2900.0, 1600.0, 2400.0, 2100.0], &[500.0; 5], None);

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        assert!(matches!(verdicts.duration, DurationVerdict::Indistinguishable(_)));
        assert!(matches!(
            judge_split(&current, &previous, tolerances(0.0, 0.5, 0.0)).duration,
            DurationVerdict::Indistinguishable(_)
        ));
    }

    #[test]
    fn an_arm_with_an_unpriced_run_is_unmeasured_rather_than_compared() {
        let previous = arm(&[1000.0; 5], &[500.0; 5], None);
        let current = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.30; 5]));

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        assert_eq!(
            verdicts.cost,
            CostVerdict::Unmeasured {
                reason: UnmeasuredReason::NotEveryRunPriced
            }
        );
    }

    #[test]
    fn too_few_runs_leave_every_metric_unmeasured() {
        let previous = arm(&[1000.0; 3], &[500.0; 3], Some(&[0.10; 3]));
        let current = arm(&[9000.0; 3], &[5000.0; 3], Some(&[0.90; 3]));

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        let too_few = UnmeasuredReason::TooFewReadings;
        assert_eq!(verdicts.cost, CostVerdict::Unmeasured { reason: too_few });
        assert_eq!(verdicts.duration, DurationVerdict::Unmeasured { reason: too_few });
        assert_eq!(verdicts.tokens, TokenVerdict::Unmeasured { reason: too_few });
    }

    #[test]
    fn a_side_with_no_completed_runs_is_no_runs_not_a_measurement() {
        let previous = ArmReadings::default();
        let current = arm(&[1000.0; 5], &[500.0; 5], Some(&[0.10; 5]));

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());

        assert_eq!(verdicts.cost, CostVerdict::NoRuns);
        assert_eq!(verdicts.duration, DurationVerdict::NoRuns);
        assert_eq!(verdicts.tokens, TokenVerdict::NoRuns);
    }

    #[test]
    fn a_verdict_serializes_under_its_metric_name() {
        let previous = arm(&[1000.0; 4], &[500.0; 4], Some(&[0.10; 4]));
        let current = arm(&[2000.0; 4], &[900.0; 4], Some(&[0.20; 4]));

        let verdicts = judge_split(&current, &previous, EfficiencyTolerances::default());
        let published = serde_json::to_value(verdicts).unwrap();

        assert_eq!(published["cost"]["status"], "costlier");
        assert_eq!(published["duration"]["status"], "slower");
        assert_eq!(published["tokens"]["status"], "more_tokens");
        assert_eq!(published["duration"]["shift"]["estimate"], 1000.0);
        assert_eq!(published["duration"]["previous_median"], 1000.0);
    }

    #[test]
    fn a_section_names_only_the_splits_that_regressed() {
        let previous = arm(&[1000.0; 4], &[500.0; 4], None);
        let slower = arm(&[2000.0; 4], &[500.0; 4], None);
        let same = arm(&[1000.0; 4], &[500.0; 4], None);
        let tolerances = EfficiencyTolerances::default();
        let section = EfficiencySection {
            tolerances,
            by_split: BTreeMap::from([
                (EvalSplit::Train, judge_split(&same, &previous, tolerances)),
                (EvalSplit::Test, judge_split(&slower, &previous, tolerances)),
            ]),
        };

        assert_eq!(
            section.regressed_splits(EfficiencyMetric::Duration),
            vec![EvalSplit::Test]
        );
        assert!(section.regressed_splits(EfficiencyMetric::Tokens).is_empty());
    }
}
