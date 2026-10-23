use serde::Deserialize;

use super::benchmark::{bucket_cost, duration_stats, BucketCost, DurationStats, TokenStats};
use super::budget::RunCost;

#[derive(Debug, Clone, Deserialize, Default)]
pub(crate) struct TimingFileInput {
    #[serde(default)]
    pub(crate) duration_ms: Option<u64>,
    #[serde(default)]
    pub(crate) total_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) input_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) output_tokens: Option<u64>,
    #[serde(default)]
    pub(crate) cost: Option<RunCost>,
    /// Written by every release before a run's cost said which harness could not price
    /// it. Read as a priced run, because only a harness that prices its runs ever wrote
    /// a number here.
    #[serde(default)]
    pub(crate) cost_usd: Option<f64>,
}

impl TimingFileInput {
    pub(crate) fn cost(&self) -> Option<RunCost> {
        self.cost
            .clone()
            .or_else(|| self.cost_usd.map(|usd| RunCost::Priced { usd }))
    }
}

/// Every reading one arm's runs reported, kept per run so the published totals and the
/// per-run shifts between two arms are read from the same values.
#[derive(Debug, Clone, Default)]
pub struct ArmTiming {
    runs: usize,
    missing_timing: usize,
    duration_ms: Vec<u64>,
    input_tokens: Vec<u64>,
    output_tokens: Vec<u64>,
    total_tokens: Vec<u64>,
    costs: Vec<RunCost>,
}

impl ArmTiming {
    pub(crate) fn record_run(&mut self, timing: Option<&TimingFileInput>) {
        self.runs += 1;
        let Some(timing) = timing else {
            self.missing_timing += 1;
            return;
        };
        self.duration_ms.extend(timing.duration_ms);
        self.input_tokens.extend(timing.input_tokens);
        self.output_tokens.extend(timing.output_tokens);
        self.total_tokens.extend(timing.total_tokens);
        self.costs.extend(timing.cost());
    }

    /// Runs that belong to the arm but whose timing is never read, such as a failure
    /// scored as zero: they still count against a whole-arm cost.
    pub(crate) fn count_unread_runs(&mut self, count: usize) {
        self.runs += count;
    }

    pub fn runs(&self) -> usize {
        self.runs
    }

    pub fn missing_timing(&self) -> usize {
        self.missing_timing
    }

    pub fn duration_stats(&self) -> DurationStats {
        duration_stats(&self.duration_ms)
    }

    pub fn token_stats(&self) -> TokenStats {
        TokenStats {
            total: sum(&self.total_tokens),
            input: sum(&self.input_tokens),
            output: sum(&self.output_tokens),
            cost: self.cost(),
        }
    }

    pub fn cost(&self) -> Option<BucketCost> {
        bucket_cost(&self.costs, self.runs)
    }

    /// Only a total that covers every run is the arm's cost; a difference against any
    /// other subtracts two different questions from each other.
    pub fn whole_cost_usd(&self) -> Option<f64> {
        self.cost()?.whole_bucket_usd()
    }

    pub fn per_run_duration_ms(&self) -> Vec<f64> {
        as_readings(&self.duration_ms)
    }

    pub fn per_run_input_tokens(&self) -> Vec<f64> {
        as_readings(&self.input_tokens)
    }

    pub fn per_run_total_tokens(&self) -> Vec<f64> {
        as_readings(&self.total_tokens)
    }

    /// Nothing unless every run was priced: the priced runs alone would read the rest as
    /// runs that cost nothing.
    pub fn per_run_cost_usd(&self) -> Option<Vec<f64>> {
        self.whole_cost_usd()?;
        Some(self.costs.iter().filter_map(RunCost::usd).collect())
    }
}

fn sum(values: &[u64]) -> Option<u64> {
    match values.is_empty() {
        true => None,
        false => Some(values.iter().sum()),
    }
}

fn as_readings(values: &[u64]) -> Vec<f64> {
    values.iter().map(|value| *value as f64).collect()
}
