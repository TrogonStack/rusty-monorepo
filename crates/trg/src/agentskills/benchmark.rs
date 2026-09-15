use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::budget::RunCost;
use super::dispersion::{self, AssertionKey, FlakinessLedger, FlakyAssertionRecord, MetricSample};
use super::eval_suite_drift::{
    detect_eval_suite_drift_snapshots, load_report_drift_snapshot, maybe_emit_eval_suite_drift_warning,
    parse_report_iteration, EvalSuiteDriftWarning,
};
use super::evals::{EvalError, Result};
use super::iteration_summary::detect_previous_report_dir;
use super::proportion::{self, Interval, Proportion};
use super::report::ScenarioKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailedRunsMode {
    #[default]
    Bucket,
    Exclude,
    Zero,
}

#[derive(Debug, Clone, Default)]
pub struct BenchmarkOptions {
    pub failed_runs: FailedRunsMode,
    pub allow_eval_suite_drift: bool,
    pub previous_report_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(title = "trg skills eval benchmark")]
pub struct BenchmarkDocument {
    pub report_id: String,
    pub generated_at: String,
    pub failed_runs_mode: FailedRunsMode,
    pub scenarios: BTreeMap<String, ScenarioBenchmark>,
    pub deltas: ScenarioDeltas,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration_comparison: Option<IterationComparison>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_eval_scenario: Vec<EvalScenarioAttemptRow>,
    pub iteration_summary: IterationSummary,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<EvalSuiteDriftWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EvalScenarioAttemptRow {
    pub eval_case_id: String,
    pub scenario_id: String,
    pub attempt_count: u32,
    pub pass_rate: AttemptPassRateStats,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flaky_assertions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AttemptPassRateStats {
    pub mean: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variance: Option<f64>,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, JsonSchema)]
pub struct IterationSummary {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_pass: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub always_fail: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub helped_by_skill: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flaky_assertions: Vec<FlakyAssertionRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timing_outliers: Vec<MetricOutlier>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub token_outliers: Vec<MetricOutlier>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MetricOutlier {
    pub run_id: String,
    pub eval_case_id: String,
    pub scenario_id: String,
    pub attempt: u32,
    pub value: u64,
    pub median: f64,
    pub mad: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScenarioBenchmark {
    pub completed: CompletedBucket,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<RunBucketSummary>,
    pub skipped: SkippedBucket,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CompletedBucket {
    pub run_count: usize,
    pub assertions: PassFailSummary,
    pub runs: PassFailSummary,
    pub duration_ms: DurationStats,
    pub tokens: TokenStats,
    pub missing_grading: usize,
    pub missing_timing: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RunBucketSummary {
    pub run_count: usize,
    pub duration_ms: DurationStats,
    pub tokens: TokenStats,
    pub missing_timing: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SkippedBucket {
    pub run_count: usize,
}

#[derive(Debug, Clone, Serialize, Default, JsonSchema)]
pub struct PassFailSummary {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub pass_rate: f64,
}

#[derive(Debug, Clone, Serialize, Default, JsonSchema)]
pub struct DurationStats {
    pub mean: f64,
    pub p50: u64,
    pub p95: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stddev: Option<f64>,
    /// Present once the draws clear `SampleFloor::dispersion()`; a group any smaller has
    /// nothing to measure its own spread against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mad: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct TokenStats {
    pub total: Option<u64>,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cost: Option<BucketCost>,
}

/// The shape a bucket's tokens are written in, which is the shape every earlier release
/// still reads. `cost_usd` was that release's whole vocabulary for cost, and it was read
/// as the bucket's cost, so it is published for exactly the buckets whose total is that:
/// a partial total under that name is the reading this field was changed to stop.
#[derive(Serialize, JsonSchema)]
#[schemars(rename = "TokenStats")]
struct SerializedTokenStats<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<&'a BucketCost>,
    /// The bucket's cost alone, for readers written before `cost` could say what a total
    /// covers. Published only for a bucket every run of which was priced.
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
}

impl<'a> From<&'a TokenStats> for SerializedTokenStats<'a> {
    fn from(stats: &'a TokenStats) -> Self {
        Self {
            total: stats.total,
            input: stats.input,
            output: stats.output,
            cost: stats.cost.as_ref(),
            cost_usd: stats.cost.as_ref().and_then(BucketCost::whole_bucket_usd),
        }
    }
}

impl Serialize for TokenStats {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        SerializedTokenStats::from(self).serialize(serializer)
    }
}

impl JsonSchema for TokenStats {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TokenStats".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        SerializedTokenStats::json_schema(generator)
    }
}

/// What the runs behind one bucket cost, or why nobody can say.
///
/// A plain total said nothing about how much of its bucket it covered, so a bucket of ten
/// runs where one carried a price published that one run's price as the bucket's cost.
/// Saying which of the two a total is what makes it readable at all.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BucketCost {
    /// Every run in this bucket carried a price, so this total is the bucket's cost.
    Whole {
        #[schemars(range(min = 0.0))]
        usd: f64,
    },
    /// Only some of the bucket's runs carried a price. `runs` is how many, so nobody
    /// reads the total as the whole bucket's.
    Partial {
        #[schemars(range(min = 0.0))]
        usd: f64,
        runs: usize,
    },
    /// At least one run here came from a harness that prices nothing, so this bucket has
    /// no total at all rather than a total that quietly leaves those runs out.
    Unpriced { harness: String },
}

impl BucketCost {
    /// The bucket's own cost, which only a total covering every one of its runs is.
    pub fn whole_bucket_usd(&self) -> Option<f64> {
        match self {
            Self::Whole { usd } => Some(*usd),
            Self::Partial { .. } | Self::Unpriced { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default, JsonSchema)]
pub struct ScenarioDeltas {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_skill_vs_without_skill: Option<ScenarioDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_skill_vs_old_skill: Option<ScenarioDelta>,
}

/// A subtraction between two arms, with what stood behind each side of it.
///
/// Three draws is the default depth of a cell, and a scenario pools several cells
/// together, so the runs behind an arm are rarely the twelve or twenty a reader might
/// assume a benchmark implies. A delta without `left`/`right` published next to it asks
/// the reader to trust a rounding of two small, unlabeled counts as if it were a finding.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ScenarioDelta {
    pub assertion_pass_rate: f64,
    /// The 95% interval the draws behind both arms leave around `assertion_pass_rate`.
    /// An interval that contains zero means these arms have not been told apart, however
    /// large the subtraction above it looks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assertion_pass_rate_interval: Option<Interval>,
    pub run_pass_rate: f64,
    /// The 95% interval around `run_pass_rate`, read the same way.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_pass_rate_interval: Option<Interval>,
    pub duration_ms_mean: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_total: Option<i64>,
    /// Published only when both arms priced every run behind them, because a difference
    /// between two totals that cover different numbers of runs is not a cost difference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub left: ArmObservations,
    pub right: ArmObservations,
}

/// What one side of a `ScenarioDelta` drew.
///
/// `runs` is always available and always means something, so it is always published.
/// A spread is not: duration is the only metric here still carrying its raw per-run
/// values at the point a delta is built, so it is the only one that can honestly offer
/// one, and only once the arm clears the floor `Dispersion` needs to describe itself.
/// Pass rates and cost are already pooled sums by then, with no per-draw distribution
/// left to measure.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct ArmObservations {
    pub runs: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_median_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_mad_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct IterationComparison {
    pub current_iteration_id: String,
    pub previous_iteration_id: String,
    pub by_scenario: BTreeMap<String, ScenarioDelta>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReportForBenchmark {
    report: ReportMeta,
    runs: Vec<RunForBenchmark>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReportMeta {
    id: String,
    #[serde(default, deserialize_with = "deserialize_iteration_meta")]
    iteration: Option<IterationMeta>,
}

fn deserialize_iteration_meta<'de, D>(deserializer: D) -> std::result::Result<Option<IterationMeta>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_object() {
        IterationMeta::deserialize(value)
            .map(Some)
            .map_err(serde::de::Error::custom)
    } else {
        Ok(None)
    }
}

#[derive(Debug, Clone, Deserialize)]
struct IterationMeta {
    id: String,
    #[serde(default)]
    previous_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RunForBenchmark {
    id: String,
    eval_case_id: String,
    scenario_id: ScenarioKind,
    attempt: u32,
    status: String,
    paths: RunPathsForBenchmark,
    #[serde(default)]
    iteration_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RunPathsForBenchmark {
    workspace: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct GradingFileInput {
    #[serde(default)]
    assertion_results: Vec<AssertionResultInput>,
    #[serde(default)]
    summary: Option<GradingSummaryInput>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
struct AssertionResultInput {
    #[serde(default)]
    assertion: String,
    #[serde(default)]
    passed: bool,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    unsupported: Option<String>,
    #[serde(default)]
    excluded: Option<String>,
}

impl AssertionResultInput {
    fn is_scored(&self) -> bool {
        self.unsupported.is_none() && self.excluded.is_none()
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[allow(dead_code)]
struct GradingSummaryInput {
    #[serde(default)]
    passed: usize,
    #[serde(default)]
    failed: usize,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    pass_rate: f64,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct TimingFileInput {
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cost: Option<RunCost>,
    /// Written by every release before a run's cost said which harness could not price
    /// it. Read as a priced run, because only a harness that prices its runs ever wrote
    /// a number here.
    #[serde(default)]
    cost_usd: Option<f64>,
}

impl TimingFileInput {
    fn cost(&self) -> Option<RunCost> {
        self.cost
            .clone()
            .or_else(|| self.cost_usd.map(|usd| RunCost::Priced { usd }))
    }
}

#[derive(Debug, Clone, Default)]
struct RunSample {
    grading: Option<GradingFileInput>,
    timing: Option<TimingFileInput>,
    missing_grading: bool,
    missing_timing: bool,
}

#[derive(Debug, Default)]
struct ScenarioAccumulator {
    completed: Vec<RunSample>,
    failed: Vec<RunSample>,
    skipped: usize,
}

pub fn build_benchmark(report_dir: &Path, options: BenchmarkOptions) -> Result<BenchmarkDocument> {
    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path)?;
    let report: ReportForBenchmark = serde_json::from_str(&content)?;

    let mut by_scenario: HashMap<ScenarioKind, ScenarioAccumulator> = HashMap::new();
    for run in &report.runs {
        let entry = by_scenario.entry(run.scenario_id).or_default();
        let sample = load_run_sample(report_dir, &run.paths.workspace);

        match classify_run(run, options.failed_runs) {
            RunDisposition::Completed => entry.completed.push(sample),
            RunDisposition::Failed => entry.failed.push(sample),
            RunDisposition::Skipped => entry.skipped += 1,
            RunDisposition::Excluded => {}
        }
    }

    let scenarios: Vec<(ScenarioKind, ScenarioBenchmark)> = ScenarioKind::ALL
        .iter()
        .map(|scenario| {
            let accumulator = by_scenario.remove(scenario).unwrap_or_default();
            (*scenario, finalize_scenario(accumulator, options.failed_runs))
        })
        .collect();

    let scenario_map: BTreeMap<String, ScenarioBenchmark> = scenarios
        .iter()
        .map(|(kind, bench)| (kind.as_str().to_string(), bench.clone()))
        .collect();

    let deltas = compute_scenario_deltas(&scenarios);
    let failed_runs = options.failed_runs;
    let allow_eval_suite_drift = options.allow_eval_suite_drift;
    let previous_report_dir = options.previous_report_dir.clone();
    let iteration_comparison = build_iteration_comparison(report_dir, &report, failed_runs);
    let (by_eval_scenario, flaky_assertions) = build_by_eval_scenario(report_dir, &report.runs, failed_runs);
    let iteration_summary = build_iteration_summary(report_dir, &report.runs, flaky_assertions, failed_runs);
    let warnings = collect_eval_suite_drift_warnings(
        report_dir,
        BenchmarkOptions {
            failed_runs,
            allow_eval_suite_drift,
            previous_report_dir,
        },
    )?;

    Ok(BenchmarkDocument {
        report_id: report.report.id,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        failed_runs_mode: failed_runs,
        scenarios: scenario_map,
        deltas,
        iteration_comparison,
        by_eval_scenario,
        iteration_summary,
        warnings,
    })
}

fn collect_eval_suite_drift_warnings(
    report_dir: &Path,
    options: BenchmarkOptions,
) -> Result<Vec<EvalSuiteDriftWarning>> {
    let current = load_report_drift_snapshot(report_dir)?;
    let previous = match options.previous_report_dir {
        Some(dir) => Some(load_report_drift_snapshot(&dir)?),
        None => detect_previous_report_dir(report_dir, current.iteration)
            .map(|previous| previous.drift_snapshot())
            .transpose()?,
    };

    let Some(previous) = previous else {
        return Ok(Vec::new());
    };

    let drift = detect_eval_suite_drift_snapshots(&current, &previous);
    maybe_emit_eval_suite_drift_warning(drift.as_ref(), options.allow_eval_suite_drift);

    Ok(drift
        .as_ref()
        .map(|report| vec![EvalSuiteDriftWarning::from(report)])
        .unwrap_or_default())
}

pub fn write_benchmark(report_dir: &Path, document: &BenchmarkDocument) -> Result<PathBuf> {
    sync_iteration_summary_to_report(report_dir, &document.iteration_summary)?;

    let output_path = report_dir.join("benchmark.json");
    let json = serde_json::to_string_pretty(document)?;
    std::fs::write(&output_path, &json)?;

    if let Ok(iteration) = read_report_iteration(report_dir) {
        let iteration_benchmark = report_dir
            .join(super::layout::iteration_dir_name(iteration))
            .join("benchmark.json");
        if let Some(parent) = iteration_benchmark.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(iteration_benchmark, &json)?;
    }

    Ok(output_path)
}

fn read_report_iteration(report_dir: &Path) -> std::result::Result<u32, EvalError> {
    let content = std::fs::read_to_string(report_dir.join("report.json"))?;
    let value: serde_json::Value = serde_json::from_str(&content)?;
    parse_report_iteration(&value)
}

pub fn sync_iteration_summary_to_report(report_dir: &Path, summary: &IterationSummary) -> Result<()> {
    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path)?;
    let mut document: serde_json::Value = serde_json::from_str(&content)?;
    document["iteration_summary"] = serde_json::to_value(summary)?;
    std::fs::write(report_path, serde_json::to_string_pretty(&document)?)?;
    Ok(())
}

enum RunDisposition {
    Completed,
    Failed,
    Skipped,
    Excluded,
}

fn classify_run(run: &RunForBenchmark, mode: FailedRunsMode) -> RunDisposition {
    match run.status.as_str() {
        "skipped" => RunDisposition::Skipped,
        "failed" | "timeout" => match mode {
            FailedRunsMode::Exclude => RunDisposition::Excluded,
            FailedRunsMode::Zero | FailedRunsMode::Bucket => RunDisposition::Failed,
        },
        _ => RunDisposition::Completed,
    }
}

fn load_run_sample(report_dir: &Path, workspace_rel: &str) -> RunSample {
    let workspace_dir = report_dir.join(workspace_rel);
    let run_dir = workspace_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| report_dir.to_path_buf());

    let grading_path = [run_dir.join("grading.json"), workspace_dir.join("grading.json")]
        .into_iter()
        .find(|path| path.is_file());
    let timing_path = [run_dir.join("timing.json"), workspace_dir.join("timing.json")]
        .into_iter()
        .find(|path| path.is_file());

    let grading = grading_path.as_ref().and_then(|path| read_json_file(path).ok());
    let timing = timing_path.as_ref().and_then(|path| read_json_file(path).ok());

    RunSample {
        missing_grading: grading.is_none(),
        missing_timing: timing.is_none(),
        grading,
        timing,
    }
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> std::result::Result<T, EvalError> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn finalize_scenario(accumulator: ScenarioAccumulator, mode: FailedRunsMode) -> ScenarioBenchmark {
    let zero_failed_count = if mode == FailedRunsMode::Zero {
        accumulator.failed.len()
    } else {
        0
    };

    ScenarioBenchmark {
        completed: summarize_completed(&accumulator.completed, zero_failed_count),
        failed: if mode == FailedRunsMode::Bucket && !accumulator.failed.is_empty() {
            Some(summarize_failed_bucket(&accumulator.failed))
        } else {
            None
        },
        skipped: SkippedBucket {
            run_count: accumulator.skipped,
        },
    }
}

fn summarize_completed(samples: &[RunSample], zero_failed_count: usize) -> CompletedBucket {
    let mut missing_grading = 0usize;
    let mut missing_timing = 0usize;
    let mut assertion_passed = 0usize;
    let mut assertion_failed = 0usize;
    let mut run_passed = 0usize;
    let mut run_failed = 0usize;
    let mut durations = Vec::new();
    let mut token_totals = Vec::new();
    let mut token_inputs = Vec::new();
    let mut token_outputs = Vec::new();
    let mut costs = Vec::new();

    for sample in samples {
        if sample.missing_grading {
            missing_grading += 1;
        } else if let Some(grading) = &sample.grading {
            let summary = grading_summary(grading);
            assertion_passed += summary.passed;
            assertion_failed += summary.failed;

            if summary.failed == 0 && summary.total > 0 {
                run_passed += 1;
            } else if summary.total > 0 {
                run_failed += 1;
            }
        }

        if sample.missing_timing {
            missing_timing += 1;
        } else if let Some(timing) = &sample.timing {
            if let Some(duration_ms) = timing.duration_ms {
                durations.push(duration_ms);
            }
            push_if_some(&mut token_totals, timing.total_tokens);
            push_if_some(&mut token_inputs, timing.input_tokens);
            push_if_some(&mut token_outputs, timing.output_tokens);
            push_if_some(&mut costs, timing.cost());
        }
    }

    run_failed += zero_failed_count;

    let run_count = samples.len() + zero_failed_count;

    CompletedBucket {
        run_count,
        assertions: pass_fail_summary(assertion_passed, assertion_failed),
        runs: pass_fail_summary(run_passed, run_failed),
        duration_ms: duration_stats(&durations),
        tokens: token_stats(&token_totals, &token_inputs, &token_outputs, &costs, run_count),
        missing_grading,
        missing_timing,
    }
}

fn summarize_failed_bucket(samples: &[RunSample]) -> RunBucketSummary {
    let mut missing_timing = 0usize;
    let mut durations = Vec::new();
    let mut token_totals = Vec::new();
    let mut token_inputs = Vec::new();
    let mut token_outputs = Vec::new();
    let mut costs = Vec::new();

    for sample in samples {
        if sample.missing_timing {
            missing_timing += 1;
        } else if let Some(timing) = &sample.timing {
            if let Some(duration_ms) = timing.duration_ms {
                durations.push(duration_ms);
            }
            push_if_some(&mut token_totals, timing.total_tokens);
            push_if_some(&mut token_inputs, timing.input_tokens);
            push_if_some(&mut token_outputs, timing.output_tokens);
            push_if_some(&mut costs, timing.cost());
        }
    }

    RunBucketSummary {
        run_count: samples.len(),
        duration_ms: duration_stats(&durations),
        tokens: token_stats(&token_totals, &token_inputs, &token_outputs, &costs, samples.len()),
        missing_timing,
    }
}

fn grading_summary(grading: &GradingFileInput) -> PassFailSummary {
    if let Some(summary) = &grading.summary {
        if summary.total > 0 {
            return pass_fail_summary(summary.passed, summary.failed);
        }
    }

    let scored = grading.assertion_results.iter().filter(|result| result.is_scored());
    let passed = scored.clone().filter(|result| result.passed).count();
    let failed = scored.count().saturating_sub(passed);
    pass_fail_summary(passed, failed)
}

fn pass_fail_summary(passed: usize, failed: usize) -> PassFailSummary {
    let total = passed + failed;
    let pass_rate = if total == 0 { 0.0 } else { passed as f64 / total as f64 };
    PassFailSummary {
        passed,
        failed,
        total,
        pass_rate,
    }
}

pub fn duration_stats(values: &[u64]) -> DurationStats {
    if values.is_empty() {
        return DurationStats::default();
    }

    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let total: u64 = sorted.iter().sum();
    let mean = total as f64 / sorted.len() as f64;
    let mut stats = DurationStats {
        mean,
        p50: percentile(&sorted, 0.50),
        p95: percentile(&sorted, 0.95),
        total,
        stddev: None,
        median: None,
        mad: None,
    };
    if sorted.len() >= 3 {
        stats.stddev = Some(stddev(&sorted, mean));
    }
    if dispersion::SampleFloor::dispersion().admits(values.len()) {
        if let Some(measured) = dispersion::Dispersion::measure(values) {
            stats.median = Some(measured.centre());
            stats.mad = Some(measured.spread());
        }
    }
    stats
}

pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let position = (sorted.len() as f64 - 1.0) * p;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower.min(sorted.len() - 1)]
    } else {
        let weight = position - lower as f64;
        let value = sorted[lower] as f64 * (1.0 - weight) + sorted[upper] as f64 * weight;
        value.round() as u64
    }
}

pub fn stddev(values: &[u64], mean: f64) -> f64 {
    if values.len() < 3 {
        return 0.0;
    }
    let variance = values
        .iter()
        .map(|value| {
            let delta = *value as f64 - mean;
            delta * delta
        })
        .sum::<f64>()
        / values.len() as f64;
    variance.sqrt()
}

fn token_stats(totals: &[u64], inputs: &[u64], outputs: &[u64], costs: &[RunCost], run_count: usize) -> TokenStats {
    TokenStats {
        total: sum_optional(totals),
        input: sum_optional(inputs),
        output: sum_optional(outputs),
        cost: bucket_cost(costs, run_count),
    }
}

/// A bucket holding one run a harness refuses to price cannot be totalled at all: adding
/// up the rest would publish the priced runs' total as the bucket's cost and read the
/// unpriced ones as runs that were free.
fn bucket_cost(costs: &[RunCost], run_count: usize) -> Option<BucketCost> {
    if let Some(RunCost::Unpriced { harness }) = costs.iter().find(|cost| cost.is_unpriced()) {
        return Some(BucketCost::Unpriced {
            harness: harness.clone(),
        });
    }

    let priced: Vec<f64> = costs.iter().filter_map(RunCost::usd).collect();
    if priced.is_empty() {
        return None;
    }
    let usd = priced.iter().sum();
    match priced.len() == run_count {
        true => Some(BucketCost::Whole { usd }),
        false => Some(BucketCost::Partial {
            usd,
            runs: priced.len(),
        }),
    }
}

/// Only a difference between two totals that each cover their whole arm is a cost
/// difference; anything else subtracts two different questions from each other.
fn whole_arm_cost(bucket: &CompletedBucket) -> Option<f64> {
    bucket.tokens.cost.as_ref()?.whole_bucket_usd()
}

fn sum_optional(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum())
    }
}

fn push_if_some<T>(values: &mut Vec<T>, value: Option<T>) {
    if let Some(value) = value {
        values.push(value);
    }
}

fn compute_scenario_deltas(scenarios: &[(ScenarioKind, ScenarioBenchmark)]) -> ScenarioDeltas {
    let with_skill = scenarios
        .iter()
        .find(|(kind, _)| *kind == ScenarioKind::WithSkill)
        .map(|(_, bench)| bench);
    let without_skill = scenarios
        .iter()
        .find(|(kind, _)| *kind == ScenarioKind::WithoutSkill)
        .map(|(_, bench)| bench);
    let old_skill = scenarios
        .iter()
        .find(|(kind, _)| *kind == ScenarioKind::OldSkill)
        .map(|(_, bench)| bench);

    ScenarioDeltas {
        with_skill_vs_without_skill: delta_between(with_skill, without_skill),
        with_skill_vs_old_skill: delta_between(with_skill, old_skill),
    }
}

fn delta_between(left: Option<&ScenarioBenchmark>, right: Option<&ScenarioBenchmark>) -> Option<ScenarioDelta> {
    let left = left?;
    let right = right?;
    if left.completed.run_count == 0 || right.completed.run_count == 0 {
        return None;
    }

    Some(ScenarioDelta {
        assertion_pass_rate: left.completed.assertions.pass_rate - right.completed.assertions.pass_rate,
        assertion_pass_rate_interval: difference_interval(&left.completed.assertions, &right.completed.assertions),
        run_pass_rate: left.completed.runs.pass_rate - right.completed.runs.pass_rate,
        run_pass_rate_interval: difference_interval(&left.completed.runs, &right.completed.runs),
        duration_ms_mean: left.completed.duration_ms.mean - right.completed.duration_ms.mean,
        tokens_total: diff_optional(left.completed.tokens.total, right.completed.tokens.total),
        cost_usd: diff_optional_f64(whole_arm_cost(&left.completed), whole_arm_cost(&right.completed)),
        left: arm_observations(left),
        right: arm_observations(right),
    })
}

/// Nothing when either side scored nothing: a difference against an arm with no draws
/// behind it has no width to report, and a missing interval says that where a very wide
/// one would read as a measurement.
fn difference_interval(left: &PassFailSummary, right: &PassFailSummary) -> Option<Interval> {
    let left = Proportion::observed(left.passed, left.failed)?;
    let right = Proportion::observed(right.passed, right.failed)?;
    Some(proportion::difference(left, right))
}

fn arm_observations(bench: &ScenarioBenchmark) -> ArmObservations {
    ArmObservations {
        runs: bench.completed.run_count,
        duration_median_ms: bench.completed.duration_ms.median,
        duration_mad_ms: bench.completed.duration_ms.mad,
    }
}

fn diff_optional(left: Option<u64>, right: Option<u64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left as i64 - right as i64),
        _ => None,
    }
}

fn diff_optional_f64(left: Option<f64>, right: Option<f64>) -> Option<f64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left - right),
        _ => None,
    }
}

fn build_iteration_comparison(
    report_dir: &Path,
    report: &ReportForBenchmark,
    failed_runs: FailedRunsMode,
) -> Option<IterationComparison> {
    if let Some(iteration) = &report.report.iteration {
        if let Some(previous_id) = &iteration.previous_id {
            return compare_iteration_ids(report_dir, &report.runs, &iteration.id, previous_id, failed_runs);
        }
    }

    let mut iteration_ids: Vec<String> = report.runs.iter().filter_map(|run| run.iteration_id.clone()).collect();
    iteration_ids.sort();
    iteration_ids.dedup();

    if iteration_ids.len() >= 2 {
        let previous_id = iteration_ids[iteration_ids.len() - 2].clone();
        let current_id = iteration_ids[iteration_ids.len() - 1].clone();
        return compare_iteration_ids(report_dir, &report.runs, &current_id, &previous_id, failed_runs);
    }

    None
}

fn compare_iteration_ids(
    report_dir: &Path,
    runs: &[RunForBenchmark],
    current_id: &str,
    previous_id: &str,
    failed_runs: FailedRunsMode,
) -> Option<IterationComparison> {
    let current = aggregate_runs_for_iteration(report_dir, runs, current_id, failed_runs);
    let previous = aggregate_runs_for_iteration(report_dir, runs, previous_id, failed_runs);

    if current.is_empty() || previous.is_empty() {
        return None;
    }

    let by_scenario: BTreeMap<String, ScenarioDelta> = ScenarioKind::ALL
        .iter()
        .filter_map(|scenario| {
            let current_bench = current.get(scenario)?;
            let previous_bench = previous.get(scenario)?;
            delta_between(Some(current_bench), Some(previous_bench)).map(|delta| (scenario.as_str().to_string(), delta))
        })
        .collect();

    if by_scenario.is_empty() {
        return None;
    }

    Some(IterationComparison {
        current_iteration_id: current_id.to_string(),
        previous_iteration_id: previous_id.to_string(),
        by_scenario,
    })
}

fn aggregate_runs_for_iteration(
    report_dir: &Path,
    runs: &[RunForBenchmark],
    iteration_id: &str,
    mode: FailedRunsMode,
) -> HashMap<ScenarioKind, ScenarioBenchmark> {
    let mut by_scenario: HashMap<ScenarioKind, ScenarioAccumulator> = HashMap::new();

    for run in runs {
        if run.iteration_id.as_deref() != Some(iteration_id) {
            continue;
        }

        let entry = by_scenario.entry(run.scenario_id).or_default();
        let sample = load_run_sample(report_dir, &run.paths.workspace);
        match classify_run(run, mode) {
            RunDisposition::Completed => entry.completed.push(sample),
            RunDisposition::Failed => entry.failed.push(sample),
            RunDisposition::Skipped => entry.skipped += 1,
            RunDisposition::Excluded => {}
        }
    }

    ScenarioKind::ALL
        .iter()
        .map(|scenario| {
            let accumulator = by_scenario.remove(scenario).unwrap_or_default();
            (*scenario, finalize_scenario(accumulator, mode))
        })
        .filter(|(_, bench)| bench.completed.run_count > 0 || bench.failed.is_some() || bench.skipped.run_count > 0)
        .collect()
}

fn build_by_eval_scenario(
    report_dir: &Path,
    runs: &[RunForBenchmark],
    mode: FailedRunsMode,
) -> (Vec<EvalScenarioAttemptRow>, Vec<FlakyAssertionRecord>) {
    let mut groups: BTreeMap<(String, ScenarioKind), Vec<&RunForBenchmark>> = BTreeMap::new();
    for run in runs {
        if matches!(classify_run(run, mode), RunDisposition::Excluded) {
            continue;
        }
        groups
            .entry((run.eval_case_id.clone(), run.scenario_id))
            .or_default()
            .push(run);
    }

    let mut ledger = FlakinessLedger::default();
    let mut rows = Vec::new();
    for ((eval_case_id, scenario_id), group_runs) in groups {
        let mut pass_rates = Vec::new();
        for run in &group_runs {
            if !matches!(classify_run(run, mode), RunDisposition::Completed) {
                continue;
            }
            let sample = load_run_sample(report_dir, &run.paths.workspace);
            if sample.missing_grading {
                continue;
            }
            if let Some(grading) = sample.grading {
                let summary = grading_summary(&grading);
                if summary.total > 0 {
                    pass_rates.push(summary.pass_rate);
                }
                record_assertion_outcomes(&mut ledger, &eval_case_id, scenario_id, run.attempt, &grading);
            }
        }

        rows.push(EvalScenarioAttemptRow {
            eval_case_id,
            scenario_id: scenario_id.as_str().to_string(),
            attempt_count: group_runs.len() as u32,
            pass_rate: attempt_pass_rate_stats(&pass_rates),
            flaky_assertions: Vec::new(),
        });
    }

    let records = ledger.flaky_records();
    for row in &mut rows {
        row.flaky_assertions = records
            .iter()
            .filter(|record| record.eval_case_id == row.eval_case_id && record.scenario_id == row.scenario_id)
            .map(|record| record.assertion.clone())
            .collect();
    }

    (rows, records)
}

fn record_assertion_outcomes(
    ledger: &mut FlakinessLedger,
    eval_case_id: &str,
    scenario_id: ScenarioKind,
    attempt: u32,
    grading: &GradingFileInput,
) {
    for result in &grading.assertion_results {
        if !result.is_scored() {
            continue;
        }
        let Ok(key) = AssertionKey::parse(eval_case_id, scenario_id, &result.assertion) else {
            continue;
        };
        ledger.observe(key, attempt, result.passed);
    }
}

fn attempt_pass_rate_stats(pass_rates: &[f64]) -> AttemptPassRateStats {
    if pass_rates.is_empty() {
        return AttemptPassRateStats {
            mean: 0.0,
            variance: None,
            min: 0.0,
            max: 0.0,
        };
    }

    let min = pass_rates.iter().copied().fold(f64::INFINITY, f64::min);
    let max = pass_rates.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mean = pass_rates.iter().sum::<f64>() / pass_rates.len() as f64;
    let variance = if pass_rates.len() >= 2 {
        Some(pass_rate_variance(pass_rates, mean))
    } else {
        None
    };

    AttemptPassRateStats {
        mean,
        variance,
        min,
        max,
    }
}

pub fn pass_rate_variance(pass_rates: &[f64], mean: f64) -> f64 {
    if pass_rates.is_empty() {
        return 0.0;
    }

    pass_rates
        .iter()
        .map(|rate| {
            let delta = *rate - mean;
            delta * delta
        })
        .sum::<f64>()
        / pass_rates.len() as f64
}

fn normalize_assertion_key(assertion: &str) -> String {
    assertion.trim().to_string()
}

fn build_iteration_summary(
    report_dir: &Path,
    runs: &[RunForBenchmark],
    flaky_assertions: Vec<FlakyAssertionRecord>,
    mode: FailedRunsMode,
) -> IterationSummary {
    let mut assertion_outcomes: BTreeMap<(String, String), (usize, usize)> = BTreeMap::new();
    let mut scenario_assertion_rates: BTreeMap<(ScenarioKind, String, String), (usize, usize)> = BTreeMap::new();

    for run in runs {
        if !matches!(classify_run(run, mode), RunDisposition::Completed) {
            continue;
        }
        let sample = load_run_sample(report_dir, &run.paths.workspace);
        let Some(grading) = &sample.grading else {
            continue;
        };
        for result in &grading.assertion_results {
            let assertion_key = normalize_assertion_key(&result.assertion);
            if assertion_key.is_empty() || !result.is_scored() {
                continue;
            }
            let key = (run.eval_case_id.clone(), assertion_key);
            let entry = assertion_outcomes.entry(key.clone()).or_insert((0, 0));
            if result.passed {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
            let scenario_entry = scenario_assertion_rates
                .entry((run.scenario_id, key.0, key.1))
                .or_insert((0, 0));
            if result.passed {
                scenario_entry.0 += 1;
            } else {
                scenario_entry.1 += 1;
            }
        }
    }

    let always_pass = assertion_outcomes
        .iter()
        .filter(|(_, (passed, failed))| *passed > 0 && *failed == 0)
        .map(|((eval_case_id, assertion), _)| format!("{eval_case_id}: {assertion}"))
        .collect();

    let always_fail = assertion_outcomes
        .iter()
        .filter(|(_, (passed, failed))| *failed > 0 && *passed == 0)
        .map(|((eval_case_id, assertion), _)| format!("{eval_case_id}: {assertion}"))
        .collect();

    let assertion_pairs: Vec<(String, String)> = scenario_assertion_rates
        .keys()
        .map(|(_, eval_case_id, assertion)| (eval_case_id.clone(), assertion.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let helped_by_skill: Vec<String> = assertion_pairs
        .into_iter()
        .filter_map(|(eval_case_id, assertion)| {
            let with_rates = scenario_pass_rate(
                &scenario_assertion_rates,
                ScenarioKind::WithSkill,
                &eval_case_id,
                &assertion,
            )?;
            let without_rates = scenario_pass_rate(
                &scenario_assertion_rates,
                ScenarioKind::WithoutSkill,
                &eval_case_id,
                &assertion,
            )?;
            (with_rates > without_rates).then(|| format!("{eval_case_id}: {assertion}"))
        })
        .collect();

    let timing_outliers = detect_metric_outliers(report_dir, runs, mode, MetricKind::Duration);
    let token_outliers = detect_metric_outliers(report_dir, runs, mode, MetricKind::Tokens);

    IterationSummary {
        always_pass,
        always_fail,
        helped_by_skill,
        flaky_assertions,
        timing_outliers,
        token_outliers,
    }
}

fn scenario_pass_rate(
    rates: &BTreeMap<(ScenarioKind, String, String), (usize, usize)>,
    scenario: ScenarioKind,
    eval_case_id: &str,
    assertion: &str,
) -> Option<f64> {
    let (passed, failed) = rates.get(&(scenario, eval_case_id.to_string(), assertion.to_string()))?;
    let total = passed + failed;
    if total == 0 {
        None
    } else {
        Some(*passed as f64 / total as f64)
    }
}

enum MetricKind {
    Duration,
    Tokens,
}

struct BenchmarkRunOrigin {
    run_id: String,
    eval_case_id: String,
    attempt: u32,
}

fn detect_metric_outliers(
    report_dir: &Path,
    runs: &[RunForBenchmark],
    mode: FailedRunsMode,
    kind: MetricKind,
) -> Vec<MetricOutlier> {
    let mut samples = Vec::new();
    for run in runs {
        if !matches!(classify_run(run, mode), RunDisposition::Completed) {
            continue;
        }
        let sample = load_run_sample(report_dir, &run.paths.workspace);
        let Some(timing) = &sample.timing else {
            continue;
        };
        let value = match kind {
            MetricKind::Duration => timing.duration_ms,
            MetricKind::Tokens => timing.total_tokens,
        };
        if let Some(value) = value {
            samples.push(MetricSample::new(
                run.scenario_id,
                BenchmarkRunOrigin {
                    run_id: run.id.clone(),
                    eval_case_id: run.eval_case_id.clone(),
                    attempt: run.attempt,
                },
                value,
            ));
        }
    }

    dispersion::outliers(samples)
        .into_iter()
        .map(|outlier| {
            let (sample, spread) = outlier.into_parts();
            let (scenario, origin, value) = sample.into_parts();
            MetricOutlier {
                run_id: origin.run_id,
                eval_case_id: origin.eval_case_id,
                scenario_id: scenario.as_str().to_string(),
                attempt: origin.attempt,
                value,
                median: spread.centre(),
                mad: spread.spread(),
            }
        })
        .collect()
}

impl ScenarioKind {
    const ALL: [ScenarioKind; 3] = [
        ScenarioKind::WithSkill,
        ScenarioKind::WithoutSkill,
        ScenarioKind::OldSkill,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_report(report_dir: &Path, runs: serde_json::Value, iteration: Option<serde_json::Value>) {
        fs::create_dir_all(report_dir).unwrap();
        let mut report = serde_json::json!({
            "report": {
                "id": "report-test",
                "generated_at": "2026-05-26T00:00:00Z",
                "producer": { "name": "trg", "version": "0.3.0" }
            },
            "suite": {
                "skill_name": "demo",
                "skill_path": "demo",
                "skill_hash": "sha256:abc",
                "evals_path": "demo/evals/evals.json",
                "evals_hash": "sha256:def"
            },
            "dimensions": {
                "eval_cases": [],
                "assertions": [],
                "skill_revisions": [],
                "model_configs": [],
                "scenarios": [],
                "grading_strategies": []
            },
            "runs": runs,
            "assertion_results": [],
            "summaries": { "by_scenario": [] },
            "comparisons": []
        });
        if let Some(iteration) = iteration {
            report["report"]["iteration"] = iteration;
        }
        fs::write(
            report_dir.join("report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
    }

    fn write_run_artifacts(report_dir: &Path, run_id: &str, grading: Option<&str>, timing: Option<&str>) {
        let run_dir = report_dir.join(format!("runs/{run_id}"));
        let workspace = run_dir.join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        if let Some(grading) = grading {
            fs::write(run_dir.join("grading.json"), grading).unwrap();
        }
        if let Some(timing) = timing {
            fs::write(run_dir.join("timing.json"), timing).unwrap();
        }
    }

    fn sample_run(id: &str, scenario: &str, status: &str, iteration_id: Option<&str>) -> serde_json::Value {
        let mut run = serde_json::json!({
            "id": id,
            "eval_case_id": "case-a",
            "scenario_id": scenario,
            "model_config_id": "ci-default",
            "skill_revision_id": "current",
            "attempt": 1,
            "status": status,
            "paths": { "workspace": format!("runs/{id}/workspace") },
            "artifacts": [],
            "metrics": {}
        });
        if let Some(iteration_id) = iteration_id {
            run["iteration_id"] = serde_json::json!(iteration_id);
        }
        run
    }

    #[test]
    fn read_report_iteration_accepts_numeric_and_object_forms() {
        let temp = tempfile::tempdir().unwrap();

        write_report(temp.path(), serde_json::json!([]), Some(serde_json::json!(3)));
        assert_eq!(read_report_iteration(temp.path()).unwrap(), 3);

        write_report(
            temp.path(),
            serde_json::json!([]),
            Some(serde_json::json!({ "id": "iter-2", "index": 2, "previous_id": "iter-1" })),
        );
        assert_eq!(read_report_iteration(temp.path()).unwrap(), 2);

        write_report(
            temp.path(),
            serde_json::json!([]),
            Some(serde_json::json!({ "id": "iter-4", "iteration": 4 })),
        );
        assert_eq!(read_report_iteration(temp.path()).unwrap(), 4);
    }

    #[test]
    fn write_benchmark_mirrors_to_iteration_dir_for_object_iteration() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([sample_run("run-001", "with_skill", "completed", None)]),
            Some(serde_json::json!({ "id": "iter-2", "index": 2, "previous_id": "iter-1" })),
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(
                r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000 }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        write_benchmark(temp.path(), &benchmark).unwrap();

        assert!(temp.path().join("benchmark.json").is_file());
        assert!(temp.path().join("iteration-2/benchmark.json").is_file());
    }

    #[test]
    fn duration_stats_omit_stddev_for_small_samples() {
        let two = duration_stats(&[100, 200]);
        assert!(two.stddev.is_none());

        let three = duration_stats(&[100, 200, 300]);
        assert!(three.stddev.is_some());
    }

    #[test]
    fn duration_stats_withhold_a_spread_below_the_dispersion_floor() {
        let three = duration_stats(&[100, 200, 300]);
        assert!(
            three.median.is_none(),
            "three draws cannot describe their own spread yet"
        );

        let four = duration_stats(&[100, 200, 300, 400]);
        assert_eq!(four.median, Some(250.0));
        assert_eq!(four.mad, Some(100.0));
    }

    #[test]
    fn percentile_and_stddev_helpers() {
        assert_eq!(percentile(&[10, 20], 0.50), 15);
        assert_eq!(percentile(&[10, 20, 30, 40], 0.50), 25);
        assert_eq!(percentile(&[10, 20, 30, 40], 0.95), 39);
        assert!((stddev(&[2, 4, 6], 4.0) - 1.632993161855452).abs() < 0.0001);
    }

    #[test]
    fn aggregates_pass_rates_and_missing_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "without_skill", "completed", None),
            ]),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(
                r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 2, "failed": 0, "total": 2, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000, "total_tokens": 100, "input_tokens": 60, "output_tokens": 40 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(
                r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": false, "evidence": "missing" }
  ],
  "summary": { "passed": 1, "failed": 1, "total": 2, "pass_rate": 0.5 }
}"#,
            ),
            None,
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let with_skill = &benchmark.scenarios["with_skill"];
        assert_eq!(with_skill.completed.assertions.pass_rate, 1.0);
        assert_eq!(with_skill.completed.missing_timing, 0);
        assert_eq!(with_skill.completed.missing_grading, 0);

        let without_skill = &benchmark.scenarios["without_skill"];
        assert_eq!(without_skill.completed.assertions.pass_rate, 0.5);
        assert_eq!(without_skill.completed.missing_timing, 1);

        let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();
        assert!((delta.assertion_pass_rate - 0.5).abs() < 0.0001);

        let interval = delta
            .assertion_pass_rate_interval
            .expect("both arms scored assertions, so the difference has a width");
        assert!(
            !interval.separates_the_arms(),
            "two assertions against two read as a finding at {interval:?}"
        );
    }

    #[test]
    fn a_difference_against_an_arm_that_scored_nothing_publishes_no_interval() {
        let left = PassFailSummary {
            passed: 2,
            failed: 0,
            total: 2,
            pass_rate: 1.0,
        };
        let nothing = PassFailSummary {
            passed: 0,
            failed: 0,
            total: 0,
            pass_rate: 0.0,
        };

        assert!(difference_interval(&left, &nothing).is_none());
        assert!(difference_interval(&left, &left).is_some());
    }

    #[test]
    fn failed_runs_bucket_exclude_and_zero_modes() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "with_skill", "failed", None),
            ]),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(
                r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000 }"#),
        );
        write_run_artifacts(temp.path(), "run-002", None, Some(r#"{ "duration_ms": 500 }"#));

        let bucket = build_benchmark(
            temp.path(),
            BenchmarkOptions {
                failed_runs: FailedRunsMode::Bucket,
                ..BenchmarkOptions::default()
            },
        )
        .unwrap();
        assert_eq!(bucket.scenarios["with_skill"].completed.run_count, 1);
        assert_eq!(bucket.scenarios["with_skill"].failed.as_ref().unwrap().run_count, 1);

        let exclude = build_benchmark(
            temp.path(),
            BenchmarkOptions {
                failed_runs: FailedRunsMode::Exclude,
                ..BenchmarkOptions::default()
            },
        )
        .unwrap();
        assert_eq!(exclude.scenarios["with_skill"].completed.run_count, 1);
        assert!(exclude.scenarios["with_skill"].failed.is_none());

        let zero = build_benchmark(
            temp.path(),
            BenchmarkOptions {
                failed_runs: FailedRunsMode::Zero,
                ..BenchmarkOptions::default()
            },
        )
        .unwrap();
        assert_eq!(zero.scenarios["with_skill"].completed.run_count, 2);
        assert_eq!(zero.scenarios["with_skill"].completed.runs.passed, 1);
        assert_eq!(zero.scenarios["with_skill"].completed.runs.failed, 1);
        assert_eq!(zero.scenarios["with_skill"].completed.missing_grading, 0);
    }

    #[test]
    fn unparseable_grading_counts_as_missing() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([sample_run("run-001", "with_skill", "completed", None)]),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some("{ this is not valid json"),
            Some(r#"{ "duration_ms": 1000 }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let with_skill = &benchmark.scenarios["with_skill"];
        assert_eq!(with_skill.completed.run_count, 1);
        assert_eq!(with_skill.completed.missing_grading, 1);
        assert_eq!(with_skill.completed.runs.passed, 0);
        assert_eq!(with_skill.completed.runs.failed, 0);
    }

    #[test]
    fn mixed_completed_failed_skipped_and_missing_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "with_skill", "failed", None),
                sample_run("run-003", "with_skill", "skipped", None),
                sample_run("run-004", "without_skill", "completed", None),
            ]),
            None,
        );
        write_run_artifacts(temp.path(), "run-001", None, Some(r#"{ "duration_ms": 1000 }"#));
        write_run_artifacts(
            temp.path(),
            "run-002",
            None,
            Some(r#"{ "duration_ms": 2000, "total_tokens": 50 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-004",
            Some(
                r#"{
  "assertion_results": [{ "assertion": "a", "passed": false, "evidence": "nope" }],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 3000 }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let with_skill = &benchmark.scenarios["with_skill"];
        assert_eq!(with_skill.completed.run_count, 1);
        assert_eq!(with_skill.completed.missing_grading, 1);
        assert_eq!(with_skill.failed.as_ref().unwrap().run_count, 1);
        assert_eq!(with_skill.skipped.run_count, 1);

        let without_skill = &benchmark.scenarios["without_skill"];
        assert_eq!(without_skill.completed.runs.failed, 1);
    }

    #[test]
    fn an_arm_scoped_check_does_not_widen_the_gap_between_the_arms() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "without_skill", "completed", None),
            ]),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(
                r#"{
  "assertion_results": [
    { "assertion": "the skill was engaged", "passed": true, "evidence": "read SKILL.md", "excluded": "presupposes the skill" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 1, "failed": 0, "total": 2, "excluded": 1, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(
                r#"{
  "assertion_results": [
    { "assertion": "the skill was engaged", "passed": false, "evidence": "never read", "excluded": "presupposes the skill" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 1, "failed": 0, "total": 2, "excluded": 1, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000 }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();
        assert!(
            delta.assertion_pass_rate.abs() < 0.0001,
            "the skill was credited for its own premise: {}",
            delta.assertion_pass_rate
        );
        assert!(benchmark.iteration_summary.always_fail.is_empty());
    }

    #[test]
    fn iteration_comparison_when_metadata_present() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", Some("iter-1")),
                sample_run("run-002", "with_skill", "completed", Some("iter-2")),
            ]),
            Some(serde_json::json!({ "id": "iter-2", "index": 2, "previous_id": "iter-1" })),
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(
                r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(
                r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 2, "failed": 0, "total": 2, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 800 }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let comparison = benchmark.iteration_comparison.as_ref().unwrap();
        assert_eq!(comparison.current_iteration_id, "iter-2");
        assert_eq!(comparison.previous_iteration_id, "iter-1");
        let delta = comparison.by_scenario.get("with_skill").unwrap();
        assert!((delta.assertion_pass_rate - 0.0).abs() < 0.0001);
        assert!((delta.duration_ms_mean - (-200.0)).abs() < 0.0001);
        assert_eq!(delta.left.runs, 1);
        assert_eq!(delta.right.runs, 1);
    }

    #[test]
    fn a_scenario_delta_carries_how_many_runs_stood_behind_each_arm() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "with_skill", "completed", None),
                sample_run("run-003", "without_skill", "completed", None),
            ]),
            None,
        );
        for run_id in ["run-001", "run-002", "run-003"] {
            write_run_artifacts(
                temp.path(),
                run_id,
                Some(
                    r#"{
  "assertion_results": [{ "assertion": "a", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
                ),
                Some(r#"{ "duration_ms": 1000 }"#),
            );
        }

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let delta = benchmark.deltas.with_skill_vs_without_skill.as_ref().unwrap();
        assert_eq!(delta.left.runs, 2);
        assert_eq!(delta.right.runs, 1);
        assert!(
            delta.left.duration_median_ms.is_none(),
            "two draws is below the floor a spread needs to describe itself"
        );
    }

    #[test]
    fn flaky_assertions_detect_pass_flip_across_attempts() {
        let temp = tempfile::tempdir().unwrap();
        let mut run1 = sample_run("run-001", "with_skill", "completed", None);
        let mut run2 = sample_run("run-002", "with_skill", "completed", None);
        run1["eval_case_id"] = serde_json::json!("case-a");
        run2["eval_case_id"] = serde_json::json!("case-a");
        run1["attempt"] = serde_json::json!(1);
        run2["attempt"] = serde_json::json!(2);
        write_report(temp.path(), serde_json::json!([run1, run2]), None);
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(
                r#"{
  "assertion_results": [{ "assertion": "checks output", "passed": true, "evidence": "ok" }],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1000 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(
                r#"{
  "assertion_results": [{ "assertion": "checks output", "passed": false, "evidence": "nope" }],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1100 }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let row = benchmark
            .by_eval_scenario
            .iter()
            .find(|row| row.eval_case_id == "case-a")
            .unwrap();
        assert_eq!(row.attempt_count, 2);
        assert_eq!(row.flaky_assertions, vec!["checks output"]);
        assert_eq!(benchmark.iteration_summary.flaky_assertions.len(), 1);
    }

    #[test]
    fn pass_rate_variance_across_attempts() {
        let rates = [1.0, 0.5, 0.0];
        let mean = rates.iter().sum::<f64>() / rates.len() as f64;
        let variance = pass_rate_variance(&rates, mean);
        assert!((variance - 0.16666666666666666).abs() < 0.0001);

        let stats = attempt_pass_rate_stats(&rates);
        assert!((stats.mean - mean).abs() < 0.0001);
        assert!((stats.variance.unwrap() - variance).abs() < 0.0001);
        assert_eq!(stats.min, 0.0);
        assert_eq!(stats.max, 1.0);

        assert_eq!(pass_rate_variance(&[], 0.0), 0.0);
    }

    fn write_timed_runs(report_dir: &Path, timings: &[(&str, &str, u32, u64, u64)]) {
        let runs: Vec<serde_json::Value> = timings
            .iter()
            .map(|(id, scenario, attempt, _, _)| {
                let mut run = sample_run(id, scenario, "completed", None);
                run["attempt"] = serde_json::json!(attempt);
                run
            })
            .collect();
        write_report(report_dir, serde_json::json!(runs), None);
        for (id, _, _, duration_ms, total_tokens) in timings {
            write_run_artifacts(
                report_dir,
                id,
                None,
                Some(&format!(
                    r#"{{ "duration_ms": {duration_ms}, "total_tokens": {total_tokens} }}"#
                )),
            );
        }
    }

    #[test]
    fn a_group_below_the_sample_floor_reports_no_outlier() {
        let temp = tempfile::tempdir().unwrap();
        write_timed_runs(
            temp.path(),
            &[
                ("run-001", "with_skill", 1, 1000, 100),
                ("run-002", "with_skill", 2, 1200, 120),
                ("run-003", "with_skill", 3, 9000, 900),
            ],
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        assert!(benchmark.iteration_summary.timing_outliers.is_empty());
        assert!(benchmark.iteration_summary.token_outliers.is_empty());
    }

    /// The arms pay different costs by construction, so a pooled centre sits between
    /// them and publishes the slower arm as a field of outliers against the faster one.
    #[test]
    fn each_arm_is_judged_against_its_own_cost_profile() {
        let temp = tempfile::tempdir().unwrap();
        write_timed_runs(
            temp.path(),
            &[
                ("run-001", "with_skill", 1, 1000, 100),
                ("run-002", "with_skill", 2, 1020, 102),
                ("run-003", "with_skill", 3, 980, 98),
                ("run-004", "with_skill", 4, 1010, 101),
                ("run-005", "without_skill", 1, 29000, 2900),
                ("run-006", "without_skill", 2, 30000, 3000),
                ("run-007", "without_skill", 3, 30500, 3050),
                ("run-008", "without_skill", 4, 31000, 3100),
            ],
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        assert!(benchmark.iteration_summary.timing_outliers.is_empty());
        assert!(benchmark.iteration_summary.token_outliers.is_empty());
    }

    #[test]
    fn an_outlier_is_published_with_the_band_its_own_arm_drew() {
        let temp = tempfile::tempdir().unwrap();
        write_timed_runs(
            temp.path(),
            &[
                ("run-001", "with_skill", 1, 1000, 100),
                ("run-002", "with_skill", 2, 1020, 102),
                ("run-003", "with_skill", 3, 980, 98),
                ("run-004", "with_skill", 4, 1010, 101),
                ("run-005", "with_skill", 5, 9000, 900),
            ],
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let timing = &benchmark.iteration_summary.timing_outliers;
        assert_eq!(timing.len(), 1);
        assert_eq!(timing[0].run_id, "run-005");
        assert_eq!(timing[0].attempt, 5);
        assert_eq!(timing[0].value, 9000);
        assert_eq!(timing[0].median, 1010.0);
        assert_eq!(timing[0].mad, 10.0);

        let tokens = &benchmark.iteration_summary.token_outliers;
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].value, 900);
        assert_eq!(tokens[0].median, 101.0);
    }

    #[test]
    fn benchmark_and_iteration_summary_agree_on_the_same_pass() {
        use crate::agentskills::iteration_summary::{build_iteration_summary_document, IterationSummaryOptions};

        let temp = tempfile::tempdir().unwrap();
        let ids = ["run-001", "run-002", "run-003", "run-004", "run-005"];
        let durations = [1000_u64, 1020, 980, 1010, 9000];
        let tokens = [100_u64, 102, 98, 101, 900];
        let passed = [true, true, true, true, false];

        let runs: Vec<serde_json::Value> = ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let mut run = sample_run(id, "with_skill", "completed", None);
                run["attempt"] = serde_json::json!(index as u32 + 1);
                run
            })
            .collect();
        write_report(temp.path(), serde_json::json!(runs), Some(serde_json::json!(1)));
        for (index, id) in ids.iter().enumerate() {
            write_run_artifacts(
                temp.path(),
                id,
                Some(&format!(
                    r#"{{ "assertion_results": [{{ "assertion": "finishes without error", "passed": {} }}] }}"#,
                    passed[index]
                )),
                Some(&format!(
                    r#"{{ "duration_ms": {}, "total_tokens": {} }}"#,
                    durations[index], tokens[index]
                )),
            );
        }

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        let summary = build_iteration_summary_document(temp.path(), IterationSummaryOptions::default()).unwrap();

        assert!(!summary.flaky_assertions.is_empty());
        assert_eq!(benchmark.iteration_summary.flaky_assertions, summary.flaky_assertions);

        let benchmark_timing = &benchmark.iteration_summary.timing_outliers;
        let summary_timing = &summary.timing_outliers;
        assert_eq!(benchmark_timing.len(), 1);
        assert_eq!(summary_timing.len(), 1);
        assert_eq!(benchmark_timing[0].eval_case_id, summary_timing[0].eval_case_id);
        assert_eq!(benchmark_timing[0].scenario_id, summary_timing[0].scenario_id);
        assert_eq!(benchmark_timing[0].attempt, summary_timing[0].attempt);
        assert_eq!(benchmark_timing[0].value, summary_timing[0].duration_ms);
        assert_eq!(benchmark_timing[0].median, summary_timing[0].median_ms);
        assert_eq!(benchmark_timing[0].mad, summary_timing[0].mad_ms);

        let benchmark_tokens = &benchmark.iteration_summary.token_outliers;
        let summary_tokens = &summary.token_outliers;
        assert_eq!(benchmark_tokens.len(), 1);
        assert_eq!(summary_tokens.len(), 1);
        assert_eq!(benchmark_tokens[0].eval_case_id, summary_tokens[0].eval_case_id);
        assert_eq!(benchmark_tokens[0].scenario_id, summary_tokens[0].scenario_id);
        assert_eq!(benchmark_tokens[0].attempt, summary_tokens[0].attempt);
        assert_eq!(benchmark_tokens[0].value, summary_tokens[0].total_tokens);
        assert_eq!(benchmark_tokens[0].median, summary_tokens[0].median_tokens);
        assert_eq!(benchmark_tokens[0].mad, summary_tokens[0].mad_tokens);
    }

    /// A bucket of ten runs where one carried a price used to publish that one price as
    /// the bucket's cost, which reads as nine runs that were free.
    #[test]
    fn a_bucket_only_some_of_whose_runs_were_priced_says_what_the_total_covers() {
        let costs = vec![RunCost::Priced { usd: 0.25 }];

        assert_eq!(bucket_cost(&costs, 3), Some(BucketCost::Partial { usd: 0.25, runs: 1 }));
    }

    #[test]
    fn a_bucket_every_run_of_which_was_priced_reports_the_bucket_cost() {
        let costs = vec![RunCost::Priced { usd: 0.25 }, RunCost::Priced { usd: 0.75 }];

        assert_eq!(bucket_cost(&costs, 2), Some(BucketCost::Whole { usd: 1.0 }));
    }

    /// Totalling the rest would read the run its harness refuses to price as a run that
    /// was free, so there is no total to publish here at all.
    #[test]
    fn a_bucket_holding_a_run_no_harness_prices_has_no_total() {
        let costs = vec![
            RunCost::Priced { usd: 0.25 },
            RunCost::Unpriced {
                harness: "codex".to_string(),
            },
        ];

        assert_eq!(
            bucket_cost(&costs, 2),
            Some(BucketCost::Unpriced {
                harness: "codex".to_string()
            })
        );
    }

    #[test]
    fn a_bucket_no_run_of_which_reported_anything_carries_no_cost() {
        assert_eq!(bucket_cost(&[], 3), None);
    }

    /// Only a bucket whose total is its own cost has a number to put under the name an
    /// earlier release read as exactly that.
    #[test]
    fn only_a_whole_bucket_publishes_the_bare_number_earlier_releases_read() {
        let whole = TokenStats {
            cost: Some(BucketCost::Whole { usd: 0.25 }),
            ..TokenStats::default()
        };
        let partial = TokenStats {
            cost: Some(BucketCost::Partial { usd: 0.25, runs: 1 }),
            ..TokenStats::default()
        };

        assert_eq!(serde_json::to_value(&whole).unwrap()["cost_usd"], 0.25);
        assert!(serde_json::to_value(&partial).unwrap().get("cost_usd").is_none());
    }

    /// Subtracting a total that covers one arm from a total that covers part of another
    /// answers no question anybody asked.
    #[test]
    fn a_cost_difference_is_withheld_unless_both_arms_priced_every_run() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "with_skill", "completed", None),
                sample_run("run-003", "without_skill", "completed", None),
            ]),
            None,
        );
        for (run_id, timing) in [
            (
                "run-001",
                r#"{ "duration_ms": 1000, "cost": { "kind": "priced", "usd": 0.25 } }"#,
            ),
            ("run-002", r#"{ "duration_ms": 1000 }"#),
            (
                "run-003",
                r#"{ "duration_ms": 1000, "cost": { "kind": "priced", "usd": 0.10 } }"#,
            ),
        ] {
            write_run_artifacts(temp.path(), run_id, None, Some(timing));
        }

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();

        assert_eq!(
            benchmark.scenarios["with_skill"].completed.tokens.cost,
            Some(BucketCost::Partial { usd: 0.25, runs: 1 })
        );
        assert_eq!(
            benchmark.scenarios["without_skill"].completed.tokens.cost,
            Some(BucketCost::Whole { usd: 0.10 })
        );
        assert_eq!(
            benchmark
                .deltas
                .with_skill_vs_without_skill
                .expect("both arms ran")
                .cost_usd,
            None
        );
    }

    /// A run of a harness that prices nothing is not a run that cost nothing, so its
    /// bucket names the harness instead of publishing what the other runs came to.
    #[test]
    fn a_bucket_of_a_harness_that_prices_nothing_names_it_instead_of_totalling() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([sample_run("run-001", "with_skill", "completed", None)]),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            None,
            Some(r#"{ "duration_ms": 1000, "cost": { "kind": "unpriced", "harness": "codex" } }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();

        assert_eq!(
            benchmark.scenarios["with_skill"].completed.tokens.cost,
            Some(BucketCost::Unpriced {
                harness: "codex".to_string()
            })
        );
    }

    #[test]
    fn benchmark_json_snapshot_matches_fixture_layout() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "without_skill", "completed", None),
                sample_run("run-003", "old_skill", "completed", None),
            ]),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            Some(
                r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": true, "evidence": "ok" },
    { "assertion": "c", "passed": true, "evidence": "ok" }
  ],
  "summary": { "passed": 3, "failed": 0, "total": 3, "pass_rate": 1.0 }
}"#,
            ),
            Some(
                r#"{ "duration_ms": 1000, "total_tokens": 300, "input_tokens": 200, "output_tokens": 100, "cost_usd": 0.01 }"#,
            ),
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            Some(
                r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok" },
    { "assertion": "b", "passed": false, "evidence": "nope" }
  ],
  "summary": { "passed": 1, "failed": 1, "total": 2, "pass_rate": 0.5 }
}"#,
            ),
            Some(r#"{ "duration_ms": 2000, "total_tokens": 500, "input_tokens": 300, "output_tokens": 200 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-003",
            Some(
                r#"{
  "assertion_results": [{ "assertion": "a", "passed": false, "evidence": "nope" }],
  "summary": { "passed": 0, "failed": 1, "total": 1, "pass_rate": 0.0 }
}"#,
            ),
            Some(r#"{ "duration_ms": 1500, "total_tokens": 400 }"#),
        );

        let mut benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        benchmark.generated_at = "2026-05-26T12:00:00Z".to_string();

        let actual = serde_json::to_value(&benchmark).unwrap();
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/benchmark_expected.json")).unwrap();
        assert_eq!(actual, expected);
    }

    fn write_drift_report(report_dir: &Path, iteration: u32, evals_hash: &str, eval_ids: &[&str]) {
        fs::create_dir_all(report_dir).unwrap();
        let eval_cases: Vec<serde_json::Value> = eval_ids.iter().map(|id| serde_json::json!({ "id": id })).collect();
        let report = serde_json::json!({
            "report": {
                "id": format!("report-iter-{iteration}"),
                "generated_at": "2026-05-26T00:00:00Z",
                "iteration": iteration,
                "producer": { "name": "trg", "version": "0.3.0" }
            },
            "suite": {
                "skill_name": "demo",
                "skill_path": "demo",
                "skill_hash": "sha256:abc",
                "evals_path": "demo/evals/evals.json",
                "evals_hash": evals_hash
            },
            "dimensions": {
                "eval_cases": eval_cases,
                "assertions": [],
                "skill_revisions": [],
                "model_configs": [],
                "scenarios": [],
                "grading_strategies": []
            },
            "runs": [],
            "assertion_results": [],
            "summaries": { "by_scenario": [] },
            "comparisons": []
        });
        fs::write(
            report_dir.join("report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn eval_suite_drift_warnings_empty_without_a_previous_report() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let report_dir = skill_root.join("report-iter-2");
        write_drift_report(&report_dir, 2, "sha256:current", &["case-a"]);

        let warnings = collect_eval_suite_drift_warnings(&report_dir, BenchmarkOptions::default()).unwrap();

        assert!(warnings.is_empty());
    }

    #[test]
    fn eval_suite_drift_warnings_detected_from_sibling_previous_report() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let report_dir = skill_root.join("report-iter-2");
        write_drift_report(&previous_dir, 1, "sha256:previous", &["case-a", "case-b"]);
        write_drift_report(&report_dir, 2, "sha256:current", &["case-a", "case-c"]);

        let warnings = collect_eval_suite_drift_warnings(&report_dir, BenchmarkOptions::default()).unwrap();

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].added_eval_ids, vec!["case-c".to_string()]);
        assert_eq!(warnings[0].removed_eval_ids, vec!["case-b".to_string()]);
    }
}
