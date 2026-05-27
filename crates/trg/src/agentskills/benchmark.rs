use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::eval_suite_drift::{
    detect_eval_suite_drift_snapshots, load_report_drift_snapshot, maybe_emit_eval_suite_drift_warning,
    EvalSuiteDriftWarning,
};
use super::evals::{EvalError, Result};
use super::iteration_summary::detect_previous_report_dir;
use super::report::ScenarioKind;

pub const SCHEMA_VERSION: &str = "trg.skills-eval.benchmark.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkDocument {
    pub schema_version: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalScenarioAttemptRow {
    pub eval_case_id: String,
    pub scenario_id: String,
    pub attempt_count: u32,
    pub pass_rate: AttemptPassRateStats,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flaky_assertions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptPassRateStats {
    pub mean: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variance: Option<f64>,
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlakyAssertionRecord {
    pub eval_case_id: String,
    pub scenario_id: String,
    pub assertion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricOutlier {
    pub run_id: String,
    pub eval_case_id: String,
    pub scenario_id: String,
    pub attempt: u32,
    pub value: u64,
    pub median: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioBenchmark {
    pub completed: CompletedBucket,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<RunBucketSummary>,
    pub skipped: SkippedBucket,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletedBucket {
    pub run_count: usize,
    pub assertions: PassFailSummary,
    pub runs: PassFailSummary,
    pub duration_ms: DurationStats,
    pub tokens: TokenStats,
    pub missing_grading: usize,
    pub missing_timing: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunBucketSummary {
    pub run_count: usize,
    pub duration_ms: DurationStats,
    pub tokens: TokenStats,
    pub missing_timing: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedBucket {
    pub run_count: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct PassFailSummary {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    pub pass_rate: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DurationStats {
    pub mean: f64,
    pub p50: u64,
    pub p95: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stddev: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TokenStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ScenarioDeltas {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_skill_vs_without_skill: Option<ScenarioDelta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with_skill_vs_old_skill: Option<ScenarioDelta>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioDelta {
    pub assertion_pass_rate: f64,
    pub run_pass_rate: f64,
    pub duration_ms_mean: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
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
    #[serde(default, alias = "text")]
    assertion: String,
    #[serde(default)]
    passed: bool,
    #[serde(default)]
    evidence: String,
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
    cost_usd: Option<f64>,
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
    let by_eval_scenario = build_by_eval_scenario(report_dir, &report.runs, failed_runs);
    let iteration_summary = build_iteration_summary(report_dir, &report.runs, &by_eval_scenario, failed_runs);
    let warnings = collect_eval_suite_drift_warnings(
        report_dir,
        BenchmarkOptions {
            failed_runs,
            allow_eval_suite_drift,
            previous_report_dir,
        },
    )?;

    Ok(BenchmarkDocument {
        schema_version: SCHEMA_VERSION.to_string(),
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
    let previous_dir = options
        .previous_report_dir
        .clone()
        .or_else(|| detect_previous_report_dir(report_dir, current.iteration));

    let Some(previous_dir) = previous_dir else {
        return Ok(Vec::new());
    };

    let previous = load_report_drift_snapshot(&previous_dir)?;
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
    value
        .pointer("/report/iteration")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
        .ok_or_else(|| {
            EvalError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "report.json missing report.iteration",
            ))
        })
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
        grading,
        timing,
        missing_grading: grading_path.is_none(),
        missing_timing: timing_path.is_none(),
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
            push_if_some_f64(&mut costs, timing.cost_usd);
        }
    }

    run_failed += zero_failed_count;

    CompletedBucket {
        run_count: samples.len() + zero_failed_count,
        assertions: pass_fail_summary(assertion_passed, assertion_failed),
        runs: pass_fail_summary(run_passed, run_failed),
        duration_ms: duration_stats(&durations),
        tokens: token_stats(&token_totals, &token_inputs, &token_outputs, &costs),
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
            push_if_some_f64(&mut costs, timing.cost_usd);
        }
    }

    RunBucketSummary {
        run_count: samples.len(),
        duration_ms: duration_stats(&durations),
        tokens: token_stats(&token_totals, &token_inputs, &token_outputs, &costs),
        missing_timing,
    }
}

fn grading_summary(grading: &GradingFileInput) -> PassFailSummary {
    if let Some(summary) = &grading.summary {
        if summary.total > 0 {
            return pass_fail_summary(summary.passed, summary.failed);
        }
    }

    let passed = grading.assertion_results.iter().filter(|result| result.passed).count();
    let failed = grading.assertion_results.len().saturating_sub(passed);
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
    };
    if sorted.len() >= 3 {
        stats.stddev = Some(stddev(&sorted, mean));
    }
    stats
}

pub fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
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

fn token_stats(totals: &[u64], inputs: &[u64], outputs: &[u64], costs: &[f64]) -> TokenStats {
    TokenStats {
        total: sum_optional(totals),
        input: sum_optional(inputs),
        output: sum_optional(outputs),
        cost_usd: sum_optional_f64(costs),
    }
}

fn sum_optional(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum())
    }
}

fn sum_optional_f64(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum())
    }
}

fn push_if_some(values: &mut Vec<u64>, value: Option<u64>) {
    if let Some(value) = value {
        values.push(value);
    }
}

fn push_if_some_f64(values: &mut Vec<f64>, value: Option<f64>) {
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
        run_pass_rate: left.completed.runs.pass_rate - right.completed.runs.pass_rate,
        duration_ms_mean: left.completed.duration_ms.mean - right.completed.duration_ms.mean,
        tokens_total: diff_optional(left.completed.tokens.total, right.completed.tokens.total),
        cost_usd: diff_optional_f64(left.completed.tokens.cost_usd, right.completed.tokens.cost_usd),
    })
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
) -> Vec<EvalScenarioAttemptRow> {
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

    let mut rows = Vec::new();
    for ((eval_case_id, scenario_id), group_runs) in groups {
        let mut pass_rates = Vec::new();
        let mut samples: Vec<(u32, GradingFileInput)> = Vec::new();
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
                samples.push((run.attempt, grading));
            }
        }

        let attempt_count = group_runs.len() as u32;
        let flaky_assertions = detect_flaky_assertions(&samples);
        rows.push(EvalScenarioAttemptRow {
            eval_case_id,
            scenario_id: scenario_id.as_str().to_string(),
            attempt_count,
            pass_rate: attempt_pass_rate_stats(&pass_rates),
            flaky_assertions,
        });
    }

    rows
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
    pass_rates
        .iter()
        .map(|rate| {
            let delta = *rate - mean;
            delta * delta
        })
        .sum::<f64>()
        / pass_rates.len() as f64
}

fn detect_flaky_assertions(samples: &[(u32, GradingFileInput)]) -> Vec<String> {
    let mut by_assertion: BTreeMap<String, BTreeMap<u32, bool>> = BTreeMap::new();
    for (attempt, grading) in samples {
        for result in &grading.assertion_results {
            let key = normalize_assertion_key(&result.assertion);
            if key.is_empty() {
                continue;
            }
            by_assertion.entry(key).or_default().insert(*attempt, result.passed);
        }
    }

    by_assertion
        .into_iter()
        .filter_map(|(assertion, attempts)| {
            let outcomes: HashSet<bool> = attempts.values().copied().collect();
            if outcomes.len() > 1 {
                Some(assertion)
            } else {
                None
            }
        })
        .collect()
}

fn normalize_assertion_key(assertion: &str) -> String {
    assertion.trim().to_string()
}

fn build_iteration_summary(
    report_dir: &Path,
    runs: &[RunForBenchmark],
    by_eval_scenario: &[EvalScenarioAttemptRow],
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
            if assertion_key.is_empty() {
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

    let flaky_assertions: Vec<FlakyAssertionRecord> = by_eval_scenario
        .iter()
        .flat_map(|row| {
            row.flaky_assertions.iter().map(|assertion| FlakyAssertionRecord {
                eval_case_id: row.eval_case_id.clone(),
                scenario_id: row.scenario_id.clone(),
                assertion: assertion.clone(),
            })
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

fn detect_metric_outliers(
    report_dir: &Path,
    runs: &[RunForBenchmark],
    mode: FailedRunsMode,
    kind: MetricKind,
) -> Vec<MetricOutlier> {
    let mut values: Vec<(String, String, String, u32, u64)> = Vec::new();
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
            values.push((
                run.id.clone(),
                run.eval_case_id.clone(),
                run.scenario_id.as_str().to_string(),
                run.attempt,
                value,
            ));
        }
    }

    if values.len() < 2 {
        return Vec::new();
    }

    let mut sorted: Vec<u64> = values.iter().map(|(_, _, _, _, v)| *v).collect();
    sorted.sort_unstable();
    let median = percentile(&sorted, 0.50);
    if median == 0 {
        return Vec::new();
    }

    let threshold = median.saturating_mul(2);
    values
        .into_iter()
        .filter(|(_, _, _, _, value)| *value >= threshold)
        .map(|(run_id, eval_case_id, scenario_id, attempt, value)| MetricOutlier {
            run_id,
            eval_case_id,
            scenario_id,
            attempt,
            value,
            median,
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
            "schema_version": "trg.skills-eval.report.v1",
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
                "graders": []
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
    fn duration_stats_omit_stddev_for_small_samples() {
        let two = duration_stats(&[100, 200]);
        assert!(two.stddev.is_none());

        let three = duration_stats(&[100, 200, 300]);
        assert!(three.stddev.is_some());
    }

    #[test]
    fn percentile_and_stddev_helpers() {
        assert_eq!(percentile(&[10, 20, 30, 40], 0.50), 30);
        assert_eq!(percentile(&[10, 20, 30, 40], 0.95), 40);
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
    { "text": "a", "passed": true, "evidence": "ok" },
    { "text": "b", "passed": false, "evidence": "missing" }
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
    }

    #[test]
    fn timing_and_token_outliers_use_double_median_threshold() {
        let temp = tempfile::tempdir().unwrap();
        write_report(
            temp.path(),
            serde_json::json!([
                sample_run("run-001", "with_skill", "completed", None),
                sample_run("run-002", "with_skill", "completed", None),
                sample_run("run-003", "with_skill", "completed", None),
            ]),
            None,
        );
        write_run_artifacts(
            temp.path(),
            "run-001",
            None,
            Some(r#"{ "duration_ms": 1000, "total_tokens": 100 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-002",
            None,
            Some(r#"{ "duration_ms": 1200, "total_tokens": 120 }"#),
        );
        write_run_artifacts(
            temp.path(),
            "run-003",
            None,
            Some(r#"{ "duration_ms": 3000, "total_tokens": 400 }"#),
        );

        let benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
        assert_eq!(benchmark.iteration_summary.timing_outliers.len(), 1);
        assert_eq!(benchmark.iteration_summary.timing_outliers[0].run_id, "run-003");
        assert_eq!(benchmark.iteration_summary.timing_outliers[0].median, 1200);
        assert_eq!(benchmark.iteration_summary.token_outliers.len(), 1);
        assert_eq!(benchmark.iteration_summary.token_outliers[0].value, 400);
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

        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/benchmark.schema.json")).unwrap();
        assert_eq!(
            actual.get("schema_version").and_then(|value| value.as_str()),
            schema
                .pointer("/properties/schema_version/const")
                .and_then(|value| value.as_str())
        );
    }
}
