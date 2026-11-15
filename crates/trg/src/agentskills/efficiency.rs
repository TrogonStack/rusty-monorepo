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

use super::arm_timing::ArmTiming;
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

    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            discarded_error_source,
            reason = "The established command-line parser contract returns a field-specific String validation message instead of a source-bearing error"
        )
    )]
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
/// sits above the tolerance, `within_tolerance` means it sits above zero and wholly inside
/// the tolerance, `increased` means it sits above zero but reaches past the tolerance, so
/// the runs cannot say which side of it the increase falls, the improving status means it
/// sits entirely below zero, and `indistinguishable` means it reaches zero.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CostVerdict {
    Cheaper(ShiftComparison),
    Costlier(ShiftComparison),
    WithinTolerance(ShiftComparison),
    Increased(ShiftComparison),
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
    Increased(ShiftComparison),
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
    Increased(ShiftComparison),
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

/// Compares two iterations' completed with_skill runs on one split. A failed run belongs
/// in neither arm: its duration is the ceiling it hit and its spend stops wherever it
/// stopped, so neither says what the skill costs to run.
pub fn judge_split(current: &ArmTiming, previous: &ArmTiming, tolerances: EfficiencyTolerances) -> SplitEfficiency {
    let no_runs = current.runs() == 0 || previous.runs() == 0;
    let cost = match (current.per_run_cost_usd(), previous.per_run_cost_usd()) {
        _ if no_runs => Judgement::NoRuns,
        (Some(current), Some(previous)) => judge(&current, &previous, tolerances.cost),
        _ => Judgement::Unmeasured(UnmeasuredReason::NotEveryRunPriced),
    };
    let duration = match no_runs {
        true => Judgement::NoRuns,
        false => judge(
            &current.per_run_duration_ms(),
            &previous.per_run_duration_ms(),
            tolerances.duration,
        ),
    };
    let tokens = match no_runs {
        true => Judgement::NoRuns,
        false => judge(
            &current.per_run_total_tokens(),
            &previous.per_run_total_tokens(),
            tolerances.tokens,
        ),
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
    HigherPossiblyBeyondTolerance(ShiftComparison),
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
    } else if interval.low() > 0.0 && interval.high() <= allowed_increase {
        Judgement::HigherWithinTolerance(comparison)
    } else if interval.low() > 0.0 {
        Judgement::HigherPossiblyBeyondTolerance(comparison)
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
            Judgement::HigherPossiblyBeyondTolerance(comparison) => Self::Increased(comparison),
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
            Judgement::HigherPossiblyBeyondTolerance(comparison) => Self::Increased(comparison),
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
            Judgement::HigherPossiblyBeyondTolerance(comparison) => Self::Increased(comparison),
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
        CostVerdict::Increased(comparison) => describe("increased", comparison, "USD", 4),
        CostVerdict::Indistinguishable(comparison) => describe("indistinguishable", comparison, "USD", 4),
        CostVerdict::Unmeasured { reason } => describe_unmeasured(*reason),
        CostVerdict::NoRuns => "no runs".to_string(),
    };
    let duration = match &verdicts.duration {
        DurationVerdict::Faster(comparison) => describe("faster", comparison, "ms", 0),
        DurationVerdict::Slower(comparison) => describe("slower", comparison, "ms", 0),
        DurationVerdict::WithinTolerance(comparison) => describe("within tolerance", comparison, "ms", 0),
        DurationVerdict::Increased(comparison) => describe("increased", comparison, "ms", 0),
        DurationVerdict::Indistinguishable(comparison) => describe("indistinguishable", comparison, "ms", 0),
        DurationVerdict::Unmeasured { reason } => describe_unmeasured(*reason),
        DurationVerdict::NoRuns => "no runs".to_string(),
    };
    let tokens = match &verdicts.tokens {
        TokenVerdict::FewerTokens(comparison) => describe("fewer tokens", comparison, "tokens", 0),
        TokenVerdict::MoreTokens(comparison) => describe("more tokens", comparison, "tokens", 0),
        TokenVerdict::WithinTolerance(comparison) => describe("within tolerance", comparison, "tokens", 0),
        TokenVerdict::Increased(comparison) => describe("increased", comparison, "tokens", 0),
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
#[path = "efficiency_tests.rs"]
mod tests;
