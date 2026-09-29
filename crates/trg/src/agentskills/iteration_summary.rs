use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::benchmark::FailedRunsMode;
use super::dispersion::{self, FlakinessLedger, FlakyAssertionRecord, MetricSample};
use super::eval_suite_drift;
use super::evals::{EvalError, EvalSplit, Result};
use super::headroom::{describe_headroom_warning, evaluate_headroom, HeadroomThreshold, HeadroomWarning};
use super::layout;
use super::proportion::{self, Interval, Proportion};
use super::report::ScenarioKind;

pub const OUTPUT_FILE_NAME: &str = "iteration-summary.json";

#[derive(Debug, Clone, Default)]
pub struct IterationSummaryOptions {
    pub failed_runs: FailedRunsMode,
    pub previous_report_dir: Option<PathBuf>,
    pub headroom_threshold: HeadroomThreshold,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IterationSummaryDocument {
    pub report_id: String,
    pub iteration: u32,
    pub generated_at: String,
    pub failed_runs_mode: FailedRunsMode,
    pub always_pass: Vec<AssertionStabilityRecord>,
    pub always_fail: Vec<AssertionStabilityRecord>,
    pub helped_by_skill: Vec<HelpedBySkillRecord>,
    pub flaky_assertions: Vec<FlakyAssertionRecord>,
    pub timing_outliers: Vec<TimingOutlierRecord>,
    pub token_outliers: Vec<TokenOutlierRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_iteration: Option<CrossIterationSection>,
    /// The same assertion stability and with-skill-vs-without-skill records, narrowed to
    /// each split, so a train gain and a flat test result sit side by side with the
    /// overall numbers above.
    pub by_split: BTreeMap<EvalSplit, SplitSummary>,
    /// Set when the overall `with_skill` assertion pass rate has cleared its headroom
    /// threshold, so a change to the skill has no room left to show up in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headroom: Option<HeadroomWarning>,
    /// Whether this iteration's `with_skill` arm beat the previous iteration's on each
    /// split, and what that says about keeping or reverting the change. Absent when there
    /// is no previous iteration to compare against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_or_revert: Option<KeepOrRevertSection>,
}

/// Whether the eval suite is the same one the previous iteration ran, which the
/// keep-or-revert verdict below depends on: a pass-rate change against a suite that
/// gained or lost cases is not a change in the skill.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SuiteComparison {
    SameSuite,
    DifferentSuites {
        current_suite_hash: String,
        previous_suite_hash: String,
    },
}

/// The `with_skill` assertion and run pass-rate change between this iteration and the
/// previous one, and the 95% intervals around each. The assertion pass rate drives the
/// verdict this rides alongside: it pools every scored assertion rather than collapsing a
/// case with three assertions and one with one assertion to the same single vote a run
/// pass rate would give them, so it moves on the same evidence the reviewer's bundle does.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct SplitDelta {
    pub assertion_pass_rate_delta: f64,
    pub assertion_pass_rate_delta_interval: Interval,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_pass_rate_delta: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_pass_rate_delta_interval: Option<Interval>,
}

/// The verdict for one split's `with_skill` delta against the previous iteration, derived
/// from whether `assertion_pass_rate_delta_interval` excludes zero.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SplitVerdict {
    /// The interval sits entirely above zero: this split's `with_skill` arm did better.
    Improved(SplitDelta),
    /// The interval sits entirely below zero: this split's `with_skill` arm did worse.
    Regressed(SplitDelta),
    /// The interval straddles zero: the change has not beaten this split's own noise.
    Indistinguishable(SplitDelta),
    /// Neither iteration scored a `with_skill` run on this split, so there is nothing to
    /// compare. Never treated as `Indistinguishable`: silence about a split is not a
    /// finding about it.
    NoRuns,
}

/// Why `recommendation` cannot be read as ordinary silence on the test split: the split it
/// depends on most had already used up its room before this iteration ran, so no change
/// could have shown as `improved` there regardless of what the change did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SaturationCap {
    TestSplitSaturated,
}

/// What the two verdicts above say to do with the change under review.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    /// The held-out split improved: the change generalized past the cases it was tuned on.
    Keep,
    /// The held-out split regressed: the change made the skill worse where it counts.
    Revert,
    /// Train improved while test did not, which is what a skill that learned the suite
    /// instead of the task looks like.
    SuspectedOverfitting,
    /// Neither split moved far enough to say either way, or the suites being compared are
    /// not the same suite.
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct KeepOrRevertSection {
    pub suites: SuiteComparison,
    /// Empty when `suites` is `different_suites`, since a verdict across two different
    /// suites is not a verdict about the change.
    pub by_split: BTreeMap<EvalSplit, SplitVerdict>,
    pub recommendation: Recommendation,
    /// Set when the test split's `with_skill` arm has no headroom left, so `recommendation`
    /// is not misread as an ordinary `inconclusive` or a clean `keep`: the split it depends
    /// on most could not have shown `improved` here no matter what the change did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capped_by_saturation: Option<SaturationCap>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SplitSummary {
    pub always_pass: Vec<AssertionStabilityRecord>,
    pub always_fail: Vec<AssertionStabilityRecord>,
    pub helped_by_skill: Vec<HelpedBySkillRecord>,
    /// Set when this split's `with_skill` assertion pass rate has cleared its headroom
    /// threshold, which matters most on the test split: the keep-or-revert verdict can
    /// never read `improved` there once it has.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headroom: Option<HeadroomWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema)]
pub struct AssertionKey {
    pub eval_id: String,
    pub assertion_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AssertionStabilityRecord {
    pub eval_case_id: String,
    pub assertion: String,
    pub attempts_observed: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_iteration_delta: Option<CrossIterationDelta>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CrossIterationDelta {
    New,
    Stable,
    Lost,
}

/// `delta` is a subtraction between two pass rates, each already a ratio over its own
/// arm's attempts. Nothing about `with_skill_pass_rate` or `without_skill_pass_rate`
/// says whether that ratio was drawn from three attempts or thirty, so the attempt
/// counts ride alongside it rather than being left for a reader to assume.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HelpedBySkillRecord {
    pub eval_case_id: String,
    pub assertion: String,
    pub with_skill_pass_rate: f64,
    pub without_skill_pass_rate: f64,
    pub delta: f64,
    pub with_skill_attempts: u32,
    pub without_skill_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TimingOutlierRecord {
    pub eval_case_id: String,
    pub scenario_id: String,
    pub attempt: u32,
    pub duration_ms: u64,
    pub median_ms: f64,
    pub mad_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TokenOutlierRecord {
    pub eval_case_id: String,
    pub scenario_id: String,
    pub attempt: u32,
    pub total_tokens: u64,
    pub median_tokens: f64,
    pub mad_tokens: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CrossIterationSection {
    pub previous_report_id: String,
    pub previous_iteration: u32,
    pub newly_always_pass: Vec<AssertionStabilityRecord>,
    pub no_longer_always_pass: Vec<AssertionStabilityRecord>,
    pub newly_always_fail: Vec<AssertionStabilityRecord>,
    pub no_longer_always_fail: Vec<AssertionStabilityRecord>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReportForSummary {
    report: ReportMeta,
    #[serde(default)]
    suite: SuiteForSummary,
    runs: Vec<RunForSummary>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReportMeta {
    id: String,
    iteration: u32,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SuiteForSummary {
    #[serde(default)]
    evals_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RunForSummary {
    eval_case_id: String,
    #[serde(default)]
    split: EvalSplit,
    scenario_id: ScenarioKind,
    attempt: u32,
    status: String,
    paths: RunPathsForSummary,
}

#[derive(Debug, Clone, Deserialize)]
struct RunPathsForSummary {
    workspace: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct GradingFileInput {
    #[serde(default)]
    assertion_results: Vec<AssertionResultInput>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AssertionResultInput {
    #[serde(default)]
    assertion: String,
    #[serde(default)]
    passed: bool,
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
struct TimingFileInput {
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct RunSample {
    grading: Option<GradingFileInput>,
    timing: Option<TimingFileInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct AssertionObservationKey {
    eval_id: String,
    assertion_text: String,
}

impl AssertionObservationKey {
    fn new(eval_id: &str, assertion_text: &str) -> Self {
        Self {
            eval_id: eval_id.to_string(),
            assertion_text: normalize_assertion_key(assertion_text),
        }
    }
}

#[derive(Debug, Clone)]
struct SummaryRunOrigin {
    eval_case_id: String,
    attempt: u32,
}

pub fn build_iteration_summary_document(
    report_dir: &Path,
    options: IterationSummaryOptions,
) -> Result<IterationSummaryDocument> {
    let report = load_report(report_dir)?;
    let current = analyze_report(report_dir, &report, options.failed_runs);

    let previous = resolve_previous_report_for_summary(
        report_dir,
        report.report.iteration,
        options.previous_report_dir.as_deref(),
    )?;

    let cross_iteration = previous.clone().and_then(|(previous_dir, previous_report)| {
        build_cross_iteration_section(&previous_dir, previous_report, &current, options.failed_runs)
    });

    let keep_or_revert = previous.map(|(previous_dir, previous_report)| {
        build_keep_or_revert_section(
            report_dir,
            &report,
            &previous_dir,
            &previous_report,
            options.failed_runs,
            options.headroom_threshold,
        )
    });

    let (always_pass, always_fail) =
        apply_cross_iteration_deltas(&current.always_pass, &current.always_fail, cross_iteration.as_ref());

    let headroom = compute_headroom(
        report_dir,
        &report,
        None,
        options.failed_runs,
        options.headroom_threshold,
    );
    let by_split = split_summaries(report_dir, &report, options.failed_runs, options.headroom_threshold);

    Ok(IterationSummaryDocument {
        report_id: report.report.id,
        iteration: report.report.iteration,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        failed_runs_mode: options.failed_runs,
        always_pass,
        always_fail,
        helped_by_skill: current.helped_by_skill,
        flaky_assertions: current.flaky_assertions,
        timing_outliers: current.timing_outliers,
        token_outliers: current.token_outliers,
        cross_iteration,
        by_split,
        headroom,
        keep_or_revert,
    })
}

/// The keep-or-revert verdict: whether this iteration's `with_skill` arm beat the previous
/// iteration's on the held-out split, the same question asked of the train split for
/// comparison, and the recommendation the two answers add up to.
fn build_keep_or_revert_section(
    report_dir: &Path,
    report: &ReportForSummary,
    previous_dir: &Path,
    previous_report: &ReportForSummary,
    mode: FailedRunsMode,
    threshold: HeadroomThreshold,
) -> KeepOrRevertSection {
    let suites = suite_comparison(report, previous_report);

    let (by_split, capped_by_saturation) = match &suites {
        SuiteComparison::DifferentSuites { .. } => (BTreeMap::new(), None),
        SuiteComparison::SameSuite => {
            let by_split: BTreeMap<EvalSplit, SplitVerdict> = EvalSplit::ALL
                .into_iter()
                .map(|split| {
                    let verdict = compute_split_verdict(report_dir, report, previous_dir, previous_report, split, mode);
                    (split, verdict)
                })
                .collect();
            let capped_by_saturation = compute_headroom(report_dir, report, Some(EvalSplit::Test), mode, threshold)
                .map(|_| SaturationCap::TestSplitSaturated);
            (by_split, capped_by_saturation)
        }
    };

    let recommendation = derive_recommendation(&by_split);

    KeepOrRevertSection {
        suites,
        by_split,
        recommendation,
        capped_by_saturation,
    }
}

fn suite_comparison(current: &ReportForSummary, previous: &ReportForSummary) -> SuiteComparison {
    if current.suite.evals_hash == previous.suite.evals_hash {
        SuiteComparison::SameSuite
    } else {
        SuiteComparison::DifferentSuites {
            current_suite_hash: current.suite.evals_hash.clone(),
            previous_suite_hash: previous.suite.evals_hash.clone(),
        }
    }
}

/// `keep` when the test split improved, `revert` when it regressed (even if train also
/// improved, since a regression on held-out cases is reason enough on its own), and
/// `suspected_overfitting` when train improved but a *measured* test split did not clear
/// the bar to say either way. A test split that never ran (`NoRuns`) is silence, not a
/// measurement: it must not be read as the flat test split that makes a train-only gain
/// suspicious, or a suite with no held-out set would report overfitting on every train
/// improvement and fail `--fail-on overfitting` for a reason it has no way to fix.
/// Everything else is `inconclusive`.
fn derive_recommendation(by_split: &BTreeMap<EvalSplit, SplitVerdict>) -> Recommendation {
    let test = by_split.get(&EvalSplit::Test);
    let train = by_split.get(&EvalSplit::Train);

    if matches!(test, Some(SplitVerdict::Improved(_))) {
        return Recommendation::Keep;
    }
    if matches!(test, Some(SplitVerdict::Regressed(_))) {
        return Recommendation::Revert;
    }
    if matches!(test, Some(SplitVerdict::Indistinguishable(_))) && matches!(train, Some(SplitVerdict::Improved(_))) {
        return Recommendation::SuspectedOverfitting;
    }
    Recommendation::Inconclusive
}

#[derive(Debug, Default)]
struct ScenarioTally {
    assertion_passed: u32,
    assertion_failed: u32,
    run_passed: u32,
    run_failed: u32,
}

impl ScenarioTally {
    fn assertion_proportion(&self) -> Option<Proportion> {
        Proportion::observed(self.assertion_passed as usize, self.assertion_failed as usize)
    }

    fn run_proportion(&self) -> Option<Proportion> {
        Proportion::observed(self.run_passed as usize, self.run_failed as usize)
    }
}

/// Pool every scored assertion (and every scored run) the `with_skill` scenario drew on
/// one split, the same reduction `analyze_report` applies per case and assertion, but
/// summed across all of them so a split with three assertions in one case and one in
/// another still reports a single pass rate to compare across iterations.
fn tally_with_skill(
    report_dir: &Path,
    report: &ReportForSummary,
    split: EvalSplit,
    mode: FailedRunsMode,
) -> ScenarioTally {
    let mut tally = ScenarioTally::default();

    for run in &report.runs {
        if run.split != split || run.scenario_id != ScenarioKind::WithSkill {
            continue;
        }
        if !matches!(classify_run(run, mode), RunDisposition::Completed) {
            continue;
        }

        let sample = load_run_sample(report_dir, &run.paths.workspace);
        let Some(grading) = &sample.grading else {
            continue;
        };

        let scored: Vec<&AssertionResultInput> = grading
            .assertion_results
            .iter()
            .filter(|result| result.is_scored())
            .collect();
        if scored.is_empty() {
            continue;
        }

        let passed = scored.iter().filter(|result| result.passed).count() as u32;
        let failed = scored.len() as u32 - passed;
        tally.assertion_passed += passed;
        tally.assertion_failed += failed;
        if failed == 0 {
            tally.run_passed += 1;
        } else {
            tally.run_failed += 1;
        }
    }

    tally
}

fn compute_split_verdict(
    report_dir: &Path,
    report: &ReportForSummary,
    previous_dir: &Path,
    previous_report: &ReportForSummary,
    split: EvalSplit,
    mode: FailedRunsMode,
) -> SplitVerdict {
    let current_tally = tally_with_skill(report_dir, report, split, mode);
    let previous_tally = tally_with_skill(previous_dir, previous_report, split, mode);

    let (Some(current_assertions), Some(previous_assertions)) = (
        current_tally.assertion_proportion(),
        previous_tally.assertion_proportion(),
    ) else {
        return SplitVerdict::NoRuns;
    };

    let assertion_pass_rate_delta_interval = proportion::difference(current_assertions, previous_assertions);
    let assertion_pass_rate_delta = current_assertions.rate() - previous_assertions.rate();

    let (run_pass_rate_delta, run_pass_rate_delta_interval) =
        match (current_tally.run_proportion(), previous_tally.run_proportion()) {
            (Some(current_runs), Some(previous_runs)) => (
                Some(current_runs.rate() - previous_runs.rate()),
                Some(proportion::difference(current_runs, previous_runs)),
            ),
            _ => (None, None),
        };

    let delta = SplitDelta {
        assertion_pass_rate_delta,
        assertion_pass_rate_delta_interval,
        run_pass_rate_delta,
        run_pass_rate_delta_interval,
    };

    if assertion_pass_rate_delta_interval.low() > 0.0 {
        SplitVerdict::Improved(delta)
    } else if assertion_pass_rate_delta_interval.high() < 0.0 {
        SplitVerdict::Regressed(delta)
    } else {
        SplitVerdict::Indistinguishable(delta)
    }
}

/// The report's own analysis, reused once per split rather than reimplemented, so a
/// split's assertion stability and with-skill deltas are read the same way the overall
/// numbers are.
fn split_summaries(
    report_dir: &Path,
    report: &ReportForSummary,
    mode: FailedRunsMode,
    threshold: HeadroomThreshold,
) -> BTreeMap<EvalSplit, SplitSummary> {
    EvalSplit::ALL
        .into_iter()
        .map(|split| {
            let narrowed = ReportForSummary {
                report: report.report.clone(),
                suite: report.suite.clone(),
                runs: report.runs.iter().filter(|run| run.split == split).cloned().collect(),
            };
            let analysis = analyze_report(report_dir, &narrowed, mode);
            let headroom = compute_headroom(report_dir, report, Some(split), mode, threshold);
            (
                split,
                SplitSummary {
                    always_pass: analysis.always_pass,
                    always_fail: analysis.always_fail,
                    helped_by_skill: analysis.helped_by_skill,
                    headroom,
                },
            )
        })
        .collect()
}

/// Whether the `with_skill` arm has any headroom left, and which cases individually
/// passed every scored assertion drawn of them.
///
/// Builds its own per-case assertion tally rather than reusing [`analyze_report`]'s,
/// because a case is saturated only once every assertion it was ever asked has passed
/// across every attempt, which [`analyze_report`]'s per-assertion `always_pass` list does
/// not answer by itself: a case with two assertions where one always passes and the other
/// always fails is not saturated, and nothing distinguishes it from a case where neither
/// does without grouping by case first.
fn compute_headroom(
    report_dir: &Path,
    report: &ReportForSummary,
    split: Option<EvalSplit>,
    mode: FailedRunsMode,
    threshold: HeadroomThreshold,
) -> Option<HeadroomWarning> {
    let mut case_totals: BTreeMap<String, (u32, u32)> = BTreeMap::new();

    for run in &report.runs {
        if run.scenario_id != ScenarioKind::WithSkill {
            continue;
        }
        if let Some(split) = split {
            if run.split != split {
                continue;
            }
        }
        if !matches!(classify_run(run, mode), RunDisposition::Completed) {
            continue;
        }

        let sample = load_run_sample(report_dir, &run.paths.workspace);
        let Some(grading) = &sample.grading else {
            continue;
        };

        let scored: Vec<&AssertionResultInput> = grading
            .assertion_results
            .iter()
            .filter(|result| result.is_scored())
            .collect();
        if scored.is_empty() {
            continue;
        }

        let passed = scored.iter().filter(|result| result.passed).count() as u32;
        let failed = scored.len() as u32 - passed;
        let entry = case_totals.entry(run.eval_case_id.clone()).or_insert((0, 0));
        entry.0 += passed;
        entry.1 += failed;
    }

    let assertions = Proportion::observed(
        case_totals.values().map(|(passed, _)| *passed as usize).sum(),
        case_totals.values().map(|(_, failed)| *failed as usize).sum(),
    );
    let saturated_case_ids = case_totals
        .iter()
        .filter(|(_, (passed, failed))| *passed > 0 && *failed == 0)
        .map(|(eval_case_id, _)| eval_case_id.clone())
        .collect();

    evaluate_headroom(threshold, assertions, saturated_case_ids)
}

pub fn write_iteration_summary(report_dir: &Path, document: &IterationSummaryDocument) -> Result<PathBuf> {
    let output_path = report_dir.join(OUTPUT_FILE_NAME);
    let json = serde_json::to_string_pretty(document)?;
    std::fs::write(&output_path, &json)?;

    let iteration_dir = report_dir.join(layout::iteration_dir_name(document.iteration));
    std::fs::create_dir_all(&iteration_dir)?;
    std::fs::write(iteration_dir.join(OUTPUT_FILE_NAME), json)?;

    Ok(output_path)
}

pub fn print_human_summary(document: &IterationSummaryDocument) {
    if let Some(headroom) = &document.headroom {
        println!("{}", describe_headroom_warning("overall", headroom));
    }
    for split in EvalSplit::ALL {
        if let Some(headroom) = document
            .by_split
            .get(&split)
            .and_then(|summary| summary.headroom.as_ref())
        {
            println!("{}", describe_headroom_warning(split.as_str(), headroom));
        }
    }

    print_section(
        "Always pass",
        document.always_pass.iter().map(|record| {
            format!(
                "{} | {} ({} attempts{})",
                record.eval_case_id,
                record.assertion,
                record.attempts_observed,
                delta_suffix(record.cross_iteration_delta)
            )
        }),
    );
    print_section(
        "Always fail",
        document.always_fail.iter().map(|record| {
            format!(
                "{} | {} ({} attempts{})",
                record.eval_case_id,
                record.assertion,
                record.attempts_observed,
                delta_suffix(record.cross_iteration_delta)
            )
        }),
    );
    print_section(
        "Helped by skill",
        document.helped_by_skill.iter().map(|record| {
            format!(
                "{} | {} (with {:.0}% [{} attempts] vs without {:.0}% [{} attempts], delta {:+.0}%)",
                record.eval_case_id,
                record.assertion,
                record.with_skill_pass_rate * 100.0,
                record.with_skill_attempts,
                record.without_skill_pass_rate * 100.0,
                record.without_skill_attempts,
                record.delta * 100.0
            )
        }),
    );
    print_section(
        "Flaky across attempts",
        document.flaky_assertions.iter().map(|record| {
            format!(
                "{} | {} | {} ({}/{} passes, {:.0}% flaky)",
                record.eval_case_id,
                record.scenario_id,
                record.assertion,
                record.pass_count,
                record.attempts,
                record.flakiness_ratio.percent()
            )
        }),
    );
    print_section(
        "Timing outliers",
        document.timing_outliers.iter().map(|record| {
            format!(
                "{} | {} attempt {} | {} ms (median {:.0}, MAD {:.0})",
                record.eval_case_id,
                record.scenario_id,
                record.attempt,
                record.duration_ms,
                record.median_ms,
                record.mad_ms
            )
        }),
    );
    print_section(
        "Token outliers",
        document.token_outliers.iter().map(|record| {
            format!(
                "{} | {} attempt {} | {} tokens (median {:.0}, MAD {:.0})",
                record.eval_case_id,
                record.scenario_id,
                record.attempt,
                record.total_tokens,
                record.median_tokens,
                record.mad_tokens
            )
        }),
    );

    if let Some(cross) = &document.cross_iteration {
        println!();
        println!(
            "Cross-iteration vs {} (iteration {}):",
            cross.previous_report_id, cross.previous_iteration
        );
        print_section(
            "Newly always pass",
            cross
                .newly_always_pass
                .iter()
                .map(|record| format!("{} | {}", record.eval_case_id, record.assertion)),
        );
        print_section(
            "No longer always pass",
            cross
                .no_longer_always_pass
                .iter()
                .map(|record| format!("{} | {}", record.eval_case_id, record.assertion)),
        );
        print_section(
            "Newly always fail",
            cross
                .newly_always_fail
                .iter()
                .map(|record| format!("{} | {}", record.eval_case_id, record.assertion)),
        );
        print_section(
            "No longer always fail",
            cross
                .no_longer_always_fail
                .iter()
                .map(|record| format!("{} | {}", record.eval_case_id, record.assertion)),
        );
    }

    if let Some(keep_or_revert) = &document.keep_or_revert {
        println!();
        println!(
            "Keep or revert: {}",
            recommendation_label(keep_or_revert.recommendation)
        );
        if let Some(cap) = keep_or_revert.capped_by_saturation {
            println!("  {}", saturation_cap_line(cap));
        }
        match &keep_or_revert.suites {
            SuiteComparison::DifferentSuites {
                current_suite_hash,
                previous_suite_hash,
            } => {
                println!(
                    "  eval suite changed since the previous iteration (previous {previous_suite_hash}, current {current_suite_hash}); no split verdict was computed"
                );
            }
            SuiteComparison::SameSuite => {
                for split in EvalSplit::ALL {
                    let Some(verdict) = keep_or_revert.by_split.get(&split) else {
                        continue;
                    };
                    println!("  {} split: {}", split.as_str(), split_verdict_line(verdict));
                }
            }
        }
    }
}

pub(crate) fn recommendation_label(recommendation: Recommendation) -> &'static str {
    match recommendation {
        Recommendation::Keep => "keep",
        Recommendation::Revert => "revert",
        Recommendation::SuspectedOverfitting => "suspected overfitting",
        Recommendation::Inconclusive => "inconclusive",
    }
}

fn saturation_cap_line(cap: SaturationCap) -> &'static str {
    match cap {
        SaturationCap::TestSplitSaturated => {
            "capped by saturation: the test split's with_skill arm has no headroom left, \
             so this recommendation cannot read as improved regardless of what changed"
        }
    }
}

fn split_verdict_line(verdict: &SplitVerdict) -> String {
    match verdict {
        SplitVerdict::Improved(delta) => format!("improved ({})", format_split_delta(delta)),
        SplitVerdict::Regressed(delta) => format!("regressed ({})", format_split_delta(delta)),
        SplitVerdict::Indistinguishable(delta) => format!("indistinguishable ({})", format_split_delta(delta)),
        SplitVerdict::NoRuns => "no with_skill runs on this split in one or both iterations".to_string(),
    }
}

fn format_split_delta(delta: &SplitDelta) -> String {
    format!(
        "assertion pass rate {:+.1}pp, 95% CI [{:+.1}pp, {:+.1}pp]",
        delta.assertion_pass_rate_delta * 100.0,
        delta.assertion_pass_rate_delta_interval.low() * 100.0,
        delta.assertion_pass_rate_delta_interval.high() * 100.0
    )
}

fn delta_suffix(delta: Option<CrossIterationDelta>) -> String {
    match delta {
        Some(CrossIterationDelta::New) => " [new]".to_string(),
        Some(CrossIterationDelta::Stable) => " [stable]".to_string(),
        Some(CrossIterationDelta::Lost) => " [lost]".to_string(),
        None => String::new(),
    }
}

fn print_section<I>(title: &str, rows: I)
where
    I: IntoIterator<Item = String>,
{
    let rows: Vec<_> = rows.into_iter().collect();
    println!();
    println!("{title} ({}):", rows.len());
    if rows.is_empty() {
        println!("  (none)");
        return;
    }
    for row in rows {
        println!("  {row}");
    }
}

#[derive(Debug, Default)]
struct AnalysisResult {
    always_pass: Vec<AssertionStabilityRecord>,
    always_fail: Vec<AssertionStabilityRecord>,
    helped_by_skill: Vec<HelpedBySkillRecord>,
    flaky_assertions: Vec<FlakyAssertionRecord>,
    timing_outliers: Vec<TimingOutlierRecord>,
    token_outliers: Vec<TokenOutlierRecord>,
    /// Set when a completed run's grading artifact could not be loaded, whether because it
    /// was never written or because it vanished before this read. Distinguishes "we could
    /// not see this iteration's scoring" from "this iteration scored nothing", since only
    /// the former should keep a caller from comparing against it.
    grading_unavailable: bool,
}

fn analyze_report(report_dir: &Path, report: &ReportForSummary, mode: FailedRunsMode) -> AnalysisResult {
    let mut assertion_outcomes: BTreeMap<AssertionObservationKey, (u32, u32)> = BTreeMap::new();
    let mut scenario_assertion_rates: BTreeMap<(String, ScenarioKind, String), (u32, u32)> = BTreeMap::new();
    let mut flakiness = FlakinessLedger::default();
    let mut durations: Vec<MetricSample<SummaryRunOrigin>> = Vec::new();
    let mut tokens: Vec<MetricSample<SummaryRunOrigin>> = Vec::new();
    let mut grading_unavailable = false;

    for run in &report.runs {
        if !matches!(classify_run(run, mode), RunDisposition::Completed) {
            continue;
        }

        let sample = load_run_sample(report_dir, &run.paths.workspace);

        match &sample.grading {
            Some(grading) => {
                for result in &grading.assertion_results {
                    let assertion_text = normalize_assertion_key(&result.assertion);
                    if assertion_text.is_empty() || !result.is_scored() {
                        continue;
                    }

                    let key = AssertionObservationKey::new(&run.eval_case_id, &assertion_text);
                    let entry = assertion_outcomes.entry(key.clone()).or_insert((0, 0));
                    if result.passed {
                        entry.0 += 1;
                    } else {
                        entry.1 += 1;
                    }

                    let scenario_entry = scenario_assertion_rates
                        .entry((run.eval_case_id.clone(), run.scenario_id, assertion_text.clone()))
                        .or_insert((0, 0));
                    if result.passed {
                        scenario_entry.0 += 1;
                    } else {
                        scenario_entry.1 += 1;
                    }

                    if let Ok(key) =
                        dispersion::AssertionKey::parse(&run.eval_case_id, run.scenario_id, &assertion_text)
                    {
                        flakiness.observe(key, run.attempt, result.passed);
                    }
                }
            }
            None => grading_unavailable = true,
        }

        if let Some(timing) = &sample.timing {
            let origin = SummaryRunOrigin {
                eval_case_id: run.eval_case_id.clone(),
                attempt: run.attempt,
            };
            if let Some(duration_ms) = timing.duration_ms {
                durations.push(MetricSample::new(run.scenario_id, origin.clone(), duration_ms));
            }
            if let Some(total_tokens) = timing.total_tokens {
                tokens.push(MetricSample::new(run.scenario_id, origin, total_tokens));
            }
        }
    }

    let always_pass = assertion_outcomes
        .iter()
        .filter(|(_, (passed, failed))| *passed > 0 && *failed == 0)
        .map(|(key, (passed, _))| AssertionStabilityRecord {
            eval_case_id: key.eval_id.clone(),
            assertion: key.assertion_text.clone(),
            attempts_observed: *passed,
            cross_iteration_delta: None,
        })
        .collect();

    let always_fail = assertion_outcomes
        .iter()
        .filter(|(_, (passed, failed))| *failed > 0 && *passed == 0)
        .map(|(key, (_, failed))| AssertionStabilityRecord {
            eval_case_id: key.eval_id.clone(),
            assertion: key.assertion_text.clone(),
            attempts_observed: *failed,
            cross_iteration_delta: None,
        })
        .collect();

    let helped_by_skill = scenario_assertion_rates
        .iter()
        .filter_map(|((eval_id, scenario, assertion_text), (passed, failed))| {
            if *scenario != ScenarioKind::WithSkill {
                return None;
            }
            let with_rate = pass_rate(*passed, *failed);
            let (without_passed, without_failed) = scenario_assertion_rates
                .get(&(eval_id.clone(), ScenarioKind::WithoutSkill, assertion_text.clone()))
                .copied()
                .unwrap_or((0, 0));
            let without_rate = pass_rate(without_passed, without_failed);
            if with_rate <= without_rate {
                return None;
            }
            Some(HelpedBySkillRecord {
                eval_case_id: eval_id.clone(),
                assertion: assertion_text.clone(),
                with_skill_pass_rate: with_rate,
                without_skill_pass_rate: without_rate,
                delta: with_rate - without_rate,
                with_skill_attempts: passed + failed,
                without_skill_attempts: without_passed + without_failed,
            })
        })
        .collect();

    AnalysisResult {
        always_pass,
        always_fail,
        helped_by_skill,
        flaky_assertions: flakiness.flaky_records(),
        timing_outliers: detect_timing_outliers(durations),
        token_outliers: detect_token_outliers(tokens),
        grading_unavailable,
    }
}

fn apply_cross_iteration_deltas(
    always_pass: &[AssertionStabilityRecord],
    always_fail: &[AssertionStabilityRecord],
    cross_iteration: Option<&CrossIterationSection>,
) -> (Vec<AssertionStabilityRecord>, Vec<AssertionStabilityRecord>) {
    let Some(cross) = cross_iteration else {
        return (always_pass.to_vec(), always_fail.to_vec());
    };

    let newly_pass: HashSet<_> = cross
        .newly_always_pass
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_case_id.clone(),
            assertion_text: record.assertion.clone(),
        })
        .collect();
    let lost_pass: HashSet<_> = cross
        .no_longer_always_pass
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_case_id.clone(),
            assertion_text: record.assertion.clone(),
        })
        .collect();
    let newly_fail: HashSet<_> = cross
        .newly_always_fail
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_case_id.clone(),
            assertion_text: record.assertion.clone(),
        })
        .collect();
    let lost_fail: HashSet<_> = cross
        .no_longer_always_fail
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_case_id.clone(),
            assertion_text: record.assertion.clone(),
        })
        .collect();

    let always_pass = always_pass
        .iter()
        .map(|record| {
            let key = AssertionKey {
                eval_id: record.eval_case_id.clone(),
                assertion_text: record.assertion.clone(),
            };
            let cross_iteration_delta = if newly_pass.contains(&key) {
                Some(CrossIterationDelta::New)
            } else if lost_pass.contains(&key) {
                Some(CrossIterationDelta::Lost)
            } else {
                Some(CrossIterationDelta::Stable)
            };
            AssertionStabilityRecord {
                cross_iteration_delta,
                ..record.clone()
            }
        })
        .collect();

    let always_fail = always_fail
        .iter()
        .map(|record| {
            let key = AssertionKey {
                eval_id: record.eval_case_id.clone(),
                assertion_text: record.assertion.clone(),
            };
            let cross_iteration_delta = if newly_fail.contains(&key) {
                Some(CrossIterationDelta::New)
            } else if lost_fail.contains(&key) {
                Some(CrossIterationDelta::Lost)
            } else {
                Some(CrossIterationDelta::Stable)
            };
            AssertionStabilityRecord {
                cross_iteration_delta,
                ..record.clone()
            }
        })
        .collect();

    (always_pass, always_fail)
}

/// Resolve the previous report to compare against, either the caller's explicit override
/// or a sibling detected next to `report_dir`. Either way this reads `report.json` exactly
/// once: an override is read here for the first and only time, and a detected candidate
/// arrives already parsed from the scan that found it.
///
/// `--previous` left unset and auto-detection finding no sibling are the same thing: there
/// is nothing to compare against, which is normal on a first iteration and reported as
/// `Ok(None)`. A `--previous` the caller did name is a claim that a specific report exists;
/// failing to read it is an infrastructure failure the caller asked to be told about, not a
/// quiet "nothing to compare against", so it is returned as `Err` instead of folded into the
/// same `None` a first iteration produces.
fn resolve_previous_report_for_summary(
    report_dir: &Path,
    current_iteration: u32,
    previous_report_dir: Option<&Path>,
) -> Result<Option<(PathBuf, ReportForSummary)>> {
    if let Some(dir) = previous_report_dir {
        let report = load_report(dir)?;
        return Ok(Some((dir.to_path_buf(), report)));
    }

    let Some(previous) = detect_previous_report_dir(report_dir, current_iteration) else {
        return Ok(None);
    };
    let report = previous.report_for_summary()?;
    Ok(Some((previous.dir().to_path_buf(), report)))
}

fn build_cross_iteration_section(
    previous_dir: &Path,
    previous_report: ReportForSummary,
    current: &AnalysisResult,
    mode: FailedRunsMode,
) -> Option<CrossIterationSection> {
    let previous = analyze_report(previous_dir, &previous_report, mode);

    // A previous iteration whose grading artifacts could not be read is not the same
    // thing as one that scored nothing: only the former must not be compared against.
    if previous.grading_unavailable {
        return None;
    }

    let current_pass = stability_key_set(&current.always_pass);
    let previous_pass = stability_key_set(&previous.always_pass);
    let current_fail = stability_key_set(&current.always_fail);
    let previous_fail = stability_key_set(&previous.always_fail);

    Some(CrossIterationSection {
        previous_report_id: previous_report.report.id,
        previous_iteration: previous_report.report.iteration,
        newly_always_pass: diff_records(&current.always_pass, &previous_pass),
        no_longer_always_pass: diff_records(&previous.always_pass, &current_pass),
        newly_always_fail: diff_records(&current.always_fail, &previous_fail),
        no_longer_always_fail: diff_records(&previous.always_fail, &current_fail),
    })
}

fn stability_key_set(records: &[AssertionStabilityRecord]) -> HashSet<AssertionKey> {
    records
        .iter()
        .map(|record| AssertionKey {
            eval_id: record.eval_case_id.clone(),
            assertion_text: record.assertion.clone(),
        })
        .collect()
}

fn diff_records(
    records: &[AssertionStabilityRecord],
    exclude: &HashSet<AssertionKey>,
) -> Vec<AssertionStabilityRecord> {
    records
        .iter()
        .filter(|record| {
            !exclude.contains(&AssertionKey {
                eval_id: record.eval_case_id.clone(),
                assertion_text: record.assertion.clone(),
            })
        })
        .cloned()
        .map(|mut record| {
            record.cross_iteration_delta = None;
            record
        })
        .collect()
}

/// A previous report a scan has already located and parsed, so a caller reads its
/// `report.json` exactly once no matter how many projections of it are needed.
pub struct PreviousReport {
    dir: PathBuf,
    document: serde_json::Value,
}

impl PreviousReport {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn drift_snapshot(&self) -> Result<eval_suite_drift::ReportDriftSnapshot> {
        eval_suite_drift::report_drift_snapshot_from_value(&self.document)
    }

    fn report_for_summary(&self) -> Result<ReportForSummary> {
        serde_json::from_value(self.document.clone()).map_err(EvalError::from)
    }
}

pub fn detect_previous_report_dir(report_dir: &Path, current_iteration: u32) -> Option<PreviousReport> {
    if current_iteration <= 1 {
        return None;
    }

    let target_iteration = current_iteration - 1;
    let skill_root = report_dir.parent()?;

    let entries = std::fs::read_dir(skill_root).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_dir() && candidate != report_dir && candidate.join("report.json").is_file())
        .collect();
    candidates.sort();

    for candidate in candidates {
        let Ok(document) = eval_suite_drift::read_report_value(&candidate) else {
            continue;
        };
        let Ok(report) = serde_json::from_value::<ReportForSummary>(document.clone()) else {
            continue;
        };
        if report.report.iteration == target_iteration {
            return Some(PreviousReport {
                dir: candidate,
                document,
            });
        }
    }

    None
}

fn load_report(report_dir: &Path) -> Result<ReportForSummary> {
    let content = std::fs::read_to_string(report_dir.join("report.json"))?;
    serde_json::from_str(&content).map_err(EvalError::from)
}

enum RunDisposition {
    Completed,
    Failed,
    Skipped,
    Excluded,
}

fn classify_run(run: &RunForSummary, mode: FailedRunsMode) -> RunDisposition {
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

    RunSample {
        grading: grading_path.as_ref().and_then(|path| read_json_file(path).ok()),
        timing: timing_path.as_ref().and_then(|path| read_json_file(path).ok()),
    }
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> std::result::Result<T, EvalError> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn normalize_assertion_key(assertion: &str) -> String {
    assertion.trim().to_string()
}

fn pass_rate(passed: u32, failed: u32) -> f64 {
    let total = passed + failed;
    if total == 0 {
        0.0
    } else {
        passed as f64 / total as f64
    }
}

fn detect_timing_outliers(samples: Vec<MetricSample<SummaryRunOrigin>>) -> Vec<TimingOutlierRecord> {
    dispersion::outliers(samples)
        .into_iter()
        .map(|outlier| {
            let (sample, spread) = outlier.into_parts();
            let (scenario, origin, value) = sample.into_parts();
            TimingOutlierRecord {
                eval_case_id: origin.eval_case_id,
                scenario_id: scenario.as_str().to_string(),
                attempt: origin.attempt,
                duration_ms: value,
                median_ms: spread.centre(),
                mad_ms: spread.spread(),
            }
        })
        .collect()
}

fn detect_token_outliers(samples: Vec<MetricSample<SummaryRunOrigin>>) -> Vec<TokenOutlierRecord> {
    dispersion::outliers(samples)
        .into_iter()
        .map(|outlier| {
            let (sample, spread) = outlier.into_parts();
            let (scenario, origin, value) = sample.into_parts();
            TokenOutlierRecord {
                eval_case_id: origin.eval_case_id,
                scenario_id: scenario.as_str().to_string(),
                attempt: origin.attempt,
                total_tokens: value,
                median_tokens: spread.centre(),
                mad_tokens: spread.spread(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_report(report_dir: &Path, runs: serde_json::Value, iteration: u32, report_id: &str) {
        write_report_with_hash(report_dir, runs, iteration, report_id, "sha256:def");
    }

    fn write_report_with_hash(
        report_dir: &Path,
        runs: serde_json::Value,
        iteration: u32,
        report_id: &str,
        evals_hash: &str,
    ) {
        fs::create_dir_all(report_dir).unwrap();
        let report = serde_json::json!({
            "report": {
                "id": report_id,
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

    fn sample_run(id: &str, eval_case_id: &str, scenario: &str, attempt: u32, status: &str) -> serde_json::Value {
        sample_run_with_split(id, eval_case_id, scenario, attempt, status, "train")
    }

    fn sample_run_with_split(
        id: &str,
        eval_case_id: &str,
        scenario: &str,
        attempt: u32,
        status: &str,
        split: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "eval_case_id": eval_case_id,
            "split": split,
            "scenario_id": scenario,
            "model_config_id": "ci-default",
            "skill_revision_id": "current",
            "attempt": attempt,
            "status": status,
            "paths": { "workspace": format!("runs/{id}/workspace") },
            "artifacts": [],
            "metrics": {}
        })
    }

    #[test]
    fn detects_always_pass_assertions() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            &report_dir,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"stable pass","passed":true}]}"#),
            None,
        );
        write_run_artifacts(
            &report_dir,
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"stable pass","passed":true}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

        assert_eq!(summary.always_pass.len(), 1);
        assert_eq!(summary.always_pass[0].eval_case_id, "case-a");
        assert_eq!(summary.always_pass[0].assertion, "stable pass");
        assert_eq!(summary.always_pass[0].attempts_observed, 2);
    }

    #[test]
    fn detects_always_fail_assertions() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 2, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            &report_dir,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"always broken","passed":false}]}"#),
            None,
        );
        write_run_artifacts(
            &report_dir,
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"always broken","passed":false}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

        assert_eq!(summary.always_fail.len(), 1);
        assert_eq!(summary.always_fail[0].attempts_observed, 2);
    }

    #[test]
    fn detects_helped_by_skill_assertions() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            &report_dir,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"skill helps","passed":true}]}"#),
            None,
        );
        write_run_artifacts(
            &report_dir,
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"skill helps","passed":false}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

        assert_eq!(summary.helped_by_skill.len(), 1);
        assert_eq!(summary.helped_by_skill[0].with_skill_pass_rate, 1.0);
        assert_eq!(summary.helped_by_skill[0].without_skill_pass_rate, 0.0);
        assert!((summary.helped_by_skill[0].delta - 1.0).abs() < 0.0001);
        assert_eq!(summary.helped_by_skill[0].with_skill_attempts, 1);
        assert_eq!(summary.helped_by_skill[0].without_skill_attempts, 1);
    }

    #[test]
    fn detects_flaky_assertions_across_attempts() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "with_skill", 2, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            &report_dir,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"flaky check","passed":true}]}"#),
            None,
        );
        write_run_artifacts(
            &report_dir,
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"flaky check","passed":false}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

        assert_eq!(summary.flaky_assertions.len(), 1);
        assert_eq!(summary.flaky_assertions[0].attempts, 2);
        assert_eq!(summary.flaky_assertions[0].pass_count, 1);
        assert!((summary.flaky_assertions[0].flakiness_ratio.value() - 0.5).abs() < 0.0001);
    }

    #[test]
    fn outlier_detection_requires_at_least_four_samples() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "with_skill", 2, "completed"),
                sample_run("run-003", "case-a", "with_skill", 3, "completed"),
            ]),
            1,
            "report-a",
        );
        for (run_id, duration_ms, tokens) in [("run-001", 1000, 100), ("run-002", 1100, 110), ("run-003", 9000, 900)] {
            write_run_artifacts(
                &report_dir,
                run_id,
                None,
                Some(&format!(
                    r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
                )),
            );
        }

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
        assert!(summary.timing_outliers.is_empty());
        assert!(summary.token_outliers.is_empty());

        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "with_skill", 2, "completed"),
                sample_run("run-003", "case-a", "with_skill", 3, "completed"),
                sample_run("run-004", "case-a", "with_skill", 4, "completed"),
                sample_run("run-005", "case-a", "with_skill", 5, "completed"),
                sample_run("run-006", "case-a", "with_skill", 6, "completed"),
                sample_run("run-007", "case-a", "with_skill", 7, "completed"),
                sample_run("run-008", "case-a", "with_skill", 8, "completed"),
            ]),
            1,
            "report-a",
        );
        for (run_id, duration_ms, tokens) in [
            ("run-001", 1000, 1000),
            ("run-002", 1010, 1010),
            ("run-003", 990, 990),
            ("run-004", 1005, 1005),
            ("run-005", 995, 995),
            ("run-006", 1000, 1000),
            ("run-007", 1010, 1010),
            ("run-008", 50000, 50000),
        ] {
            write_run_artifacts(
                &report_dir,
                run_id,
                None,
                Some(&format!(
                    r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
                )),
            );
        }

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
        assert_eq!(summary.timing_outliers.len(), 1);
        assert_eq!(summary.timing_outliers[0].attempt, 8);
        assert_eq!(summary.timing_outliers[0].duration_ms, 50000);
        assert_eq!(summary.timing_outliers[0].median_ms, 1002.5);
        assert_eq!(summary.timing_outliers[0].mad_ms, 7.5);
        assert_eq!(summary.token_outliers.len(), 1);
        assert_eq!(summary.token_outliers[0].total_tokens, 50000);
    }

    /// An arm whose runs mostly landed on the same number has a median absolute
    /// deviation of zero, and a band of width zero calls every remaining value unusual.
    #[test]
    fn an_arm_whose_runs_mostly_agree_exactly_reports_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "with_skill", 2, "completed"),
                sample_run("run-003", "case-a", "with_skill", 3, "completed"),
                sample_run("run-004", "case-a", "with_skill", 4, "completed"),
                sample_run("run-005", "case-a", "with_skill", 5, "completed"),
                sample_run("run-006", "case-a", "with_skill", 6, "completed"),
                sample_run("run-007", "case-a", "with_skill", 7, "completed"),
                sample_run("run-008", "case-a", "with_skill", 8, "completed"),
            ]),
            1,
            "report-a",
        );
        for (run_id, duration_ms, tokens) in [
            ("run-001", 1000, 1000),
            ("run-002", 1000, 1000),
            ("run-003", 1000, 1000),
            ("run-004", 1000, 1000),
            ("run-005", 1000, 1000),
            ("run-006", 1000, 1000),
            ("run-007", 1000, 1000),
            ("run-008", 50000, 50000),
        ] {
            write_run_artifacts(
                &report_dir,
                run_id,
                None,
                Some(&format!(
                    r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
                )),
            );
        }

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
        assert!(summary.timing_outliers.is_empty());
        assert!(summary.token_outliers.is_empty());
    }

    #[test]
    fn an_explicit_previous_that_cannot_be_read_fails_the_summary_instead_of_comparing_against_nothing() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("report-iter-2");
        let missing_previous = root.path().join("report-iter-1");

        write_report(
            &current,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            2,
            "report-iter-2",
        );
        write_run_artifacts(
            &current,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
            None,
        );

        let result = build_iteration_summary_document(
            &current,
            IterationSummaryOptions {
                previous_report_dir: Some(missing_previous),
                ..IterationSummaryOptions::default()
            },
        );

        assert!(
            result.is_err(),
            "a named --previous that cannot be read must fail the summary, not be silently treated as no previous at all"
        );
    }

    #[test]
    fn cross_iteration_deltas_surface_newly_stable_assertions() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous = skill_root.join("report-iter-1");
        let current = skill_root.join("report-iter-2");

        write_report(
            &previous,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            1,
            "report-iter-1",
        );
        write_run_artifacts(
            &previous,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"now stable","passed":false}]}"#),
            None,
        );

        write_report(
            &current,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            2,
            "report-iter-2",
        );
        for run_id in ["run-001", "run-002"] {
            write_run_artifacts(
                &current,
                run_id,
                Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
                None,
            );
        }

        let summary = build_iteration_summary_document(
            &current,
            IterationSummaryOptions {
                previous_report_dir: Some(previous),
                ..IterationSummaryOptions::default()
            },
        )
        .unwrap();

        let cross = summary.cross_iteration.as_ref().unwrap();
        assert_eq!(cross.newly_always_pass.len(), 1);
        assert_eq!(
            summary.always_pass[0].cross_iteration_delta,
            Some(CrossIterationDelta::New)
        );
    }

    #[test]
    fn cross_iteration_omitted_when_previous_grading_vanishes_after_detection() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous = skill_root.join("report-iter-1");
        let current = skill_root.join("report-iter-2");

        write_report(
            &previous,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            1,
            "report-iter-1",
        );
        write_run_artifacts(
            &previous,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"now stable","passed":false}]}"#),
            None,
        );

        write_report(
            &current,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            2,
            "report-iter-2",
        );
        write_run_artifacts(
            &current,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
            None,
        );

        // Detection reads report.json while the previous iteration's directory still
        // exists, exactly as `resolve_previous_report_for_summary` does.
        let previous_report = load_report(&previous).unwrap();

        // The previous iteration's directory is removed after detection but before the
        // grading artifacts underneath it are read for analysis.
        fs::remove_dir_all(&previous).unwrap();

        let current_report = load_report(&current).unwrap();
        let current_analysis = analyze_report(&current, &current_report, FailedRunsMode::default());

        let cross_iteration =
            build_cross_iteration_section(&previous, previous_report, &current_analysis, FailedRunsMode::default());

        assert!(
            cross_iteration.is_none(),
            "a previous iteration whose grading could not be read must not be compared against"
        );
    }

    #[test]
    fn cross_iteration_present_when_previous_iteration_scored_nothing() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous = skill_root.join("report-iter-1");
        let current = skill_root.join("report-iter-2");

        write_report(
            &previous,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            1,
            "report-iter-1",
        );
        // The grading artifact is present and reads fine; it simply scored nothing.
        write_run_artifacts(&previous, "run-001", Some(r#"{"assertion_results":[]}"#), None);

        write_report(
            &current,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            2,
            "report-iter-2",
        );
        write_run_artifacts(
            &current,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
            None,
        );

        let summary = build_iteration_summary_document(
            &current,
            IterationSummaryOptions {
                previous_report_dir: Some(previous),
                ..IterationSummaryOptions::default()
            },
        )
        .unwrap();

        let cross = summary
            .cross_iteration
            .as_ref()
            .expect("a readable but empty previous iteration must still be compared against");
        assert_eq!(cross.newly_always_pass.len(), 1);
    }

    #[test]
    fn detect_previous_report_dir_finds_matching_sibling() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        write_report(&previous_dir, serde_json::json!([]), 1, "report-iter-1");
        write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

        let previous = detect_previous_report_dir(&current_dir, 2).expect("a matching sibling exists");

        assert_eq!(previous.dir(), previous_dir.as_path());
        let report = previous.report_for_summary().unwrap();
        assert_eq!(report.report.id, "report-iter-1");
        assert_eq!(report.report.iteration, 1);
    }

    #[test]
    fn detect_previous_report_dir_skips_unparseable_candidates() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let broken_dir = skill_root.join("report-broken");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        std::fs::create_dir_all(&broken_dir).unwrap();
        std::fs::write(broken_dir.join("report.json"), "not json").unwrap();

        write_report(&previous_dir, serde_json::json!([]), 1, "report-iter-1");
        write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

        let previous = detect_previous_report_dir(&current_dir, 2).expect("the valid sibling is still found");
        assert_eq!(previous.dir(), previous_dir.as_path());
    }

    #[test]
    fn detect_previous_report_dir_returns_none_when_no_sibling_matches() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let current_dir = skill_root.join("report-iter-2");

        write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

        assert!(detect_previous_report_dir(&current_dir, 2).is_none());
    }

    #[test]
    fn detect_previous_report_dir_returns_none_for_a_first_iteration() {
        let root = tempfile::tempdir().unwrap();
        let report_dir = root.path().join("report");
        write_report(&report_dir, serde_json::json!([]), 1, "report-a");

        assert!(detect_previous_report_dir(&report_dir, 1).is_none());
    }

    #[test]
    fn previous_report_projection_survives_deletion_of_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        write_report(
            &previous_dir,
            serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
            1,
            "report-iter-1",
        );
        write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

        let previous = detect_previous_report_dir(&current_dir, 2).expect("a matching sibling exists");

        std::fs::remove_dir_all(&previous_dir).unwrap();
        assert!(!previous_dir.exists());

        let report = previous
            .report_for_summary()
            .expect("the report was already read during detection");
        assert_eq!(report.report.id, "report-iter-1");
        assert_eq!(report.report.iteration, 1);

        let snapshot = previous
            .drift_snapshot()
            .expect("the drift snapshot projects from the same in-memory document");
        assert_eq!(snapshot.iteration, 1);
    }

    #[test]
    fn iteration_summary_json_matches_schema() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        write_report(
            &report_dir,
            serde_json::json!([
                sample_run("run-001", "case-a", "with_skill", 1, "completed"),
                sample_run("run-002", "case-a", "without_skill", 1, "completed"),
            ]),
            1,
            "report-a",
        );
        write_run_artifacts(
            &report_dir,
            "run-001",
            Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
            Some(r#"{ "duration_ms": 1000, "total_tokens": 100 }"#),
        );
        write_run_artifacts(
            &report_dir,
            "run-002",
            Some(r#"{"assertion_results":[{"assertion":"a","passed":false}]}"#),
            Some(r#"{ "duration_ms": 2000, "total_tokens": 200 }"#),
        );

        let mut summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
        summary.generated_at = "2026-05-26T12:00:00Z".to_string();

        let json = serde_json::to_value(&summary).unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schemas/iteration-summary.json.schema.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<_> = validator.iter_errors(&json).map(|error| error.to_string()).collect();
        assert!(errors.is_empty(), "schema errors: {errors:?}");
    }

    fn write_with_skill_runs(
        report_dir: &Path,
        id_prefix: &str,
        eval_case_id: &str,
        split: &str,
        passed_count: usize,
        failed_count: usize,
    ) -> Vec<serde_json::Value> {
        let mut runs = Vec::new();
        let mut attempt = 1u32;
        for i in 0..passed_count {
            let id = format!("{id_prefix}-pass-{i}");
            runs.push(sample_run_with_split(
                &id,
                eval_case_id,
                "with_skill",
                attempt,
                "completed",
                split,
            ));
            write_run_artifacts(
                report_dir,
                &id,
                Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
                None,
            );
            attempt += 1;
        }
        for i in 0..failed_count {
            let id = format!("{id_prefix}-fail-{i}");
            runs.push(sample_run_with_split(
                &id,
                eval_case_id,
                "with_skill",
                attempt,
                "completed",
                split,
            ));
            write_run_artifacts(
                report_dir,
                &id,
                Some(r#"{"assertion_results":[{"assertion":"a","passed":false}]}"#),
                None,
            );
            attempt += 1;
        }
        runs
    }

    fn build_summary_against_previous(
        previous_dir: &Path,
        previous_runs: Vec<serde_json::Value>,
        current_dir: &Path,
        current_runs: Vec<serde_json::Value>,
    ) -> IterationSummaryDocument {
        write_report(
            previous_dir,
            serde_json::Value::Array(previous_runs),
            1,
            "report-iter-1",
        );
        write_report(current_dir, serde_json::Value::Array(current_runs), 2, "report-iter-2");

        build_iteration_summary_document(
            current_dir,
            IterationSummaryOptions {
                previous_report_dir: Some(previous_dir.to_path_buf()),
                ..IterationSummaryOptions::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn a_test_split_improvement_recommends_keep() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 0, 40);
        let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 40, 0);

        let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert_eq!(keep_or_revert.suites, SuiteComparison::SameSuite);
        assert!(matches!(
            keep_or_revert.by_split.get(&EvalSplit::Test),
            Some(SplitVerdict::Improved(_))
        ));
        assert_eq!(keep_or_revert.recommendation, Recommendation::Keep);
    }

    #[test]
    fn a_test_split_regression_recommends_revert_even_if_train_also_improved() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let mut previous_runs = write_with_skill_runs(&previous_dir, "prev-test", "case-a", "test", 40, 0);
        previous_runs.extend(write_with_skill_runs(
            &previous_dir,
            "prev-train",
            "case-b",
            "train",
            0,
            40,
        ));
        let mut current_runs = write_with_skill_runs(&current_dir, "cur-test", "case-a", "test", 0, 40);
        current_runs.extend(write_with_skill_runs(
            &current_dir,
            "cur-train",
            "case-b",
            "train",
            40,
            0,
        ));

        let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert!(matches!(
            keep_or_revert.by_split.get(&EvalSplit::Test),
            Some(SplitVerdict::Regressed(_))
        ));
        assert!(matches!(
            keep_or_revert.by_split.get(&EvalSplit::Train),
            Some(SplitVerdict::Improved(_))
        ));
        assert_eq!(
            keep_or_revert.recommendation,
            Recommendation::Revert,
            "a held-out regression is reason enough to revert on its own"
        );
    }

    #[test]
    fn train_improving_while_test_stays_flat_is_suspected_overfitting() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let mut previous_runs = write_with_skill_runs(&previous_dir, "prev-test", "case-a", "test", 5, 5);
        previous_runs.extend(write_with_skill_runs(
            &previous_dir,
            "prev-train",
            "case-b",
            "train",
            0,
            40,
        ));
        let mut current_runs = write_with_skill_runs(&current_dir, "cur-test", "case-a", "test", 5, 5);
        current_runs.extend(write_with_skill_runs(
            &current_dir,
            "cur-train",
            "case-b",
            "train",
            40,
            0,
        ));

        let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert!(matches!(
            keep_or_revert.by_split.get(&EvalSplit::Test),
            Some(SplitVerdict::Indistinguishable(_))
        ));
        assert!(matches!(
            keep_or_revert.by_split.get(&EvalSplit::Train),
            Some(SplitVerdict::Improved(_))
        ));
        assert_eq!(keep_or_revert.recommendation, Recommendation::SuspectedOverfitting);
    }

    #[test]
    fn a_suite_with_no_test_runs_in_either_iteration_reports_no_runs_and_stays_inconclusive() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let previous_runs = write_with_skill_runs(&previous_dir, "prev-train", "case-b", "train", 5, 5);
        let current_runs = write_with_skill_runs(&current_dir, "cur-train", "case-b", "train", 5, 5);

        let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert_eq!(
            keep_or_revert.by_split.get(&EvalSplit::Test),
            Some(&SplitVerdict::NoRuns)
        );
        assert_eq!(keep_or_revert.recommendation, Recommendation::Inconclusive);
    }

    /// A train-only suite (no `test`-split cases declared at all) is silence about
    /// generalization, not a finding of overfitting: nothing measured the held-out split
    /// because it does not exist, unlike `train_improving_while_test_stays_flat_is_suspected_overfitting`
    /// where a measured, flat test split is what makes the train gain suspicious.
    #[test]
    fn train_improving_with_no_test_runs_at_all_stays_inconclusive_not_overfitting() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let previous_runs = write_with_skill_runs(&previous_dir, "prev-train", "case-b", "train", 0, 40);
        let current_runs = write_with_skill_runs(&current_dir, "cur-train", "case-b", "train", 40, 0);

        let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert_eq!(
            keep_or_revert.by_split.get(&EvalSplit::Test),
            Some(&SplitVerdict::NoRuns),
            "no test-split runs exist in either iteration"
        );
        assert!(matches!(
            keep_or_revert.by_split.get(&EvalSplit::Train),
            Some(SplitVerdict::Improved(_))
        ));
        assert_eq!(
            keep_or_revert.recommendation,
            Recommendation::Inconclusive,
            "a suite with no held-out set cannot support suspected_overfitting"
        );
    }

    #[test]
    fn a_changed_eval_suite_reports_different_suites_instead_of_comparing() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let previous_runs = write_with_skill_runs(&previous_dir, "prev-test", "case-a", "test", 0, 40);
        let current_runs = write_with_skill_runs(&current_dir, "cur-test", "case-a", "test", 40, 0);

        write_report_with_hash(
            &previous_dir,
            serde_json::Value::Array(previous_runs),
            1,
            "report-iter-1",
            "sha256:before",
        );
        write_report_with_hash(
            &current_dir,
            serde_json::Value::Array(current_runs),
            2,
            "report-iter-2",
            "sha256:after",
        );

        let summary = build_iteration_summary_document(
            &current_dir,
            IterationSummaryOptions {
                previous_report_dir: Some(previous_dir),
                ..IterationSummaryOptions::default()
            },
        )
        .unwrap();

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert_eq!(
            keep_or_revert.suites,
            SuiteComparison::DifferentSuites {
                current_suite_hash: "sha256:after".to_string(),
                previous_suite_hash: "sha256:before".to_string(),
            }
        );
        assert!(
            keep_or_revert.by_split.is_empty(),
            "a verdict across two different suites is not a verdict about the change"
        );
        assert_eq!(keep_or_revert.recommendation, Recommendation::Inconclusive);
    }

    #[test]
    fn keep_or_revert_json_matches_schema() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 0, 40);
        let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 40, 0);

        let mut summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);
        summary.generated_at = "2026-05-26T12:00:00Z".to_string();

        let json = serde_json::to_value(&summary).unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../schemas/iteration-summary.json.schema.json")).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<_> = validator.iter_errors(&json).map(|error| error.to_string()).collect();
        assert!(errors.is_empty(), "schema errors: {errors:?}");
    }

    #[test]
    fn build_iteration_summary_reports_headroom_and_names_saturated_cases_per_split() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        let mut runs = write_with_skill_runs(&report_dir, "test", "case-a", "test", 40, 0);
        runs.extend(write_with_skill_runs(&report_dir, "train", "case-b", "train", 40, 0));
        write_report(&report_dir, serde_json::Value::Array(runs), 1, "report-a");

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

        let headroom = summary
            .headroom
            .as_ref()
            .expect("both splits pooled clear the default threshold");
        assert_eq!(
            headroom.saturated_case_ids,
            vec!["case-a".to_string(), "case-b".to_string()]
        );

        let test_headroom = summary.by_split[&EvalSplit::Test]
            .headroom
            .as_ref()
            .expect("the test split alone is also saturated");
        assert_eq!(test_headroom.saturated_case_ids, vec!["case-a".to_string()]);

        let train_headroom = summary.by_split[&EvalSplit::Train]
            .headroom
            .as_ref()
            .expect("the train split alone is also saturated");
        assert_eq!(train_headroom.saturated_case_ids, vec!["case-b".to_string()]);
    }

    #[test]
    fn build_iteration_summary_reports_no_headroom_warning_when_the_with_skill_arm_still_has_room() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        let runs = write_with_skill_runs(&report_dir, "test", "case-a", "test", 5, 5);
        write_report(&report_dir, serde_json::Value::Array(runs), 1, "report-a");

        let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

        assert!(summary.headroom.is_none());
        assert!(summary.by_split[&EvalSplit::Test].headroom.is_none());
    }

    #[test]
    fn build_iteration_summary_headroom_threshold_is_configurable() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = temp.path().join("report");
        let runs = write_with_skill_runs(&report_dir, "test", "case-a", "test", 5, 0);
        write_report(&report_dir, serde_json::Value::Array(runs), 1, "report-a");

        let default_summary =
            build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
        assert!(default_summary.headroom.is_none());

        let lenient_summary = build_iteration_summary_document(
            &report_dir,
            IterationSummaryOptions {
                headroom_threshold: HeadroomThreshold::parse(0.5).unwrap(),
                ..IterationSummaryOptions::default()
            },
        )
        .unwrap();
        assert!(lenient_summary.headroom.is_some());
    }

    #[test]
    fn keep_or_revert_is_capped_by_saturation_when_the_test_split_is_saturated() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 0, 40);
        let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 40, 0);

        let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert_eq!(
            keep_or_revert.capped_by_saturation,
            Some(SaturationCap::TestSplitSaturated),
            "the test split's with_skill arm passed everything, so the keep verdict cannot be read as ordinary"
        );
    }

    #[test]
    fn keep_or_revert_is_not_capped_when_the_test_split_still_has_room() {
        let root = tempfile::tempdir().unwrap();
        let skill_root = root.path().join("demo-skill");
        let previous_dir = skill_root.join("report-iter-1");
        let current_dir = skill_root.join("report-iter-2");

        let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 5, 5);
        let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 5, 5);

        let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

        let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
        assert_eq!(keep_or_revert.capped_by_saturation, None);
    }
}
