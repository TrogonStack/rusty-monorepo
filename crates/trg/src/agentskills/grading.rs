//! Mechanical and script-based grading for skill eval runs.

use std::io::Write;
mod constants;
use constants::{
    BASELINE_GRADER_SYSTEM_PROMPT, LLM_GRADER_SYSTEM_PROMPT, MOCK_EXPECTATION_EVALUATION, READ_ONLY_FIXTURE_EVALUATION,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::compare::{parse_winner, shuffle_swap, BlindLabel, ComparisonWinner};
use super::evals::{EvalCase, EvalError, EvalSuite, Result};
use super::graders::{
    self, BaselineReference, CaseGrader, GradeInput, GradeTarget, Grader, GraderOutcome, GraderWeight, TargetContent,
    TargetDeclaration,
};
use super::judge::{self, JudgeEndpoint, JudgeModel, JudgeProvider, JudgeRequest};
use super::judge_votes::{tally_opinions, JudgeVoteTally, JudgeVotes};
use super::model_family::ModelFamily;
use super::outputs::FINAL_MD;
use super::report::{
    recompute_scenario_summaries, GraderChoice, GradingStrategy, JudgeSettings, ReportDocument, RunRecord,
};
use super::runner::Runner;
use super::span_reference::SpanReference;
use super::transcript::{read_normalized_transcript, NormalizedTranscript};
use super::validation::{ValidationError, ValidationErrors};
use crate::telemetry::semconv::generated::attributes::GEN_AI_REQUEST_MODEL;
use crate::telemetry::semconv::trg::{
    EVAL_CASE_ID, EVAL_CASE_SPLIT, EVAL_GRADER_KIND, EVAL_GRADER_NAME, EVAL_ITERATION, EVAL_RUN_ID, EVAL_SCENARIO,
};
use crate::telemetry::ContentCapture;
use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use tracing::field::Empty;

pub(crate) mod telemetry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraderKind {
    Mechanical,
    Declarative,
    Llm,
    Script,
    NeedsLlm,
    None,
}

impl GraderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mechanical => "mechanical",
            Self::Declarative => "declarative",
            Self::Llm => "llm",
            Self::Script => "script",
            Self::NeedsLlm => "needs_llm",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum GraderMode {
    #[default]
    Auto,
    None,
    Llm,
    Script,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct GraderInfo {
    pub kind: GraderKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct AssertionGradeResult {
    #[schemars(length(min = 1))]
    pub assertion: String,
    pub passed: bool,
    pub evidence: String,
    pub grader: GraderInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// Present when the property cannot be observed on the runner that produced
    /// the run. Such a result is neither a pass nor a failure, so it is excluded
    /// from `summary.pass_rate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported: Option<String>,
    /// Present when the grader presupposes the skill, so its `passed` value is
    /// an indicator to read rather than a score to count. Kept out of
    /// `summary.pass_rate` in both arms, so the gap between them measures the
    /// work and not the premise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded: Option<String>,
    /// No grader could answer this assertion at all: it deferred to a judge and
    /// grading ran under `--grader none`, so no judge was consulted. Distinct from
    /// `unsupported`, which means a grader existed and the harness could not answer
    /// it, and from `excluded`, which means the author scoped it to the other arm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ungraded: Option<String>,
    /// How a panel of judges split, present only when more than one opinion was taken.
    /// A single opinion has no split to report, and `passed` already carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub votes: Option<JudgeVoteTally>,
    /// How much this result counts toward its case's score, relative to the
    /// rest of the case. Absent whenever the declaring grader left it
    /// unweighted, so an undeclared weight cannot be told apart in the
    /// serialized form from a build that predates weighting at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<GraderWeight>,
}

/// Which single bucket a result belongs to.
///
/// The three markers are independent options, so one result can carry several
/// at once and anything counting over them has to decide which wins. Deciding
/// that here, once, is what stops a printed list from naming assertions that
/// the number printed beside it does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionOutcome {
    /// Deliberately out of scope for this arm, which settles the result no
    /// matter what else is true of it: a check nobody was going to score loses
    /// nothing by also having been unanswerable.
    Excluded,
    /// The harness cannot answer this kind of question at all.
    Unsupported,
    /// In scope and answerable in principle, but nothing attempted it.
    Ungraded,
    /// Counted for or against the skill.
    Scored,
}

impl AssertionGradeResult {
    pub fn is_unsupported(&self) -> bool {
        self.unsupported.is_some()
    }

    pub fn is_excluded(&self) -> bool {
        self.excluded.is_some()
    }

    pub fn is_ungraded(&self) -> bool {
        self.ungraded.is_some()
    }

    pub fn outcome(&self) -> AssertionOutcome {
        if self.is_excluded() {
            AssertionOutcome::Excluded
        } else if self.is_unsupported() {
            AssertionOutcome::Unsupported
        } else if self.is_ungraded() {
            AssertionOutcome::Ungraded
        } else {
            AssertionOutcome::Scored
        }
    }

    pub fn is_scored(&self) -> bool {
        self.outcome() == AssertionOutcome::Scored
    }

    pub fn effective_weight(&self) -> GraderWeight {
        self.weight.unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct GradingSummary {
    pub passed: usize,
    pub failed: usize,
    pub total: usize,
    #[serde(default)]
    pub unsupported: usize,
    #[serde(default)]
    pub excluded: usize,
    /// No grader could even attempt these, so they carry no signal either way.
    #[serde(default)]
    pub ungraded: usize,
    /// Over the scored assertions only, so an ungradable property cannot drag a
    /// skill's score down on a runner that simply cannot be observed. `null` when
    /// nothing was scored at all, because no assertion answered is not the same
    /// result as every assertion failed.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub pass_rate: Option<f64>,
}

/// The assertions the ungraded count counted.
///
/// The number and the list beneath it are two views of one set, so they are
/// taken with one predicate rather than two that have to be kept in step.
fn ungraded_assertion_texts(results: &[AssertionGradeResult]) -> Vec<String> {
    results
        .iter()
        .filter(|result| result.outcome() == AssertionOutcome::Ungraded)
        .map(|result| result.assertion.clone())
        .collect()
}

/// The single place the grading arithmetic lives, so the writer and both
/// validators cannot drift from one another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GradingCounts {
    pub passed: usize,
    pub failed: usize,
    pub unsupported: usize,
    pub excluded: usize,
    pub ungraded: usize,
    pub total: usize,
}

impl GradingCounts {
    pub fn tally(results: &[AssertionGradeResult]) -> Self {
        let mut passed = 0;
        let mut unsupported = 0;
        let mut excluded = 0;
        let mut ungraded = 0;
        for result in results {
            match result.outcome() {
                AssertionOutcome::Excluded => excluded += 1,
                AssertionOutcome::Unsupported => unsupported += 1,
                AssertionOutcome::Ungraded => ungraded += 1,
                AssertionOutcome::Scored if result.passed => passed += 1,
                AssertionOutcome::Scored => {}
            }
        }
        let total = results.len();
        Self {
            passed,
            failed: total - passed - unsupported - excluded - ungraded,
            unsupported,
            excluded,
            ungraded,
            total,
        }
    }

    pub fn scored(self) -> usize {
        self.total - self.unsupported - self.excluded - self.ungraded
    }

    pub fn pass_rate(self) -> Option<f64> {
        if self.scored() == 0 {
            None
        } else {
            Some(self.passed as f64 / self.scored() as f64)
        }
    }

    pub fn summary(self) -> GradingSummary {
        GradingSummary {
            passed: self.passed,
            failed: self.failed,
            total: self.total,
            unsupported: self.unsupported,
            excluded: self.excluded,
            ungraded: self.ungraded,
            pass_rate: self.pass_rate(),
        }
    }
}

/// A case's own score: the fraction of scored weight it passed, `None` under
/// the same condition `GradingCounts::pass_rate` reports `None` under, since
/// weighting which scored assertions count for more cannot manufacture a
/// score out of a case that scored nothing.
///
/// A case where every grader left its weight undeclared takes the same code
/// path `GradingCounts::pass_rate` always has, so a suite that never opts
/// into weighting reports byte-identical scores to before weighting existed.
pub fn case_score(results: &[AssertionGradeResult]) -> Option<f64> {
    if results.iter().all(|r| r.weight.is_none()) {
        return GradingCounts::tally(results).pass_rate();
    }

    let total_weight: f64 = results
        .iter()
        .filter(|r| r.is_scored())
        .map(|r| r.effective_weight().value())
        .sum();
    if total_weight == 0.0 {
        return None;
    }
    let passed_weight: f64 = results
        .iter()
        .filter(|r| r.is_scored() && r.passed)
        .map(|r| r.effective_weight().value())
        .sum();
    Some(passed_weight / total_weight)
}

pub fn pass_rate_matches(reported: Option<f64>, expected: Option<f64>) -> bool {
    match (reported, expected) {
        (None, None) => true,
        (Some(reported), Some(expected)) => (reported - expected).abs() <= 0.0001,
        _ => false,
    }
}

pub fn describe_pass_rate(rate: Option<f64>) -> String {
    match rate {
        Some(rate) => format!("{rate}"),
        None => "not scored".to_string(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
pub struct GradingFile {
    pub assertion_results: Vec<AssertionGradeResult>,
    pub summary: GradingSummary,
}

#[derive(Debug, Clone, Default)]
pub struct GradeOptions {
    pub grader: GraderMode,
    pub grader_provider: JudgeProvider,
    pub grader_model: Option<String>,
    pub grader_command: Option<String>,
    pub grader_votes: JudgeVotes,
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GradeReport {
    pub runs_graded: usize,
    pub assertions_graded: usize,
    pub passed: usize,
    pub failed: usize,
    /// Every assertion no grader could even attempt, mechanical or judge. Sourced
    /// from the same tally as `unsupported` and `excluded` rather than a separate
    /// count, so this can never disagree with what the grading files themselves say.
    pub ungraded: usize,
    #[serde(default)]
    pub unsupported: usize,
    #[serde(default)]
    pub excluded: usize,
    #[serde(default)]
    pub ungraded_assertions: Vec<String>,
    #[serde(default)]
    pub run_statuses: GradedRunStatuses,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge_independence: Option<JudgeIndependenceWarning>,
}

/// Set when the judge comes from the same model family as a model that produced the runs it
/// grades.
///
/// A judge tends to rate output from its own family more kindly, so a pass recorded this way
/// is weaker evidence than one from an unrelated judge. It is a warning rather than a
/// refusal, since a same-family judge is sometimes the only one available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct JudgeIndependenceWarning {
    pub family: ModelFamily,
    pub judge_model: String,
    /// Each graded run's model in that family, or `<runner> default` for a run that left the
    /// choice to a harness that only defaults to that family.
    pub runner_models: Vec<String>,
}

impl JudgeIndependenceWarning {
    fn assess(judge: &JudgeModel, runner: Option<Runner>, runs: &[RunRecord]) -> Option<Self> {
        let family = ModelFamily::of(judge.as_str())?;
        let runner_models: std::collections::BTreeSet<String> = runs
            .iter()
            .filter(|run| run.started())
            .filter_map(|run| match &run.runner_model {
                Some(model) => (ModelFamily::of(model.as_str()) == Some(family)).then(|| model.to_string()),
                None => runner
                    .filter(|runner| ModelFamily::of_harness_default(*runner) == Some(family))
                    .map(|runner| format!("{} default", runner.display_name())),
            })
            .collect();
        (!runner_models.is_empty()).then(|| Self {
            family,
            judge_model: judge.as_str().to_string(),
            runner_models: runner_models.into_iter().collect(),
        })
    }

    pub fn describe(&self) -> String {
        format!(
            "WARN: judge model '{}' is from the same model family ({}) as the runs it graded ({}); a judge tends to favour its own family, so prefer a --grader-model from another family",
            self.judge_model,
            self.family,
            self.runner_models.join(", ")
        )
    }
}

/// The run statuses behind an assertion tally.
///
/// An assertion tally alone cannot separate a skill that failed its checks from
/// a runner that never produced a workspace to check, and both arrive as
/// `0/N passed`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GradedRunStatuses {
    pub completed: usize,
    pub failed: usize,
    pub timed_out: usize,
    /// Runs stopped for going past a cap on their tool calls or turns. Absent from
    /// grading files written before runs could be capped.
    #[serde(default)]
    pub budget_exhausted: usize,
    pub skipped: usize,
    pub unrecognized: usize,
}

impl GradedRunStatuses {
    pub fn record(&mut self, status: &str) {
        match status {
            "completed" => self.completed += 1,
            "failed" => self.failed += 1,
            "timeout" => self.timed_out += 1,
            "budget_exhausted" => self.budget_exhausted += 1,
            "skipped" => self.skipped += 1,
            _ => self.unrecognized += 1,
        }
    }

    pub fn not_completed(self) -> usize {
        self.failed + self.timed_out + self.budget_exhausted + self.skipped + self.unrecognized
    }

    pub fn describe_not_completed(self) -> Option<String> {
        let parts: Vec<String> = [
            (self.failed, "failed"),
            (self.timed_out, "timed out"),
            (self.budget_exhausted, "stopped at a run cap"),
            (self.skipped, "skipped"),
            (self.unrecognized, "of an unrecognized status"),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, label)| format!("{count} {label}"))
        .collect();

        if parts.is_empty() {
            None
        } else {
            Some(parts.join(", "))
        }
    }
}

#[derive(Debug, Clone)]
struct RunContext {
    run_dir: PathBuf,
    workspace_dir: PathBuf,
    outputs_dir: PathBuf,
    transcript_path: PathBuf,
    skill_dir: PathBuf,
    /// Outputs-relative paths the run created, read from the artifact index
    /// already persisted onto the run record rather than walked again here.
    created_files: Vec<String>,
}

/// The grading options plus the judge they resolve to, resolved once per
/// bundle so a missing credential is reported before any run is graded rather
/// than part way through one.
#[derive(Debug)]
struct GradeSession<'a> {
    options: &'a GradeOptions,
    judge: Option<JudgeEndpoint>,
}

impl<'a> GradeSession<'a> {
    fn open(options: &'a GradeOptions, suite: &EvalSuite) -> Result<Self> {
        let judge = if suite_needs_a_judge(options, suite) {
            let model = JudgeModel::resolve(options.grader_model.as_deref()).ok_or_else(|| {
                EvalError::Validation(
                    ValidationError::for_field(
                        "--grader-model",
                        format!(
                            "is required to grade with an LLM judge; pass it, set {}, or drop the llm graders",
                            judge::MODEL_ENV
                        ),
                    )
                    .into(),
                )
            })?;
            Some(JudgeEndpoint::resolve(
                options.grader_provider,
                &model,
                "--grader-model",
            )?)
        } else {
            None
        };

        Ok(Self { options, judge })
    }

    fn endpoint(&self) -> Result<&JudgeEndpoint> {
        self.judge.as_ref().ok_or_else(|| {
            EvalError::Validation(
                ValidationError::for_field("--grader-model", "no LLM judge was resolved for this run").into(),
            )
        })
    }
}

/// Whether anything in the suite can reach the LLM judge under these options.
///
/// `--grader llm` always can. Under `auto` only a declared `llm` grader can, so a suite
/// of purely mechanical graders is graded without a credential.
fn suite_needs_a_judge(options: &GradeOptions, suite: &EvalSuite) -> bool {
    match options.grader {
        GraderMode::Llm => true,
        GraderMode::None | GraderMode::Script => false,
        GraderMode::Auto => suite.evals.iter().any(|case| {
            case.graders
                .iter()
                .any(|declared| matches!(declared.grader, Grader::Llm { .. } | Grader::Baseline { .. }))
        }),
    }
}

pub fn grade_report_bundle(report_dir: &Path, options: GradeOptions) -> Result<GradeReport> {
    let span = tracing::info_span!("grade", "otel.status_code" = Empty, { ERROR_TYPE } = Empty);
    let graded = span.in_scope(|| grade_bundle(report_dir, options));
    telemetry::record_result(&span, &graded);
    graded
}

fn open_session<'a>(options: &'a GradeOptions, suite: &EvalSuite) -> Result<GradeSession<'a>> {
    let span = tracing::info_span!(
        "open judge session",
        "otel.status_code" = Empty,
        { ERROR_TYPE } = Empty,
        { GEN_AI_REQUEST_MODEL } = Empty,
    );
    let session = span.in_scope(|| GradeSession::open(options, suite));
    telemetry::record_result(&span, &session);
    if let Some(judge) = session.as_ref().ok().and_then(|session| session.judge.as_ref()) {
        span.record(GEN_AI_REQUEST_MODEL, judge.model.as_str());
    }
    session
}

fn grade_bundle(report_dir: &Path, options: GradeOptions) -> Result<GradeReport> {
    let report_path = report_dir.join("report.json");
    let report_content = std::fs::read_to_string(&report_path).map_err(|e| {
        EvalError::Validation(
            ValidationError::for_field(
                format!("report '{}'", report_path.display()),
                format!("failed to read: {e}"),
            )
            .into(),
        )
    })?;
    let mut document: ReportDocument = serde_json::from_str(&report_content).map_err(|e| {
        EvalError::Validation(
            ValidationError::for_field(
                format!("report '{}'", report_path.display()),
                format!("failed to parse: {e}"),
            )
            .into(),
        )
    })?;

    let skill_path = PathBuf::from(&document.suite.skill_path);
    let suite: EvalSuite =
        super::case_directories::resolve_eval_suite(&crate::fs::RealFS, &skill_path, &document.suite.eval_dir)?.suite;

    let case_index: HashMap<String, &EvalCase> = suite.evals.iter().map(|c| (c.id.to_string(), c)).collect();

    let session = open_session(&options, &suite)?;
    let grading_strategy = build_grading_strategy(&options, session.judge.as_ref().map(|judge| &judge.model));
    if document.dimensions.grading_strategies.is_empty() {
        document.dimensions.grading_strategies.push(grading_strategy.clone());
    }

    let mut report = GradeReport {
        runs_graded: 0,
        assertions_graded: 0,
        passed: 0,
        failed: 0,
        ungraded: 0,
        unsupported: 0,
        excluded: 0,
        ungraded_assertions: Vec::new(),
        run_statuses: GradedRunStatuses::default(),
        judge_independence: session.judge.as_ref().and_then(|judge| {
            let runner = document
                .report
                .runner
                .as_deref()
                .and_then(|name| <Runner as clap::ValueEnum>::from_str(name, false).ok());
            JudgeIndependenceWarning::assess(&judge.model, runner, &document.runs)
        }),
    };

    let runs = document.runs.clone();
    for (run, run_mut) in runs.iter().zip(document.runs.iter_mut()) {
        let case = match case_index.get(&run.eval_case_id) {
            Some(case) => *case,
            None => {
                return Err(EvalError::Validation(
                    ValidationError::for_field(
                        format!("run '{}'", run.id),
                        format!("eval case '{}' not found in evals.json", run.eval_case_id),
                    )
                    .into(),
                ));
            }
        };

        if !run.started() {
            report.run_statuses.record(&run.status);
            continue;
        }

        let span = grade_run_span(run);
        run_mut.grade_trace = SpanReference::of(&span);
        let graded = span.in_scope(|| {
            grade_run(
                RunToGrade {
                    report_dir,
                    run,
                    case,
                    skill_path: &skill_path,
                    session: &session,
                },
                run_mut,
                &mut report,
            )
        });
        telemetry::record_result(&span, &graded);
        graded?;
    }

    update_report_after_grading(&mut document, &runs, report_dir, &grading_strategy)?;
    std::fs::write(report_dir.join("report.json"), serde_json::to_string_pretty(&document)?)?;

    Ok(report)
}

/// One started run and everything grading it reads.
struct RunToGrade<'a> {
    report_dir: &'a Path,
    run: &'a RunRecord,
    case: &'a EvalCase,
    skill_path: &'a Path,
    session: &'a GradeSession<'a>,
}

/// `grade run`, linked to the span the run executed under when the report
/// recorded one, since the run finished in an earlier trace or an earlier part
/// of this one.
fn grade_run_span(run: &RunRecord) -> tracing::Span {
    let span = tracing::info_span!(
        "grade run",
        "otel.status_code" = Empty,
        { ERROR_TYPE } = Empty,
        { EVAL_RUN_ID } = run.id.as_str(),
        { EVAL_CASE_ID } = run.eval_case_id.as_str(),
        { EVAL_CASE_SPLIT } = run.split.as_str(),
        { EVAL_SCENARIO } = run.scenario_id.as_str(),
        { EVAL_ITERATION } = i64::from(run.iteration),
    );
    telemetry::link_to(&span, run.span_reference());
    span
}

fn grade_run(target: RunToGrade<'_>, run_mut: &mut RunRecord, report: &mut GradeReport) -> Result<()> {
    let RunToGrade {
        report_dir,
        run,
        case,
        skill_path,
        session,
    } = target;
    let options = session.options;
    let ctx = run_context(report_dir, run, skill_path);
    let mut assertion_results = Vec::with_capacity(case.graders.len() + run.mock_violations.len());
    let mut grader_kinds: Vec<&'static str> = Vec::with_capacity(assertion_results.capacity());

    let declarative = DeclarativeContext::load(&ctx)?;
    for grader in &case.graders {
        assertion_results.push(grade_with_span(grader, case, &declarative, &ctx, session)?);
        grader_kinds.push(grader.grader.kind());
    }

    // A read-only fixture that was modified during the run is a failure of the run
    // itself, not a property left for an assertion or grader to notice, so it is added
    // here rather than left to whichever declared checks the case happens to have.
    for path in &run.read_only_fixture_violations {
        assertion_results.push(read_only_fixture_violation_result(path));
        grader_kinds.push(READ_ONLY_FIXTURE_EVALUATION);
    }

    for violation in &run.mock_violations {
        assertion_results.push(mock_violation_result(violation));
        grader_kinds.push(MOCK_EXPECTATION_EVALUATION);
    }

    restore_when_nothing_would_be_scored(&mut assertion_results);

    report.assertions_graded += assertion_results.len();
    report
        .ungraded_assertions
        .extend(ungraded_assertion_texts(&assertion_results));
    let counts = GradingCounts::tally(&assertion_results);
    report.passed += counts.passed;
    report.failed += counts.failed;
    report.unsupported += counts.unsupported;
    report.excluded += counts.excluded;
    report.ungraded += counts.ungraded;
    run_mut.case_score = case_score(&assertion_results);

    emit_evaluations(run, &assertion_results, &grader_kinds, run_mut.case_score);

    let span = tracing::info_span!(
        "write grading results",
        "otel.status_code" = Empty,
        { ERROR_TYPE } = Empty
    );
    let written = span.in_scope(|| {
        let grading = build_grading_file(assertion_results)?;
        validate_grading_document(&grading, options.strict)?;

        let grading_path = ctx.run_dir.join("grading.json");
        std::fs::write(&grading_path, serde_json::to_string_pretty(&grading)?)?;

        store_grader_artifacts(run_mut, report_dir, &ctx, options, &grading)
    });
    telemetry::record_result(&span, &written);
    written?;

    report.run_statuses.record(&run.status);
    report.runs_graded += 1;
    Ok(())
}

fn grade_with_span(
    declared: &CaseGrader,
    eval_case: &EvalCase,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let kind = declared.grader.kind();
    let name = declared.name.as_ref().map_or(kind, |name| name.as_str());
    let span = tracing::info_span!(
        "grader",
        "otel.name" = format!("grader {kind}"),
        "otel.status_code" = Empty,
        { ERROR_TYPE } = Empty,
        { EVAL_GRADER_NAME } = name,
        { EVAL_GRADER_KIND } = kind,
    );
    let graded = span.in_scope(|| grade_declaratively(declared, eval_case, declarative, ctx, session));
    telemetry::record_result(&span, &graded);
    graded
}

/// One evaluation event and one counted assertion per result, plus the case's
/// own score, each in the context of the span the run executed under.
fn emit_evaluations(
    run: &RunRecord,
    results: &[AssertionGradeResult],
    grader_kinds: &[&'static str],
    score: Option<f64>,
) {
    let content = ContentCapture::from_env();
    let parent = run.span_reference();
    for (result, kind) in results.iter().zip(grader_kinds) {
        let name = result.name.as_deref().unwrap_or(kind);
        telemetry::Evaluation::of_assertion(result, name, kind, &run.eval_case_id, &run.id).emit(parent, content);
        telemetry::count_assertion(result, kind);
    }
    if let Some(score) = score {
        telemetry::Evaluation {
            name: telemetry::CASE_SCORE_EVALUATION,
            label: None,
            score: Some(score),
            explanation: None,
            grader_kind: None,
            case_id: &run.eval_case_id,
            run_id: Some(&run.id),
            provenance: telemetry::Provenance::Live,
        }
        .emit(parent, content);
    }
}

fn run_context(report_dir: &Path, run: &RunRecord, skill_path: &Path) -> RunContext {
    let workspace_dir = report_dir.join(&run.paths.workspace);
    let run_dir = workspace_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| report_dir.to_path_buf());
    let outputs_dir = if run_dir.join("outputs").is_dir() {
        run_dir.join("outputs")
    } else {
        workspace_dir.join("outputs")
    };
    let transcript_path = run_dir.join("transcript.jsonl");
    let created_files = created_files_from_artifacts(run);

    RunContext {
        run_dir,
        workspace_dir,
        outputs_dir,
        transcript_path,
        skill_dir: skill_path.to_path_buf(),
        created_files,
    }
}

/// Recovers outputs-relative filenames from the artifact index the run already
/// recorded, so `GradeTarget::CreatedFiles` does not re-walk the output directory.
fn created_files_from_artifacts(run: &RunRecord) -> Vec<String> {
    let outputs_root = Path::new(&run.paths.outputs);
    run.artifacts
        .iter()
        .filter(|artifact| artifact.get("kind").and_then(serde_json::Value::as_str) == Some("output"))
        .filter_map(|artifact| artifact.get("path").and_then(serde_json::Value::as_str))
        .map(|path| relative_slash_path(outputs_root, Path::new(path)))
        .collect()
}

/// `judge` is the model this pass actually resolved, per [`GradeSession::open`]'s reading
/// of [`suite_needs_a_judge`] for `options.grader` and the suite being graded, and is
/// `None` when no judge was reached. It is taken as an argument rather than recomputed
/// here so that whether `auto` used a judge is decided in exactly one place, and it
/// carries the model rather than a flag so a record cannot name a different model from
/// the one that answered.
fn build_grading_strategy(options: &GradeOptions, judge: Option<&JudgeModel>) -> GradingStrategy {
    let judge_settings = || JudgeSettings {
        provider: options.grader_provider,
        model: judge
            .map(JudgeModel::to_string)
            .or_else(|| options.grader_model.clone()),
        votes: options.grader_votes,
    };
    let grader = match options.grader {
        GraderMode::Auto => GraderChoice::Auto {
            judge: judge.is_some().then(judge_settings),
        },
        GraderMode::None => GraderChoice::None,
        GraderMode::Llm => GraderChoice::Llm {
            judge: judge_settings(),
        },
        GraderMode::Script => GraderChoice::Script {
            command: options.grader_command.clone(),
        },
    };
    GradingStrategy {
        grader,
        strict: options.strict,
    }
}

/// An artifact the run never wrote is itself a finding: the agent produced no
/// final answer, and empty content is what the graders should be shown. An
/// artifact that exists but cannot be read is not that finding. Collapsing the
/// two grades every declared check against empty content and reports the case
/// as one where the agent wrote nothing, which sends whoever reads the report
/// looking at the skill instead of at the file nothing could read.
fn read_artifact(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(EvalError::Io(std::io::Error::new(
            err.kind(),
            format!("{}: {err}", path.display()),
        ))),
    }
}

/// The run artifacts a declarative grader reads, loaded once per run.
struct DeclarativeContext {
    final_text: String,
    raw_transcript: String,
    transcript: Option<NormalizedTranscript>,
}

impl DeclarativeContext {
    fn load(ctx: &RunContext) -> Result<Self> {
        Ok(Self {
            final_text: read_artifact(&ctx.outputs_dir.join(FINAL_MD))?,
            raw_transcript: read_artifact(&ctx.transcript_path)?,
            transcript: read_normalized_transcript(&ctx.transcript_path).ok(),
        })
    }

    fn input<'a>(&'a self, ctx: &'a RunContext) -> GradeInput<'a> {
        GradeInput {
            final_text: &self.final_text,
            run_dir: &ctx.run_dir,
            workspace_dir: &ctx.workspace_dir,
            outputs_dir: &ctx.outputs_dir,
            raw_transcript: &self.raw_transcript,
            transcript: self.transcript.as_ref(),
            skill_dir: &ctx.skill_dir,
            created_files: &ctx.created_files,
        }
    }
}

fn grade_declaratively(
    declared: &CaseGrader,
    eval_case: &EvalCase,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let options = session.options;
    let grader = &declared.grader;
    let assertion = grader.describe();
    let outcome = graders::evaluate(grader, &declarative.input(ctx));
    let excluded = declared.exclusion_reason();
    let name = declared.name.as_ref().map(ToString::to_string);

    let declarative_info = GraderInfo {
        kind: GraderKind::Declarative,
        model: None,
        command: None,
    };

    Ok(match outcome {
        GraderOutcome::Passed { evidence } => AssertionGradeResult {
            assertion,
            passed: true,
            evidence,
            grader: declarative_info,
            name,
            rationale: None,
            unsupported: None,
            excluded,
            ungraded: None,
            votes: None,
            weight: declared.weight,
        },
        GraderOutcome::Failed { evidence } => AssertionGradeResult {
            assertion,
            passed: false,
            evidence,
            grader: declarative_info,
            name,
            rationale: None,
            unsupported: None,
            excluded,
            ungraded: None,
            votes: None,
            weight: declared.weight,
        },
        GraderOutcome::Unsupported { reason } => AssertionGradeResult {
            assertion,
            passed: false,
            evidence: reason.clone(),
            grader: declarative_info,
            name,
            rationale: None,
            unsupported: Some(reason),
            excluded,
            ungraded: None,
            votes: None,
            weight: declared.weight,
        },
        GraderOutcome::Deferred { criterion, target } => {
            let mut result = match options.grader {
                GraderMode::Llm | GraderMode::Auto => grade_with_llm(
                    &criterion,
                    &target,
                    &CaseBrief::of(eval_case),
                    declarative,
                    ctx,
                    session,
                )?,
                GraderMode::Script => grade_with_script(&criterion, None, eval_case, ctx, options)?,
                GraderMode::None => needs_llm_result(&criterion, "grader mode is none, so no judge was consulted"),
            };
            result.excluded = excluded;
            result.name = name;
            result.weight = declared.weight;
            result
        }
        GraderOutcome::Comparison {
            criterion,
            target,
            reference,
        } => {
            let mut result = match options.grader {
                GraderMode::Llm | GraderMode::Auto => grade_against_baseline(
                    &assertion,
                    &criterion,
                    &target,
                    &reference,
                    eval_case,
                    declarative,
                    ctx,
                    session,
                )?,
                GraderMode::Script => grade_with_script(&criterion, Some(&reference), eval_case, ctx, options)?,
                GraderMode::None => needs_llm_result(&assertion, "grader mode is none, so no judge was consulted"),
            };
            result.excluded = excluded;
            result.name = name;
            result.weight = declared.weight;
            result
        }
        GraderOutcome::AuthoringError { reason } => {
            return Err(EvalError::Validation(
                ValidationError::for_field(assertion, reason).into(),
            ));
        }
    })
}

/// Turn one logged `expect` mismatch into an ordinary failing assertion.
///
/// A violation never stops the mock from answering the call, so it never fails a run on
/// its own; this is the one place it turns into something grading already knows how to
/// tally, rather than a second reporting channel a caller has to learn.
fn mock_violation_result(violation: &super::mocks::MockViolation) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: format!(
            "mcp mock {}/{} honours its declared expectations",
            violation.server.as_str(),
            violation.tool.as_str()
        ),
        passed: false,
        evidence: format!(
            "`{}` was expected to satisfy `{}`, but the call received {}",
            violation.path.as_str(),
            violation.constraint,
            violation.received
        ),
        grader: GraderInfo {
            kind: GraderKind::Mechanical,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    }
}

/// A case whose every check is arm-scoped would otherwise measure nothing at
/// all, which is never what the author meant by writing it, so the exclusions
/// are lifted and the case is scored as declared. But lifting them only helps
/// when doing so would actually produce something scorable; a case that is
/// entirely ungraded, or whose only observable checks are also unsupported,
/// gains nothing from the lift and must not have it applied.
fn restore_when_nothing_would_be_scored(results: &mut [AssertionGradeResult]) {
    let scored = results.iter().filter(|result| result.is_scored()).count();
    if scored > 0 {
        return;
    }
    let lifting_would_score = results
        .iter()
        .any(|result| result.is_excluded() && !result.is_unsupported() && !result.is_ungraded());
    if !lifting_would_score {
        return;
    }
    for result in results.iter_mut() {
        result.excluded = None;
    }
}

fn read_only_fixture_violation_result(path: &str) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: format!("read-only fixture '{path}' is unchanged"),
        passed: false,
        evidence: format!("fixture '{path}' did not match its source after the run"),
        grader: GraderInfo {
            kind: GraderKind::Mechanical,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    }
}

fn needs_llm_result(assertion: &str, evidence: &str) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: false,
        evidence: evidence.to_string(),
        grader: GraderInfo {
            kind: GraderKind::NeedsLlm,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: Some(evidence.to_string()),
        votes: None,
        weight: None,
    }
}

/// A grader script that did not exit cleanly graded nothing, whatever it printed.
///
/// Recording the crash as a failed assertion reads as evidence the skill did the wrong
/// thing, which is the one thing it is not evidence of. Ungraded is the honest record, and
/// the gate already refuses to pass a suite carrying anything ungraded, so a broken script
/// still stops the build without discarding every other case in the run.
/// The payload goes over on its own thread so that neither side can wedge the other.
///
/// A grader is free to print more than a pipe holds before it reads its input, and a parent that
/// insists on finishing the write first would then wait on a buffer only the grader can drain
/// while the grader waits on one only the parent can drain. Neither ever gives way.
fn feed_payload(child: &mut std::process::Child, payload: Vec<u8>) -> Option<PayloadHandover> {
    let mut stdin = child.stdin.take()?;
    Some(std::thread::spawn(move || stdin.write_all(&payload)))
}

type PayloadHandover = std::thread::JoinHandle<std::io::Result<()>>;

/// A grader that stopped reading is answered by its exit status, not by the write that failed.
///
/// A script which exits before draining the payload closes the pipe under us, and reporting that
/// as harness I/O aborts the entire run over one broken grader, discarding every case already
/// paid for. Whatever it exited with is the honest account of what it did.
fn payload_handed_over(handover: Option<PayloadHandover>) -> Result<()> {
    let Some(handover) = handover else {
        return Ok(());
    };
    match handover.join() {
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Ok(other) => other.map_err(EvalError::Io),
        Err(_) => Err(EvalError::Io(std::io::Error::other(
            "the thread handing the payload to the grader script panicked",
        ))),
    }
}

fn script_crashed_result(assertion: &str, command: &str, output: &std::process::Output) -> AssertionGradeResult {
    let evidence = format!(
        "script grader exited with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: false,
        evidence: evidence.clone(),
        grader: GraderInfo {
            kind: GraderKind::Script,
            model: None,
            command: Some(command.to_string()),
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: Some(evidence),
        votes: None,
        weight: None,
    }
}

/// `baseline` is `Some` only for a `baseline` grader, and carrying it here is what
/// keeps `--grader script` from quietly answering a different question: a script
/// handed only the criterion would grade the run on its own, which is not what a
/// comparison asked.
fn grade_with_script(
    assertion: &str,
    baseline: Option<&BaselineReference>,
    eval_case: &EvalCase,
    ctx: &RunContext,
    options: &GradeOptions,
) -> Result<AssertionGradeResult> {
    let command = options.grader_command.as_deref().ok_or_else(|| {
        EvalError::Validation(ValidationError::for_field("--grader-command", "is required when --grader script").into())
    })?;

    #[derive(Serialize)]
    struct BaselineInput<'a> {
        reference: &'a str,
        content: &'a str,
    }
    #[derive(Serialize)]
    struct ScriptInput<'a> {
        assertion: &'a str,
        workspace: &'a Path,
        outputs: &'a Path,
        transcript: &'a Path,
        #[serde(skip_serializing_if = "Option::is_none")]
        baseline: Option<BaselineInput<'a>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        grader_hints: Option<&'a HashMap<String, serde_json::Value>>,
    }
    let input = serde_json::to_value(ScriptInput {
        assertion,
        workspace: &ctx.workspace_dir,
        outputs: &ctx.outputs_dir,
        transcript: &ctx.transcript_path,
        baseline: baseline.map(|value| BaselineInput {
            reference: value.declared(),
            content: value.text(),
        }),
        grader_hints: eval_case.grader_hints.as_ref(),
    })?;

    let cli = telemetry::CliSpan::start(command);
    let mut process = Command::new(command);
    process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&ctx.workspace_dir);
    cli.inject(&mut process);
    let mut child = process.spawn().map_err(|e| {
        cli.spawn_failed();
        EvalError::Validation(ValidationError::for_field("--grader-command", e.to_string()).into())
    })?;
    cli.spawned(child.id());

    let handover = feed_payload(&mut child, serde_json::to_string(&input)?.into_bytes());
    let output = child.wait_with_output().map_err(|e| {
        cli.failed("wait_failed");
        EvalError::Io(e)
    })?;
    cli.exited(&output.status);
    drop(cli);
    payload_handed_over(handover)?;

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();

    #[derive(Serialize)]
    struct ScriptResult<'a> {
        assertion: &'a str,
        exit_code: Option<i32>,
        stdout: &'a str,
        stderr: std::borrow::Cow<'a, str>,
    }
    let artifact_path = ctx.run_dir.join("grader-script-result.json");
    std::fs::write(
        &artifact_path,
        serde_json::to_string_pretty(&ScriptResult {
            assertion,
            exit_code: output.status.code(),
            stdout: &raw,
            stderr: String::from_utf8_lossy(&output.stderr),
        })?,
    )?;

    if !output.status.success() {
        return Ok(script_crashed_result(assertion, command, &output));
    }

    let parsed: ScriptGraderResponse = serde_json::from_str(&raw).map_err(|e| {
        EvalError::Validation(
            ValidationError::for_field(
                "script grader output",
                format!("invalid JSON contract: {e}; expected {{\"passed\": bool, \"evidence\": string, \"rationale\"?: string}}"),
            )
            .into(),
        )
    })?;

    Ok(AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: parsed.passed,
        evidence: parsed.evidence,
        grader: GraderInfo {
            kind: GraderKind::Script,
            model: None,
            command: Some(command.to_string()),
        },
        rationale: parsed.rationale,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    })
}

#[derive(Debug, Deserialize)]
struct ScriptGraderResponse {
    passed: bool,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

fn grade_with_llm(
    assertion: &str,
    target: &TargetDeclaration,
    brief: &CaseBrief,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let target = target.resolve();
    if let Some(evidence) = blank_answer(&target, declarative, ctx) {
        return Ok(settled_without_a_judge(assertion, evidence));
    }

    let endpoint = session.endpoint()?;
    let payload = llm_grader_payload(assertion, &target, brief, declarative, ctx)?;
    let mut request = JudgeRequest::new(LLM_GRADER_SYSTEM_PROMPT, payload.text);
    if let Some(image) = payload.image {
        request = request.with_image(image);
    }

    let votes = session.options.grader_votes;
    let mut opinions = Vec::with_capacity(votes.count() as usize);
    for _ in votes.ballots() {
        let reply = judge::judge(endpoint, &request, "--grader-model")?;
        let parsed: LlmGraderResponse = judge::parse_json_reply(&reply.content, "--grader-model")?;
        opinions.push((parsed.passed, parsed));
    }

    let verdict = tally_opinions(opinions).ok_or_else(|| {
        EvalError::Validation(
            ValidationError::for_field("--grader-votes", "no judge opinion was taken for this assertion").into(),
        )
    })?;

    Ok(AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: verdict.tally.majority_passed(),
        evidence: verdict.opinion.evidence,
        grader: GraderInfo {
            kind: GraderKind::Llm,
            model: Some(endpoint.model.to_string()),
            command: None,
        },
        rationale: verdict.opinion.rationale,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: (!votes.is_single()).then_some(verdict.tally),
        weight: None,
    })
}

/// Why the run left the judge nothing to grade, when it did.
///
/// A judge shown an empty answer still returns a verdict, and a lenient one passes
/// assertions that read as satisfied by saying nothing, so a blank answer is failed here
/// without billing a judge for it. `created_files` and `mock_calls` are listings rather
/// than answers: an empty one is an observation an assertion may be about, so it is still
/// put to the judge, as is a target that is missing, which already says why.
///
/// A `final_text` judge is also shown the files directly under `outputs/`, so a run that
/// wrote its answer to a file and said nothing in its final message has still answered,
/// and the judge gets to read it.
fn blank_answer(target: &GradeTarget, declarative: &DeclarativeContext, ctx: &RunContext) -> Option<String> {
    if matches!(target, GradeTarget::CreatedFiles | GradeTarget::MockCalls) {
        return None;
    }
    if matches!(target, GradeTarget::FinalText)
        && collect_output_files(&ctx.outputs_dir, OutputScope::TopLevel)
            .iter()
            .any(|(_, text)| !text.trim().is_empty())
    {
        return None;
    }
    match declarative.input(ctx).target_content(target) {
        TargetContent::Text(text) if text.trim().is_empty() => Some(format!(
            "{target} is empty, so the run gave no answer for the judge to grade"
        )),
        TargetContent::Text(_) | TargetContent::Image { .. } | TargetContent::Missing(_) => None,
    }
}

/// Which blind label the run's own output was given for one baseline comparison.
///
/// The judge is never told which side is the reference, because a judge that knows
/// which output is the incumbent is answering a different question from the one the
/// case wrote down. Which label the run gets is fixed by the case and the criterion,
/// the same deterministic rule `compare` already uses to debias its pairs, so
/// re-grading a report asks the judge the question it asked before rather than its
/// mirror image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BaselinePairing {
    run: BlindLabel,
}

impl BaselinePairing {
    /// Each side is held to the same share of the payload rather than letting a
    /// short output pass its unused share to the other, which is what
    /// `PayloadBudget` does for a list of artifacts. Rolling the leftover across
    /// would let the judge read one side whole and the other clipped, and "at least
    /// as good as" answered against a clipped reference compares two different
    /// things.
    const SIDE_BYTES: usize = PayloadBudget::TOTAL_BYTES / 2;

    fn for_criterion(eval_case_id: &str, criterion: &str) -> Self {
        let run = if shuffle_swap(&format!("{eval_case_id}:{criterion}")) {
            BlindLabel::B
        } else {
            BlindLabel::A
        };
        Self { run }
    }

    fn reference(self) -> BlindLabel {
        match self.run {
            BlindLabel::A => BlindLabel::B,
            BlindLabel::B => BlindLabel::A,
        }
    }

    fn outputs(self, run: &str, reference: &str, direction: TruncateDirection) -> BTreeMap<&'static str, String> {
        BTreeMap::from([
            (self.run.as_str(), Self::fit(run, direction)),
            (self.reference().as_str(), Self::fit(reference, direction)),
        ])
    }

    fn fit(text: &str, direction: TruncateDirection) -> String {
        if text.len() <= Self::SIDE_BYTES {
            return text.to_string();
        }
        truncate(text, Self::SIDE_BYTES, direction)
    }

    /// A tie passes. "At least as good as" is the whole of what a baseline asks, so
    /// a run that matches the reference has met it; only a run the judge puts behind
    /// the reference has not.
    fn at_least_as_good(self, winner: ComparisonWinner) -> bool {
        match winner {
            ComparisonWinner::Tie => true,
            ComparisonWinner::A => self.run == BlindLabel::A,
            ComparisonWinner::B => self.run == BlindLabel::B,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BaselineJudgeResponse {
    better: String,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

/// Grade a run against a reference output the suite already accepts.
///
/// A missing or unreadable target is settled here rather than sent to the judge:
/// a run that produced nothing is not at least as good as a reference that exists,
/// and there is nothing for a judge to read but the absence.
#[allow(clippy::too_many_arguments)]
fn grade_against_baseline(
    assertion: &str,
    criterion: &str,
    target: &GradeTarget,
    reference: &BaselineReference,
    eval_case: &EvalCase,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    session: &GradeSession,
) -> Result<AssertionGradeResult> {
    let run_output = match declarative.input(ctx).target_content(target) {
        TargetContent::Text(text) => text,
        TargetContent::Missing(reason) => return Ok(settled_without_a_judge(assertion, reason)),
        TargetContent::Image { .. } => {
            return Ok(settled_without_a_judge(
                assertion,
                format!("{target} is an image, and a baseline reference is compared as text"),
            ))
        }
    };

    // A blank reference is refused where it is read, and a blank run has to fall at the
    // same line. `final_text` reads a missing `final.md` as an empty string rather than
    // as a missing target, so without this a run that produced nothing reaches the judge
    // and passes on a tie against a reference it never answered.
    if run_output.trim().is_empty() {
        return Ok(settled_without_a_judge(
            assertion,
            format!("{target} is empty, and nothing is not at least as good as a baseline"),
        ));
    }

    let direction = match target {
        GradeTarget::Transcript => TruncateDirection::Tail,
        _ => TruncateDirection::Head,
    };
    let pairing = BaselinePairing::for_criterion(eval_case.id.as_str(), criterion);
    #[derive(Serialize)]
    struct BaselinePayload<'a> {
        criterion: &'a str,
        outputs: BTreeMap<&'static str, String>,
    }
    let payload = serde_json::to_string(&BaselinePayload {
        criterion,
        outputs: pairing.outputs(&run_output, reference.text(), direction),
    })?;

    let endpoint = session.endpoint()?;
    let request = JudgeRequest::new(BASELINE_GRADER_SYSTEM_PROMPT, payload);

    let votes = session.options.grader_votes;
    let mut opinions = Vec::with_capacity(votes.count() as usize);
    for _ in votes.ballots() {
        let reply = judge::judge(endpoint, &request, "--grader-model")?;
        let parsed: BaselineJudgeResponse = judge::parse_json_reply(&reply.content, "--grader-model")?;
        let winner = parse_winner(&parsed.better)?;
        opinions.push((pairing.at_least_as_good(winner), parsed));
    }

    let verdict = tally_opinions(opinions).ok_or_else(|| {
        EvalError::Validation(
            ValidationError::for_field("--grader-votes", "no judge opinion was taken for this comparison").into(),
        )
    })?;

    let passed = verdict.tally.majority_passed();
    let standing = if passed {
        "is at least as good as"
    } else {
        "falls short of"
    };
    Ok(AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed,
        evidence: format!(
            "the run {standing} baseline '{}': {}",
            reference.declared(),
            verdict.opinion.evidence
        ),
        grader: GraderInfo {
            kind: GraderKind::Llm,
            model: Some(endpoint.model.to_string()),
            command: None,
        },
        rationale: verdict.opinion.rationale,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: (!votes.is_single()).then_some(verdict.tally),
        weight: None,
    })
}

fn settled_without_a_judge(assertion: &str, evidence: impl Into<String>) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed: false,
        evidence: evidence.into(),
        grader: GraderInfo {
            kind: GraderKind::Declarative,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    }
}

/// Total bytes of content one judge request may carry, across every artifact
/// placed into it.
///
/// Previously each artifact (the final text, and separately every output file)
/// got its own 8,000-byte excerpt with no cap on how many artifacts a run could
/// contribute, so a run with many output files could inflate one assertion into
/// an unbounded request. The cap now applies once, to the request as a whole.
#[derive(Debug, Clone, Copy)]
struct PayloadBudget {
    remaining_bytes: usize,
    remaining_slots: usize,
}

impl PayloadBudget {
    const TOTAL_BYTES: usize = 8_000;

    /// What is left of the request once `brief` has claimed the bytes it always keeps,
    /// for the run's own artifacts to share.
    fn after(brief: &CaseBrief, slots: usize) -> Self {
        Self::with_bytes(Self::TOTAL_BYTES - brief.reserved_bytes(), slots)
    }

    fn with_bytes(bytes: usize, slots: usize) -> Self {
        Self {
            remaining_bytes: bytes,
            remaining_slots: slots.max(1),
        }
    }

    /// Each remaining artifact claims an equal share of what is left, so one
    /// large early artifact does not starve every artifact placed after it,
    /// and a share an artifact does not need rolls forward to the rest.
    fn place(&mut self, text: &str, direction: TruncateDirection) -> Placement {
        let share = self.remaining_bytes / self.remaining_slots.max(1);
        self.remaining_slots = self.remaining_slots.saturating_sub(1);

        if share == 0 {
            return Placement::Omitted;
        }
        if text.len() <= share {
            self.remaining_bytes -= text.len();
            return Placement::Whole(text.to_string());
        }
        self.remaining_bytes -= share;
        Placement::Truncated(truncate(text, share, direction))
    }
}

/// What the case asked and the answer its author verified, handed to the judge alongside
/// the run's material.
///
/// A judge shown only the run's output grades whether the output sounds right, not whether
/// it answers the question correctly, and passes a confident wrong answer for it.
struct CaseBrief<'a> {
    prompt: &'a str,
    expected_output: &'a str,
}

impl<'a> CaseBrief<'a> {
    /// What each part of the brief keeps however much of the request the run's own
    /// artifacts want. The run's answer is what is being graded, so it has first claim on
    /// the rest, and the brief is what gets truncated when the request runs out.
    const FLOOR_BYTES: usize = 1_000;

    fn of(case: &'a EvalCase) -> Self {
        Self {
            prompt: case.prompt.as_str(),
            expected_output: case.expected_output.as_str(),
        }
    }

    fn reserved_bytes(&self) -> usize {
        self.prompt.len().min(Self::FLOOR_BYTES) + self.expected_output.len().min(Self::FLOOR_BYTES)
    }

    /// Writes the brief into `payload` out of whatever `artifacts` left unspent.
    ///
    /// `expected_output` is placed before `prompt`, since a reference is what a judge
    /// cannot grade correctness without, and a question is usually short enough to fit.
    fn place_into(&self, payload: &mut serde_json::Value, artifacts: PayloadBudget) {
        let remaining = artifacts.remaining_bytes + self.reserved_bytes();
        let reference_cap = remaining - self.prompt.len().min(Self::FLOOR_BYTES);
        let reference_spent = self.expected_output.len().min(reference_cap);
        payload["expected_output"] = serde_json::Value::String(Self::clip(self.expected_output, reference_cap));
        payload["prompt"] = serde_json::Value::String(Self::clip(self.prompt, remaining - reference_spent));
    }

    fn clip(text: &str, cap: usize) -> String {
        if text.len() <= cap {
            text.to_string()
        } else {
            truncate(text, cap, TruncateDirection::Head)
        }
    }
}

enum Placement {
    Whole(String),
    Truncated(String),
    Omitted,
}

#[derive(Debug, Clone, Copy)]
enum TruncateDirection {
    Head,
    Tail,
}

fn truncate(text: &str, cap: usize, direction: TruncateDirection) -> String {
    match direction {
        TruncateDirection::Head => {
            let mut end = cap;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}\n[truncated after {end} bytes]", &text[..end])
        }
        // A transcript's interesting part is usually its end (the final tool
        // calls and the answer the run converged on), not its start, so the
        // judge keeps the tail rather than the head.
        TruncateDirection::Tail => truncate_transcript_tail(text, cap),
    }
}

fn truncate_bytes_from_tail(text: &str, cap: usize) -> String {
    let mut start = text.len().saturating_sub(cap);
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let shown = text.len() - start;
    format!(
        "[showing the last {shown} bytes; earlier content truncated]\n{}",
        &text[start..]
    )
}

/// A flat byte cut lands wherever the budget runs out, which can fall inside
/// the one message a criterion is about and gives no sign that anything was
/// removed. A transcript is line-delimited NDJSON, one message per line, so
/// the cut can instead fall between messages: this keeps whole messages from
/// the end backward until the budget is spent, keeps the first message too
/// when room remains, and names what sat between them rather than
/// discarding it silently.
fn truncate_transcript_tail(text: &str, cap: usize) -> String {
    let messages: Vec<&str> = text.split('\n').filter(|line| !line.is_empty()).collect();
    if messages.len() < 2 {
        return truncate_bytes_from_tail(text, cap);
    }

    let mut used = 0usize;
    let mut first_kept = messages.len();
    for (index, message) in messages.iter().enumerate().rev() {
        // The join puts a newline *between* messages, so the last one kept carries
        // none of its own. Charging it one spends a byte the output never writes,
        // and a transcript over the cap by nothing more than that loses a whole
        // message to pay for it.
        let joined = match index == messages.len() - 1 {
            true => message.len(),
            false => message.len() + 1,
        };
        if used + joined > cap {
            break;
        }
        used += joined;
        first_kept = index;
    }

    if first_kept == messages.len() {
        return truncate_bytes_from_tail(messages[messages.len() - 1], cap);
    }
    if first_kept == 0 {
        return messages.join("\n");
    }

    let tail = messages[first_kept..].join("\n");
    let first = messages[0];
    let remaining = cap.saturating_sub(used);

    // Strictly less: what is left has to cover the newline that joins the first
    // message to what follows, not only the message itself.
    if first.len() < remaining {
        let hidden = first_kept - 1;
        if hidden == 0 {
            format!("{first}\n{tail}")
        } else {
            format!("{first}\n[{hidden} message(s) omitted from the middle of the transcript]\n{tail}")
        }
    } else {
        format!("[{first_kept} message(s) omitted from the start of the transcript]\n{tail}")
    }
}

/// What a judge request needs beyond a system prompt: the text payload, and,
/// when a target resolves to a picture, the attachment that carries it.
///
/// A bare `String` cannot also carry an optional image without either
/// smuggling it into the text (which is exactly the shape mismatch a vision
/// content part exists to avoid) or falling back to a tuple that leaves the
/// pairing anonymous at every call site.
struct JudgePayload {
    text: String,
    image: Option<judge::ImageAttachment>,
}

impl JudgePayload {
    fn text_only(text: String) -> Self {
        Self { text, image: None }
    }
}

fn llm_grader_payload(
    assertion: &str,
    target: &GradeTarget,
    brief: &CaseBrief,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
) -> Result<JudgePayload> {
    match target {
        GradeTarget::FinalText => {
            structured_payload(assertion, brief, declarative, ctx, OutputScope::TopLevel).map(JudgePayload::text_only)
        }
        GradeTarget::AnyOutput => {
            structured_payload(assertion, brief, declarative, ctx, OutputScope::Nested).map(JudgePayload::text_only)
        }
        GradeTarget::Transcript
        | GradeTarget::File(_)
        | GradeTarget::CreatedFiles
        | GradeTarget::Files(_)
        | GradeTarget::MockCalls => single_target_payload(assertion, target, brief, declarative, ctx),
    }
}

/// How far under `outputs/` a structured payload looks for files to hand the
/// judge alongside the final text.
///
/// `final_text` (declared or defaulted) keeps the pre-target behavior of
/// looking only at what sits directly in `outputs/`, unchanged so a suite
/// written before targets existed still grades the same way. `any_output`
/// means the same thing here that it means to the mechanical grader, which
/// walks every file the run produced, nested directories included.
enum OutputScope {
    TopLevel,
    Nested,
}

fn collect_output_files(outputs_dir: &Path, scope: OutputScope) -> Vec<(String, String)> {
    let mut output_files = Vec::new();
    match scope {
        OutputScope::TopLevel => {
            if let Ok(entries) = std::fs::read_dir(outputs_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name == FINAL_MD {
                        continue;
                    }
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        output_files.push((name, text));
                    }
                }
            }
        }
        OutputScope::Nested => collect_output_files_nested(outputs_dir, outputs_dir, &mut output_files),
    }
    output_files
}

fn collect_output_files_nested(root: &Path, dir: &Path, output_files: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_output_files_nested(root, &path, output_files);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let name = relative_slash_path(root, &path);
        if name == FINAL_MD {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            output_files.push((name, text));
        }
    }
}

/// A path relative to `root`, rendered with `/` separators regardless of
/// platform, so a nested output's key in the judge payload matches what an
/// author would write in an assertion.
fn relative_slash_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The payload shape for a judge looking at the run's final text plus its output
/// files, the shape a suite that does not declare a `target` gets.
fn structured_payload(
    assertion: &str,
    brief: &CaseBrief,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
    scope: OutputScope,
) -> Result<String> {
    let mut output_files = collect_output_files(&ctx.outputs_dir, scope);
    output_files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut budget = PayloadBudget::after(brief, 1 + output_files.len());
    let mut omitted = 0usize;

    let final_text = match budget.place(&declarative.final_text, TruncateDirection::Head) {
        Placement::Whole(text) | Placement::Truncated(text) => text,
        Placement::Omitted => {
            omitted += 1;
            String::new()
        }
    };

    let mut outputs = serde_json::Map::new();
    for (name, text) in output_files {
        match budget.place(&text, TruncateDirection::Head) {
            Placement::Whole(text) | Placement::Truncated(text) => {
                outputs.insert(name, serde_json::Value::String(text));
            }
            Placement::Omitted => omitted += 1,
        }
    }

    #[derive(Serialize)]
    struct LegacyJudgePayload<'a> {
        assertion: &'a str,
        final_text: String,
        outputs: serde_json::Map<String, serde_json::Value>,
    }
    let mut payload = serde_json::to_value(LegacyJudgePayload {
        assertion,
        final_text,
        outputs,
    })?;
    if omitted > 0 {
        payload["artifacts_omitted"] =
            serde_json::Value::String(format!("{omitted} artifact(s) omitted: judge payload budget exhausted"));
    }
    brief.place_into(&mut payload, budget);

    Ok(serde_json::to_string(&payload)?)
}

/// The payload shape for a judge aimed at one specific target. Self-describing
/// (it names the target) since, unlike the legacy shape, the judge is not
/// implicitly looking at "the run's output" but at whatever was declared.
fn single_target_payload(
    assertion: &str,
    target: &GradeTarget,
    brief: &CaseBrief,
    declarative: &DeclarativeContext,
    ctx: &RunContext,
) -> Result<JudgePayload> {
    let content = declarative.input(ctx).target_content(target);
    let direction = match target {
        GradeTarget::Transcript => TruncateDirection::Tail,
        _ => TruncateDirection::Head,
    };

    #[derive(Serialize)]
    struct TargetPayload<'a> {
        assertion: &'a str,
        target: String,
    }
    let mut payload = serde_json::to_value(TargetPayload {
        assertion,
        target: target.to_string(),
    })?;
    let mut image = None;
    let mut budget = PayloadBudget::after(brief, 1);

    match content {
        TargetContent::Text(text) => match budget.place(&text, direction) {
            Placement::Whole(text) | Placement::Truncated(text) => {
                payload["content"] = serde_json::Value::String(text);
            }
            Placement::Omitted => {
                payload["artifacts_omitted"] =
                    serde_json::Value::String("1 artifact(s) omitted: judge payload budget exhausted".to_string());
            }
        },
        TargetContent::Image { media_type, bytes } => {
            payload["content"] = serde_json::Value::String("image attached separately".to_string());
            image = Some(judge::ImageAttachment::new(media_type, &bytes));
        }
        TargetContent::Missing(reason) => {
            payload["missing_reason"] = serde_json::Value::String(reason);
        }
    }
    brief.place_into(&mut payload, budget);

    Ok(JudgePayload {
        text: serde_json::to_string(&payload)?,
        image,
    })
}

#[derive(Debug, Deserialize)]
struct LlmGraderResponse {
    passed: bool,
    evidence: String,
    #[serde(default)]
    rationale: Option<String>,
}

pub fn build_grading_file(assertion_results: Vec<AssertionGradeResult>) -> Result<GradingFile> {
    let summary = GradingCounts::tally(&assertion_results).summary();

    Ok(GradingFile {
        assertion_results,
        summary,
    })
}

pub fn evidence_is_trivial(assertion: &str, evidence: &str) -> bool {
    let a = normalize_for_compare(assertion);
    let e = normalize_for_compare(evidence);
    e.is_empty() || e == a || a.contains(&e) && e.len() > a.len() / 2
}

fn normalize_for_compare(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn validate_grading_document(grading: &GradingFile, strict: bool) -> Result<()> {
    let mut errors = ValidationErrors::new();

    if grading.assertion_results.is_empty() {
        errors.push(ValidationError::for_field(
            "assertion_results",
            "must contain at least one result",
        ));
    }

    let counts = GradingCounts::tally(&grading.assertion_results);
    let passed = counts.passed;
    let failed = counts.failed;

    for (index, result) in grading.assertion_results.iter().enumerate() {
        if result.assertion.trim().is_empty() {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}].assertion"),
                "must be a non-empty string",
            ));
        }
        if result.evidence.trim().is_empty() {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}].evidence"),
                "must be a non-empty string",
            ));
        }
        if result.passed && result.is_scored() && evidence_is_trivial(&result.assertion, &result.evidence) {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}].evidence"),
                "passed assertions must include non-trivial evidence",
            ));
        }
        if strict && result.grader.kind == GraderKind::NeedsLlm {
            errors.push(ValidationError::for_field(
                format!("assertion_results[{index}]"),
                "requires LLM grading in strict mode",
            ));
        }
    }

    if grading.summary.passed != passed {
        errors.push(ValidationError::for_field(
            "summary.passed",
            format!("{} does not match {passed} passed results", grading.summary.passed),
        ));
    }
    if grading.summary.failed != failed {
        errors.push(ValidationError::for_field(
            "summary.failed",
            format!("{} does not match {failed} failed results", grading.summary.failed),
        ));
    }
    if grading.summary.unsupported != counts.unsupported {
        errors.push(ValidationError::for_field(
            "summary.unsupported",
            format!(
                "{} does not match {} unsupported results",
                grading.summary.unsupported, counts.unsupported
            ),
        ));
    }
    if grading.summary.excluded != counts.excluded {
        errors.push(ValidationError::for_field(
            "summary.excluded",
            format!(
                "{} does not match {} excluded results",
                grading.summary.excluded, counts.excluded
            ),
        ));
    }
    if grading.summary.ungraded != counts.ungraded {
        errors.push(ValidationError::for_field(
            "summary.ungraded",
            format!(
                "{} does not match {} ungraded results",
                grading.summary.ungraded, counts.ungraded
            ),
        ));
    }
    if grading.summary.total != grading.assertion_results.len() {
        errors.push(ValidationError::for_field(
            "summary.total",
            format!(
                "{} does not match {} results",
                grading.summary.total,
                grading.assertion_results.len()
            ),
        ));
    }

    if !pass_rate_matches(grading.summary.pass_rate, counts.pass_rate()) {
        errors.push(ValidationError::for_field(
            "summary.pass_rate",
            format!(
                "{} does not match computed rate {}",
                describe_pass_rate(grading.summary.pass_rate),
                describe_pass_rate(counts.pass_rate())
            ),
        ));
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    Ok(())
}

fn store_grader_artifacts(
    run: &mut RunRecord,
    report_dir: &Path,
    ctx: &RunContext,
    options: &GradeOptions,
    grading: &GradingFile,
) -> Result<()> {
    let grading_relative = ctx
        .run_dir
        .join("grading.json")
        .strip_prefix(report_dir)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "grading.json".to_string());

    run.artifacts.retain(|artifact| {
        artifact
            .get("kind")
            .and_then(|value| value.as_str())
            .is_none_or(|kind| kind != "grading" && kind != "grader_result")
    });

    run.artifacts.push(super::report::artifact("grading", grading_relative));

    if options.grader == GraderMode::Script {
        let script_result = ctx.run_dir.join("grader-script-result.json");
        if script_result.is_file() {
            let relative = script_result
                .strip_prefix(report_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| script_result.display().to_string());
            run.artifacts.push(super::report::artifact("grader_result", relative));
        }
    }

    let _ = grading;
    Ok(())
}

/// A graded assertion as it appears in `report.json`: the result exactly as its own run wrote
/// it, plus the two keys that only the run knows. Flattening rather than listing the fields by
/// hand is what keeps a field added to `AssertionGradeResult` later from silently missing the
/// published report the way `name`, `excluded`, `rationale` and `votes` once did.
#[derive(Serialize)]
struct PublishedAssertionResult<'a> {
    run_id: &'a str,
    eval_case_id: &'a str,
    #[serde(flatten)]
    result: &'a AssertionGradeResult,
}

fn update_report_after_grading(
    document: &mut ReportDocument,
    runs: &[RunRecord],
    report_dir: &Path,
    grading_strategy: &GradingStrategy,
) -> Result<()> {
    document.assertion_results.clear();
    if !document.dimensions.grading_strategies.contains(grading_strategy) {
        document.dimensions.grading_strategies.push(grading_strategy.clone());
    }

    for run in runs {
        let run_dir = report_dir
            .join(&run.paths.workspace)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| report_dir.to_path_buf());
        let grading_path = run_dir.join("grading.json");
        if !grading_path.is_file() {
            continue;
        }
        let grading: GradingFile = serde_json::from_str(&std::fs::read_to_string(&grading_path)?)?;
        for result in &grading.assertion_results {
            document
                .assertion_results
                .push(serde_json::to_value(PublishedAssertionResult {
                    run_id: &run.id,
                    eval_case_id: &run.eval_case_id,
                    result,
                })?);
        }
    }

    recompute_scenario_summaries(document);

    Ok(())
}

#[cfg(test)]
#[path = "grading_tests.rs"]
mod tests;
