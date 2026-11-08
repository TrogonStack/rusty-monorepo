//! Rebuilds the trace a live `eval run` would have exported from a finished report
//! bundle, for a pass that ran with tracing off or whose trace the backend no longer has.
//!
//! Spans are built with the OpenTelemetry API rather than `tracing`, because only the
//! API lets a span start and end at a recorded time instead of the moment it is created.

mod constants;
use constants::RUN_FAILED;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use clap::ValueEnum;
use opentelemetry::trace::{Link, SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use opentelemetry_semantic_conventions::attribute::{ERROR_TYPE, PROCESS_EXECUTABLE_NAME, PROCESS_EXIT_CODE};
use serde::{Deserialize, Serialize};

use super::budget::RunCost;
use super::cache::runner_kind_label;
use super::eval_telemetry::AttemptOutcome;
use super::evals::{EvalError, Result};
use super::grading::telemetry::{Evaluation, Provenance, CASE_SCORE_EVALUATION};
use super::grading::AssertionGradeResult;
use super::report::{GraderChoice, ReportDocument, ReportSection, RunRecord};
use super::runner::telemetry::FailureType;
use super::runner::{Runner, FAILURE_KIND_RUNNER};
use super::span_reference::SpanReference;
use super::validation::{ValidationError, ValidationErrors};
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, GEN_AI_AGENT_NAME, GEN_AI_OPERATION_NAME, GEN_AI_PROVIDER_NAME, GEN_AI_REQUEST_MODEL,
    GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS, GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS, GEN_AI_USAGE_INPUT_TOKENS,
    GEN_AI_USAGE_OUTPUT_TOKENS, GEN_AI_WORKFLOW_NAME,
};
use crate::telemetry::semconv::trg::{
    EVAL_ATTEMPT, EVAL_ATTEMPT_OUTCOME, EVAL_ATTEMPT_TRANSIENT, EVAL_CACHE_HIT, EVAL_CASE_ID, EVAL_CASE_SPLIT,
    EVAL_COST_USD, EVAL_GRADER_KIND, EVAL_GRADER_NAME, EVAL_GRADING_STRATEGY, EVAL_ITERATION, EVAL_MODEL_CONFIG,
    EVAL_REPLAYED, EVAL_REPORT_ID, EVAL_RUNNER_KIND, EVAL_RUNNER_MODEL, EVAL_RUNNER_VERSION, EVAL_RUN_COUNT,
    EVAL_RUN_ID, EVAL_RUN_STATUS, EVAL_SCENARIO, EVAL_SCENARIOS, EVAL_SKILL_REVISION, EVAL_TOOL_GRANT,
};
use crate::telemetry::ContentCapture;

/// What to do with a run whose live span the report already points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracedRuns {
    /// Leave it out, so a backend that has the live trace does not count its usage and
    /// cost twice.
    Skip,
    /// Rebuild it anyway, linked to the live span it duplicates.
    Replay,
}

impl TracedRuns {
    fn replays(self, run: &RunRecord) -> bool {
        self == Self::Replay || run.trace.is_none()
    }

    fn replays_grade(self, run: &RunRecord) -> bool {
        self == Self::Replay || run.grade_trace.is_none()
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        options_struct_without_default,
        reason = "Replay requires an explicit content-capture and trace replay policy; an implicit default could export content or duplicate traces"
    )
)]
pub struct ReplayOptions {
    pub traced_runs: TracedRuns,
    pub content: ContentCapture,
    /// The span of the command doing the export, which the replayed suite links to.
    pub exported_from: Option<SpanReference>,
}

/// A run rebuilt under a replayed suite, and where its run span landed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplayedRun {
    pub run_id: String,
    pub span: SpanReference,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Replay {
    Exported {
        /// Absent when every run was traced live and only their grading was replayed.
        suite: Option<SpanReference>,
        runs: Vec<ReplayedRun>,
        skipped_runs: Vec<String>,
        /// Runs traced live whose grading never was, replayed into the live run's trace;
        /// `span` is where the replayed `grade run` landed.
        live_run_grades: Vec<ReplayedRun>,
        /// Runs whose grading already has a live trace, so their evaluation events are
        /// not sent a second time.
        skipped_grades: Vec<String>,
    },
    /// Every run and every grade already has a live trace, or the report has no runs.
    NothingToReplay {
        skipped_runs: Vec<String>,
        skipped_grades: Vec<String>,
    },
    /// No trace exporter is configured, so nothing was sent anywhere.
    TracingOff,
}

/// One entry of `report.json`'s `assertion_results`.
#[derive(Deserialize)]
struct RecordedAssertion {
    run_id: String,
    #[serde(flatten)]
    result: AssertionGradeResult,
}

/// When the pass started, read from `report.generated_at`, which the bundle is stamped
/// with before any run executes. Runs record a duration but no start of their own.
#[derive(Debug, Clone, Copy)]
struct Timeline {
    started: SystemTime,
}

impl Timeline {
    fn of(report: &ReportSection) -> Result<Self> {
        let started = chrono::DateTime::parse_from_rfc3339(&report.generated_at).map_err(|error| {
            EvalError::Validation(ValidationErrors::from(ValidationError::for_field(
                "report.generated_at",
                format!("`{}` is not an RFC 3339 timestamp: {error}", report.generated_at),
            )))
        })?;
        Ok(Self {
            started: started.into(),
        })
    }

    fn run_end(self, run: &RunRecord) -> SystemTime {
        run.metrics
            .duration_ms
            .and_then(|ms| self.started.checked_add(Duration::from_millis(ms)))
            .unwrap_or(self.started)
    }
}

/// Rebuilds `document` as spans on `tracer` and evaluation events through the logs
/// bridge, every one of them marked `trg.eval.replayed`.
pub fn replay<T>(tracer: &T, document: &ReportDocument, options: &ReplayOptions) -> Result<Replay>
where
    T: Tracer,
    T::Span: Send + Sync + 'static,
{
    let timeline = Timeline::of(&document.report)?;
    let assertions = recorded_assertions(document)?;
    let was_graded = |run: &RunRecord| was_graded(run, &assertions);
    let (runs, skipped): (Vec<&RunRecord>, Vec<&RunRecord>) =
        document.runs.iter().partition(|run| options.traced_runs.replays(run));
    let skipped_runs: Vec<String> = skipped.iter().map(|run| run.id.clone()).collect();
    let skipped_grades: Vec<String> = document
        .runs
        .iter()
        .filter(|run| was_graded(run) && !options.traced_runs.replays_grade(run))
        .map(|run| run.id.clone())
        .collect();
    let grades_into_live_runs: Vec<(&RunRecord, &SpanReference)> = skipped
        .iter()
        .filter(|run| was_graded(run) && options.traced_runs.replays_grade(run))
        .filter_map(|run| run.span_reference().map(|live| (*run, live)))
        .collect();
    if runs.is_empty() && grades_into_live_runs.is_empty() {
        return Ok(Replay::NothingToReplay {
            skipped_runs,
            skipped_grades,
        });
    }

    let spans = Spans { tracer };
    let suite_reference = if runs.is_empty() {
        None
    } else {
        let Some(suite_reference) = replay_suite(&spans, document, options, &runs, &assertions, timeline) else {
            return Ok(Replay::TracingOff);
        };
        Some(suite_reference)
    };

    let mut live_run_grades = Vec::with_capacity(grades_into_live_runs.len());
    for (run, live) in grades_into_live_runs {
        let results = assertions.get(run.id.as_str()).map_or(&[][..], Vec::as_slice);
        let home = GradeHome::LiveRun(live);
        let Some(span) = replay_grading(&spans, home, run, results, options.content, timeline.run_end(run)) else {
            return Ok(Replay::TracingOff);
        };
        live_run_grades.push(ReplayedRun {
            run_id: run.id.clone(),
            span,
        });
    }

    let (suite, runs) = suite_reference.unzip();
    Ok(Replay::Exported {
        suite,
        runs: runs.unwrap_or_default(),
        skipped_runs,
        live_run_grades,
        skipped_grades,
    })
}

/// The replayed suite for the runs being rebuilt, with their grading, or `None` when no
/// trace exporter gave the suite span an identity.
fn replay_suite<T>(
    spans: &Spans<'_, T>,
    document: &ReportDocument,
    options: &ReplayOptions,
    runs: &[&RunRecord],
    assertions: &BTreeMap<&str, Vec<AssertionGradeResult>>,
    timeline: Timeline,
) -> Option<(SpanReference, Vec<ReplayedRun>)>
where
    T: Tracer,
    T::Span: Send + Sync + 'static,
{
    let suite = spans.start(&Context::new(), suite_span(document, options), timeline.started);
    let suite_reference = SpanReference::of_context(suite.span().span_context())?;
    let finished = runs
        .iter()
        .map(|run| timeline.run_end(run))
        .max()
        .unwrap_or(timeline.started);

    let execute = spans.start(
        &suite,
        Blueprint::new("execute runs").with(KeyValue::new(
            EVAL_RUN_COUNT,
            i64::try_from(document.runs.len()).unwrap_or(i64::MAX),
        )),
        timeline.started,
    );
    let runner = document
        .report
        .runner
        .as_deref()
        .and_then(|name| <Runner as ValueEnum>::from_str(name, true).ok());
    let mut replayed = Vec::with_capacity(runs.len());
    for run in runs {
        let span = replay_run(spans, &execute, run, &document.report, runner, timeline);
        replayed.push((*run, span));
    }
    execute.span().end_with_timestamp(finished);

    let graded: Vec<_> = replayed
        .iter()
        .filter(|(run, _)| was_graded(run, assertions) && options.traced_runs.replays_grade(run))
        .collect();
    if !graded.is_empty() {
        let grade = spans.start(&suite, Blueprint::new("grade"), finished);
        for (run, span) in graded {
            let results = assertions.get(run.id.as_str()).map_or(&[][..], Vec::as_slice);
            let home = GradeHome::ReplayedRun {
                grade: &grade,
                run: *span,
            };
            replay_grading(spans, home, run, results, options.content, finished);
        }
        grade.span().end_with_timestamp(finished);
    }
    suite.span().end_with_timestamp(finished);

    let runs = replayed
        .into_iter()
        .map(|(run, span)| ReplayedRun {
            run_id: run.id.clone(),
            span,
        })
        .collect();
    Some((suite_reference, runs))
}

fn was_graded(run: &RunRecord, assertions: &BTreeMap<&str, Vec<AssertionGradeResult>>) -> bool {
    assertions.contains_key(run.id.as_str()) || run.case_score.is_some()
}

fn recorded_assertions(document: &ReportDocument) -> Result<BTreeMap<&str, Vec<AssertionGradeResult>>> {
    let mut by_run: BTreeMap<&str, Vec<AssertionGradeResult>> = BTreeMap::new();
    for (index, value) in document.assertion_results.iter().enumerate() {
        let recorded = RecordedAssertion::deserialize(value).map_err(|error| {
            EvalError::Validation(ValidationErrors::from(ValidationError::for_field(
                format!("assertion_results[{index}]"),
                error.to_string(),
            )))
        })?;
        let Some(run) = document.runs.iter().find(|run| run.id == recorded.run_id) else {
            continue;
        };
        by_run.entry(run.id.as_str()).or_default().push(recorded.result);
    }
    Ok(by_run)
}

fn suite_span(document: &ReportDocument, options: &ReplayOptions) -> Blueprint {
    let skill = &document.suite.skill_name;
    let scenarios = document
        .dimensions
        .scenarios
        .iter()
        .map(|scenario| scenario.id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let strategy = document
        .dimensions
        .grading_strategies
        .last()
        .map_or("none", |strategy| grading_mode(&strategy.grader));
    Blueprint::new(format!("{} {skill}", gen_ai_operation_name::INVOKE_WORKFLOW))
        .with(KeyValue::new(
            GEN_AI_OPERATION_NAME,
            gen_ai_operation_name::INVOKE_WORKFLOW,
        ))
        .with(KeyValue::new(GEN_AI_WORKFLOW_NAME, skill.clone()))
        .with(KeyValue::new(EVAL_SKILL_REVISION, document.suite.skill_hash.clone()))
        .with(KeyValue::new(EVAL_ITERATION, i64::from(document.report.iteration)))
        .with(KeyValue::new(EVAL_SCENARIOS, scenarios))
        .with(KeyValue::new(EVAL_GRADING_STRATEGY, strategy))
        .with(KeyValue::new(EVAL_REPORT_ID, document.report.id.clone()))
        .linked_to(options.exported_from.as_ref())
        .linked_to(document.report.span_reference())
}

fn grading_mode(choice: &GraderChoice) -> &'static str {
    match choice {
        GraderChoice::Auto { .. } => "auto",
        GraderChoice::None => "none",
        GraderChoice::Llm { .. } => "llm",
        GraderChoice::Script { .. } => "script",
    }
}

fn replay_run<T>(
    spans: &Spans<'_, T>,
    execute: &Context,
    run: &RunRecord,
    report: &ReportSection,
    runner: Option<Runner>,
    timeline: Timeline,
) -> SpanReference
where
    T: Tracer,
    T::Span: Send + Sync + 'static,
{
    let mut blueprint = Blueprint::new("run")
        .with(KeyValue::new(EVAL_RUN_ID, run.id.clone()))
        .with(KeyValue::new(EVAL_CASE_ID, run.eval_case_id.clone()))
        .with(KeyValue::new(EVAL_CASE_SPLIT, run.split.as_str()))
        .with(KeyValue::new(EVAL_SCENARIO, run.scenario_id.as_str()))
        .with(KeyValue::new(EVAL_ITERATION, i64::from(run.iteration)))
        .with(KeyValue::new(EVAL_MODEL_CONFIG, run.model_config_id.clone()))
        .with(KeyValue::new(EVAL_CACHE_HIT, run.cache.is_some()))
        .with(KeyValue::new(EVAL_RUN_STATUS, run.status.clone()))
        .linked_to(run.span_reference());
    if let Some(runner) = runner {
        blueprint = blueprint.with(KeyValue::new(EVAL_RUNNER_KIND, runner_kind_label(runner)));
    }
    if let Some(version) = &report.runner_version {
        blueprint = blueprint.with(KeyValue::new(EVAL_RUNNER_VERSION, version.clone()));
    }
    if let Some(model) = &run.runner_model {
        blueprint = blueprint.with(KeyValue::new(EVAL_RUNNER_MODEL, model.as_str().to_string()));
    }
    if let Some(grant) = &run.tool_grant {
        blueprint = blueprint.with(KeyValue::new(EVAL_TOOL_GRANT, grant.to_string()));
    }
    if matches!(run.status.as_str(), "failed" | "timeout" | "budget_exhausted") {
        blueprint = blueprint.failed(run.failure_kind.clone().unwrap_or_else(|| RUN_FAILED.to_string()));
    }

    let ended = timeline.run_end(run);
    let span = spans.start(execute, blueprint, timeline.started);
    let reference = SpanReference::of_context(span.span().span_context())
        .expect("a run span shares the recording suite span's trace");
    if let Some(attempt) = AttemptRecord::of(run) {
        let attempt_span = spans.start(&span, attempt.blueprint(run), timeline.started);
        if attempt == AttemptRecord::Answered {
            spans
                .start(&attempt_span, agent_invocation(run, runner), timeline.started)
                .span()
                .end_with_timestamp(ended);
        }
        attempt_span.span().end_with_timestamp(ended);
    }
    span.span().end_with_timestamp(ended);
    reference
}

/// What the report shows of a run's final harness invocation. Earlier attempts a retry
/// replaced leave nothing behind, so only the last one can be rebuilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptRecord {
    /// The harness answered, and the run recorded what it reported.
    Answered,
    /// trg could not get an answer out of the harness at all.
    RunnerError,
}

impl AttemptRecord {
    fn of(run: &RunRecord) -> Option<Self> {
        if run.cache.is_some() {
            None
        } else if run.failure_kind.as_deref() == Some(FAILURE_KIND_RUNNER) {
            Some(Self::RunnerError)
        } else if run.metrics.duration_ms.is_some() {
            Some(Self::Answered)
        } else {
            None
        }
    }

    fn blueprint(self, run: &RunRecord) -> Blueprint {
        let blueprint = Blueprint::new("attempt").with(KeyValue::new(EVAL_ATTEMPT, i64::from(run.runner_invocations)));
        let outcome = match self {
            Self::Answered => AttemptOutcome::Completed,
            Self::RunnerError => AttemptOutcome::RunnerError,
        };
        let blueprint = blueprint
            .with(KeyValue::new(EVAL_ATTEMPT_OUTCOME, outcome.as_str()))
            .with(KeyValue::new(EVAL_ATTEMPT_TRANSIENT, outcome.is_transient()));
        match self {
            Self::Answered => blueprint,
            Self::RunnerError => blueprint.failed(outcome.as_str().to_string()),
        }
    }
}

fn agent_invocation(run: &RunRecord, runner: Option<Runner>) -> Blueprint {
    let operation = gen_ai_operation_name::INVOKE_AGENT;
    let name = runner.map_or_else(
        || operation.to_string(),
        |runner| format!("{operation} {}", runner.display_name()),
    );
    let mut blueprint = Blueprint::new(name)
        .kind(SpanKind::Client)
        .with(KeyValue::new(GEN_AI_OPERATION_NAME, operation));
    if let Some(runner) = runner {
        blueprint = blueprint
            .with(KeyValue::new(GEN_AI_PROVIDER_NAME, runner.gen_ai_provider()))
            .with(KeyValue::new(GEN_AI_AGENT_NAME, runner.display_name()))
            .with(KeyValue::new(PROCESS_EXECUTABLE_NAME, runner.program_name()));
    }
    if let Some(model) = &run.runner_model {
        blueprint = blueprint.with(KeyValue::new(GEN_AI_REQUEST_MODEL, model.as_str().to_string()));
    }
    let metrics = &run.metrics;
    let cached = metrics.cached_tokens;
    let selected_items = [
        (GEN_AI_USAGE_INPUT_TOKENS, metrics.input_tokens),
        (GEN_AI_USAGE_OUTPUT_TOKENS, metrics.output_tokens),
        (
            GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS,
            cached.and_then(|cached| cached.read_tokens()),
        ),
        (
            GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS,
            cached.and_then(|cached| cached.write_tokens()),
        ),
    ];
    for (key, value) in selected_items {
        if let Some(value) = value {
            blueprint = blueprint.with(KeyValue::new(key, i64::try_from(value).unwrap_or(i64::MAX)));
        }
    }
    if let Some(RunCost::Priced { usd }) = metrics.cost {
        blueprint = blueprint.with(KeyValue::new(EVAL_COST_USD, usd));
    }
    if let Some(code) = metrics.exit_code {
        blueprint = blueprint.with(KeyValue::new(PROCESS_EXIT_CODE, i64::from(code)));
    }
    let failure = match run.status.as_str() {
        "timeout" => Some(FailureType::Timeout),
        "budget_exhausted" => Some(FailureType::BudgetExhausted),
        "failed" => Some(FailureType::of_exit(metrics.exit_code).unwrap_or(FailureType::HarnessError)),
        _ => None,
    };
    match failure {
        Some(failure) => blueprint.failed(failure.as_str().to_string()),
        None => blueprint,
    }
}

/// Where a replayed `grade run` goes.
#[derive(Clone, Copy)]
enum GradeHome<'a> {
    /// Under the replayed suite's `grade`, for a run this export rebuilt.
    ReplayedRun { grade: &'a Context, run: SpanReference },
    /// Inside the live trace, as a child of the run span a live pass already exported.
    LiveRun(&'a SpanReference),
}

impl GradeHome<'_> {
    fn parent(self) -> Context {
        match self {
            Self::ReplayedRun { grade, .. } => grade.clone(),
            Self::LiveRun(live) => Context::new().with_remote_span_context(live.span_context()),
        }
    }

    fn run(self) -> SpanReference {
        match self {
            Self::ReplayedRun { run, .. } => run,
            Self::LiveRun(live) => *live,
        }
    }
}

/// `grade run` and one grader span per recorded result, all at `at` since a report
/// records no grading time, and the run's evaluation events in the run span's context,
/// as a live grade emits them. `None`, with no events sent, when no trace exporter gave
/// the grade span an identity.
fn replay_grading<T>(
    spans: &Spans<'_, T>,
    home: GradeHome<'_>,
    run: &RunRecord,
    results: &[AssertionGradeResult],
    content: ContentCapture,
    at: SystemTime,
) -> Option<SpanReference>
where
    T: Tracer,
    T::Span: Send + Sync + 'static,
{
    let mut blueprint = Blueprint::new("grade run")
        .with(KeyValue::new(EVAL_RUN_ID, run.id.clone()))
        .with(KeyValue::new(EVAL_CASE_ID, run.eval_case_id.clone()))
        .with(KeyValue::new(EVAL_CASE_SPLIT, run.split.as_str()))
        .with(KeyValue::new(EVAL_SCENARIO, run.scenario_id.as_str()))
        .with(KeyValue::new(EVAL_ITERATION, i64::from(run.iteration)))
        .linked_to(run.grade_span_reference());
    if let GradeHome::ReplayedRun { run: replayed, .. } = home {
        blueprint = blueprint.linked_to(Some(&replayed)).linked_to(run.span_reference());
    }
    let graded = spans.start(&home.parent(), blueprint, at);
    let Some(reference) = SpanReference::of_context(graded.span().span_context()) else {
        graded.span().end_with_timestamp(at);
        return None;
    };
    let replayed = &home.run();
    for result in results {
        let kind = result.grader.kind.as_str();
        let name = result.name.as_deref().unwrap_or(kind);
        spans
            .start(
                &graded,
                Blueprint::new(format!("grader {kind}"))
                    .with(KeyValue::new(EVAL_GRADER_NAME, name.to_string()))
                    .with(KeyValue::new(EVAL_GRADER_KIND, kind)),
                at,
            )
            .span()
            .end_with_timestamp(at);
        Evaluation {
            provenance: Provenance::Replayed,
            ..Evaluation::of_assertion(result, name, kind, &run.eval_case_id, &run.id)
        }
        .emit(Some(replayed), content);
    }
    if let Some(score) = run.case_score {
        Evaluation {
            name: CASE_SCORE_EVALUATION,
            label: None,
            score: Some(score),
            explanation: None,
            grader_kind: None,
            case_id: &run.eval_case_id,
            run_id: Some(&run.id),
            provenance: Provenance::Replayed,
        }
        .emit(Some(replayed), content);
    }
    graded.span().end_with_timestamp(at);
    Some(reference)
}

/// A span to start: everything but its times, which the caller reads off the report.
struct Blueprint {
    name: Cow<'static, str>,
    kind: SpanKind,
    attributes: Vec<KeyValue>,
    links: Vec<Link>,
    failure: Option<String>,
}

impl Blueprint {
    fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self {
            name: name.into(),
            kind: SpanKind::Internal,
            attributes: vec![KeyValue::new(EVAL_REPLAYED, true)],
            links: Vec::new(),
            failure: None,
        }
    }

    fn kind(mut self, kind: SpanKind) -> Self {
        self.kind = kind;
        self
    }

    fn with(mut self, attribute: KeyValue) -> Self {
        self.attributes.push(attribute);
        self
    }

    fn linked_to(mut self, reference: Option<&SpanReference>) -> Self {
        if let Some(reference) = reference {
            self.links.push(Link::with_context(reference.span_context()));
        }
        self
    }

    fn failed(mut self, error_type: String) -> Self {
        self.attributes.push(KeyValue::new(ERROR_TYPE, error_type.clone()));
        self.failure = Some(error_type);
        self
    }
}

struct Spans<'a, T> {
    tracer: &'a T,
}

impl<T> Spans<'_, T>
where
    T: Tracer,
    T::Span: Send + Sync + 'static,
{
    /// Starts `blueprint` under `parent` at `start`, handing back a context the caller
    /// parents children on and ends at the span's recorded end.
    fn start(&self, parent: &Context, blueprint: Blueprint, start: SystemTime) -> Context {
        let builder = self
            .tracer
            .span_builder(blueprint.name)
            .with_kind(blueprint.kind)
            .with_start_time(start)
            .with_attributes(blueprint.attributes)
            .with_links(blueprint.links);
        let span = self.tracer.build_with_context(builder, parent);
        let context = parent.with_span(span);
        if let Some(failure) = blueprint.failure {
            context.span().set_status(Status::error(failure));
        }
        context
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
