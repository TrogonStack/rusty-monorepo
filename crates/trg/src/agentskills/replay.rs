//! Rebuilds the trace a live `eval run` would have exported from a finished report
//! bundle, for a pass that ran with tracing off or whose trace the backend no longer has.
//!
//! Spans are built with the OpenTelemetry API rather than `tracing`, because only the
//! API lets a span start and end at a recorded time instead of the moment it is created.

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

/// `error.type` for a failed run that did not say which way, as a live pass records it.
const RUN_FAILED: &str = "run_failed";

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
    if run.status == "failed" {
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
        match self {
            Self::Answered => blueprint,
            Self::RunnerError => {
                let outcome = AttemptOutcome::RunnerError;
                blueprint
                    .with(KeyValue::new(EVAL_ATTEMPT_OUTCOME, outcome.as_str()))
                    .with(KeyValue::new(EVAL_ATTEMPT_TRANSIENT, outcome.is_transient()))
                    .failed(outcome.as_str().to_string())
            }
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
    for (key, value) in [
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
    ] {
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
mod tests {
    use std::time::{Duration, SystemTime};

    use opentelemetry::logs::AnyValue;
    use opentelemetry::trace::{SpanKind, Status, TracerProvider as _};
    use opentelemetry::Value;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLogRecord, SdkLoggerProvider};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use tracing_subscriber::prelude::*;

    use super::*;
    use crate::agentskills::cache::RunCacheInfo;
    use crate::agentskills::evals::{EvalDirName, EvalSplit};
    use crate::agentskills::grading::{GraderInfo, GraderKind};
    use crate::agentskills::model_name::ModelName;
    use crate::agentskills::report::{
        CacheTokens, DimensionsSection, GradingStrategy, ProducerSection, RunMetrics, RunPaths, ScenarioDimension,
        ScenarioKind, SuiteSection, SummariesSection,
    };
    use crate::telemetry::semconv::generated::attributes::{
        GEN_AI_EVALUATION_EXPLANATION, GEN_AI_EVALUATION_NAME, GEN_AI_EVALUATION_SCORE_VALUE,
    };
    use crate::telemetry::testing::CapturedTrace;

    const LIVE_SUITE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const LIVE_RUN_SPAN: &str = "00f067aa0ba902b7";
    const LIVE_GRADE_SPAN: &str = "53995c3f42cd8ad8";
    const LIVE_RUN_GRADE_SPAN: &str = "e457b5a2e4d86bd1";
    const EXPORT_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";
    const EXPORT_SPAN: &str = "b7ad6b7169203331";
    const CONTENT_ON: &[(&str, &str)] = &[
        ("OTEL_SEMCONV_STABILITY_OPT_IN", "gen_ai_latest_experimental"),
        ("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "EVENT_ONLY"),
    ];

    fn reference(trace_id: &str, span_id: &str) -> SpanReference {
        serde_json::from_value(serde_json::json!({ "trace_id": trace_id, "span_id": span_id })).unwrap()
    }

    fn generated_at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_779_796_800)
    }

    fn after(ms: u64) -> SystemTime {
        generated_at() + Duration::from_millis(ms)
    }

    fn run(id: &str, status: &str, duration_ms: Option<u64>) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            eval_case_id: format!("case-{id}"),
            eval_slug: "case".to_string(),
            split: EvalSplit::Test,
            scenario_id: ScenarioKind::WithSkill,
            iteration: 3,
            model_config_id: "default".to_string(),
            skill_revision_id: "current".to_string(),
            attempt: 1,
            status: status.to_string(),
            runner_invocations: 1,
            failure_kind: None,
            paths: RunPaths {
                workspace: format!("runs/{id}/workspace"),
                outputs: format!("runs/{id}/workspace/outputs"),
            },
            mirror_path: String::new(),
            runner_model: Some(ModelName::parse("claude-sonnet-4-5").unwrap()),
            runner_model_source: None,
            tool_grant: None,
            artifacts: Vec::new(),
            metrics: RunMetrics {
                duration_ms,
                exit_code: duration_ms.map(|_| 0),
                ..RunMetrics::default()
            },
            cache: None,
            skill_integrity: None,
            read_only_fixture_violations: Vec::new(),
            warnings: Vec::new(),
            mock_violations: Vec::new(),
            case_score: None,
            trace: None,
            grade_trace: None,
        }
    }

    fn assertion(run_id: &str, kind: GraderKind, passed: bool, evidence: &str) -> serde_json::Value {
        serde_json::json!({
            "run_id": run_id,
            "eval_case_id": format!("case-{run_id}"),
            "assertion": "the output names the file",
            "passed": passed,
            "evidence": evidence,
            "grader": GraderInfo { kind, model: None, command: None },
        })
    }

    /// A finished pass: a priced completed run, a failed run, a cache hit, a run the
    /// harness never answered, and a run that was already traced and graded live.
    fn bundle() -> ReportDocument {
        let mut priced = run("run-001", "completed", Some(4_000));
        priced.metrics.input_tokens = Some(120);
        priced.metrics.output_tokens = Some(30);
        priced.metrics.cached_tokens = Some(CacheTokens::parse(Some(500), Some(10)).unwrap());
        priced.metrics.cost = Some(RunCost::Priced { usd: 0.25 });
        priced.case_score = Some(0.5);

        let mut failed = run("run-002", "failed", Some(1_500));
        failed.metrics.exit_code = Some(2);
        failed.failure_kind = Some("nonzero_exit".to_string());

        let mut cached = run("run-003", "completed", Some(900));
        cached.cache = Some(RunCacheInfo {
            hit: true,
            source_run_id: "run-000".to_string(),
            key: "sha256:cache".to_string(),
        });

        let mut unanswered = run("run-004", "failed", None);
        unanswered.failure_kind = Some(FAILURE_KIND_RUNNER.to_string());
        unanswered.runner_invocations = 2;

        let mut traced = run("run-005", "completed", Some(2_000));
        traced.trace = Some(reference(LIVE_SUITE, LIVE_RUN_SPAN));
        traced.grade_trace = Some(reference(LIVE_SUITE, LIVE_RUN_GRADE_SPAN));

        ReportDocument {
            report: ReportSection {
                id: "report-7".to_string(),
                generated_at: "2026-05-26T12:00:00Z".to_string(),
                iteration: 3,
                producer: ProducerSection {
                    name: "trg".to_string(),
                    version: "0.0.0".to_string(),
                },
                runner: Some("claude-code".to_string()),
                runner_binary: None,
                runner_version: Some("2.1.0".to_string()),
                environment: Default::default(),
                permission: Default::default(),
                allowed_tools: None,
                telemetry_forwarding: crate::agentskills::runner::TelemetryForwarding::Off,
                ci: None,
                trace: None,
            },
            suite: SuiteSection {
                skill_name: "demo-skill".to_string(),
                skill_path: "demo-skill".to_string(),
                skill_hash: "sha256:abc".to_string(),
                evals_path: "demo-skill/evals/evals.json".to_string(),
                evals_hash: "sha256:def".to_string(),
                eval_dir: EvalDirName::default(),
                old_skill_path: None,
                old_skill_hash: None,
                case_selection: None,
            },
            dimensions: DimensionsSection {
                eval_cases: Vec::new(),
                assertions: Vec::new(),
                skill_revisions: Vec::new(),
                model_configs: Vec::new(),
                scenarios: vec![ScenarioDimension {
                    id: ScenarioKind::WithSkill,
                    kind: ScenarioKind::WithSkill,
                }],
                grading_strategies: vec![GradingStrategy {
                    grader: GraderChoice::Auto { judge: None },
                    strict: false,
                }],
            },
            runs: vec![priced, failed, cached, unanswered, traced],
            assertion_results: vec![
                assertion("run-001", GraderKind::Mechanical, true, "found SKILL.md in final.md"),
                assertion("run-001", GraderKind::Llm, false, "the answer quoted secret notes"),
                assertion("run-005", GraderKind::Mechanical, true, "found it"),
            ],
            summaries: SummariesSection {
                by_scenario: Vec::new(),
                human_feedback: None,
            },
            improvement_feedback: Vec::new(),
            comparisons: Vec::new(),
            iteration_summary: None,
            budget: None,
        }
    }

    fn options(traced_runs: TracedRuns) -> ReplayOptions {
        ReplayOptions {
            traced_runs,
            content: ContentCapture::NoContent,
            exported_from: Some(reference(EXPORT_TRACE, EXPORT_SPAN)),
        }
    }

    /// Replays `document` on an in-memory tracer, with a subscriber that turns
    /// evaluation events into log records the way the production logs bridge does.
    fn replayed(document: &ReportDocument, options: &ReplayOptions) -> (Replay, CapturedTrace, Vec<SdkLogRecord>) {
        crate::telemetry::testing::consult_every_dispatcher();
        let spans = InMemorySpanExporter::default();
        let tracer_provider = SdkTracerProvider::builder().with_simple_exporter(spans.clone()).build();
        let logs = InMemoryLogExporter::default();
        let logger_provider = SdkLoggerProvider::builder().with_simple_exporter(logs.clone()).build();
        let subscriber = tracing_subscriber::registry().with(OpenTelemetryTracingBridge::new(&logger_provider));
        let tracer = tracer_provider.tracer("trg");
        let outcome = tracing::subscriber::with_default(subscriber, || replay(&tracer, document, options))
            .expect("the fixture bundle replays");
        let _ = tracer_provider.force_flush();
        let _ = logger_provider.force_flush();
        let trace = CapturedTrace {
            spans: spans.get_finished_spans().expect("never shut down here"),
        };
        let records = logs
            .get_emitted_logs()
            .expect("never shut down here")
            .into_iter()
            .map(|log| log.record)
            .collect();
        (outcome, trace, records)
    }

    fn grade_span<'a>(trace: &'a CapturedTrace, id: &str) -> Option<&'a SpanData> {
        trace.span_where("grade run", EVAL_RUN_ID, id)
    }

    fn run_span<'a>(trace: &'a CapturedTrace, id: &str) -> &'a SpanData {
        trace
            .spans_named("run")
            .into_iter()
            .find(|span| attribute(span, EVAL_RUN_ID) == Some(Value::from(id.to_string())))
            .unwrap_or_else(|| panic!("no run span for {id}"))
    }

    fn attribute(span: &SpanData, key: &str) -> Option<Value> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    }

    fn child<'a>(trace: &'a CapturedTrace, parent: &SpanData, name: &str) -> Option<&'a SpanData> {
        trace.children_of(parent).into_iter().find(|span| span.name == name)
    }

    fn links_to(span: &SpanData, target: &SpanReference) -> bool {
        span.links.iter().any(|link| {
            link.span_context.trace_id() == target.trace_id() && link.span_context.span_id() == target.span_id()
        })
    }

    fn log_attribute<'a>(record: &'a SdkLogRecord, key: &str) -> Option<&'a AnyValue> {
        record
            .attributes_iter()
            .find(|(name, _)| name.as_str() == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn a_replay_has_the_shape_of_a_live_pass() {
        let (_, trace, _) = replayed(&bundle(), &options(TracedRuns::Skip));

        let suite = trace.span("invoke_workflow demo-skill").expect("suite span");
        assert_eq!(suite.parent_span_id, opentelemetry::trace::SpanId::INVALID);
        trace.assert_child_of("execute runs", "invoke_workflow demo-skill");
        trace.assert_child_of("grade", "invoke_workflow demo-skill");
        let execute = trace.span("execute runs").unwrap();
        assert_eq!(trace.children_of(execute).len(), 4, "every untraced run");

        let priced = run_span(&trace, "run-001");
        let attempt = child(&trace, priced, "attempt").expect("the answered attempt");
        let agent = child(&trace, attempt, "invoke_agent claude-code").expect("the harness invocation");
        assert_eq!(agent.span_kind, SpanKind::Client);

        assert!(
            trace.children_of(run_span(&trace, "run-003")).is_empty(),
            "a cache hit invoked no harness"
        );
        let unanswered = child(&trace, run_span(&trace, "run-004"), "attempt").expect("the attempt that errored");
        assert!(trace.children_of(unanswered).is_empty(), "the harness never answered");

        let graded = grade_span(&trace, "run-001").expect("the run's grade span");
        assert_eq!(
            graded.parent_span_id,
            trace.span("grade").unwrap().span_context.span_id()
        );
        let graders: Vec<_> = trace
            .children_of(graded)
            .iter()
            .map(|span| span.name.to_string())
            .collect();
        assert_eq!(graders, ["grader mechanical", "grader llm"]);
        assert!(links_to(
            graded,
            &SpanReference::of_context(&priced.span_context).unwrap()
        ));
    }

    #[test]
    fn spans_start_when_the_bundle_was_stamped_and_last_the_recorded_duration() {
        let (_, trace, _) = replayed(&bundle(), &options(TracedRuns::Skip));

        let priced = run_span(&trace, "run-001");
        assert_eq!(priced.start_time, generated_at());
        assert_eq!(priced.end_time, after(4_000));
        let agent = trace.span("invoke_agent claude-code").unwrap();
        assert_eq!(agent.start_time, generated_at());
        assert_eq!(agent.end_time, after(4_000));

        let unanswered = run_span(&trace, "run-004");
        assert_eq!(
            unanswered.start_time, unanswered.end_time,
            "no duration recorded, so no length invented"
        );

        for name in ["invoke_workflow demo-skill", "execute runs"] {
            let span = trace.span(name).unwrap();
            assert_eq!(span.start_time, generated_at(), "{name}");
            assert_eq!(span.end_time, after(4_000), "{name} ends with its longest run");
        }
        for span in [
            trace.span("grade").unwrap(),
            grade_span(&trace, "run-001").unwrap(),
            trace.span("grader llm").unwrap(),
        ] {
            assert_eq!(span.start_time, after(4_000), "{}", span.name);
            assert_eq!(span.end_time, after(4_000), "{} has no recorded duration", span.name);
        }
    }

    #[test]
    fn integer_attributes_are_exported_as_integers() {
        let (_, trace, _) = replayed(&bundle(), &options(TracedRuns::Skip));

        assert_eq!(
            trace.attribute("invoke_workflow demo-skill", EVAL_ITERATION),
            Some(Value::I64(3))
        );
        assert_eq!(trace.attribute("execute runs", EVAL_RUN_COUNT), Some(Value::I64(5)));
        let agent = "invoke_agent claude-code";
        assert_eq!(trace.attribute(agent, GEN_AI_USAGE_INPUT_TOKENS), Some(Value::I64(120)));
        assert_eq!(trace.attribute(agent, GEN_AI_USAGE_OUTPUT_TOKENS), Some(Value::I64(30)));
        assert_eq!(
            trace.attribute(agent, GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS),
            Some(Value::I64(500))
        );
        assert_eq!(
            trace.attribute(agent, GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS),
            Some(Value::I64(10))
        );
        assert_eq!(trace.attribute(agent, PROCESS_EXIT_CODE), Some(Value::I64(0)));
        assert_eq!(trace.attribute(agent, EVAL_COST_USD), Some(Value::F64(0.25)));
        assert_eq!(
            attribute(run_span(&trace, "run-001"), EVAL_ITERATION),
            Some(Value::I64(3))
        );
        let unanswered = child(&trace, run_span(&trace, "run-004"), "attempt").unwrap();
        assert_eq!(attribute(unanswered, EVAL_ATTEMPT), Some(Value::I64(2)));
        assert_eq!(
            attribute(grade_span(&trace, "run-001").unwrap(), EVAL_ITERATION),
            Some(Value::I64(3))
        );
    }

    #[test]
    fn the_suite_and_runs_carry_what_a_live_pass_records() {
        let (_, trace, _) = replayed(&bundle(), &options(TracedRuns::Skip));

        let suite = "invoke_workflow demo-skill";
        for (key, value) in [
            (GEN_AI_OPERATION_NAME, "invoke_workflow"),
            (GEN_AI_WORKFLOW_NAME, "demo-skill"),
            (EVAL_SKILL_REVISION, "sha256:abc"),
            (EVAL_SCENARIOS, "with_skill"),
            (EVAL_GRADING_STRATEGY, "auto"),
            (EVAL_REPORT_ID, "report-7"),
        ] {
            assert_eq!(trace.attribute(suite, key), Some(Value::from(value)), "{key}");
        }
        assert!(links_to(
            trace.span(suite).unwrap(),
            &reference(EXPORT_TRACE, EXPORT_SPAN)
        ));

        let priced = run_span(&trace, "run-001");
        for (key, value) in [
            (EVAL_CASE_ID, "case-run-001"),
            (EVAL_CASE_SPLIT, "test"),
            (EVAL_SCENARIO, "with_skill"),
            (EVAL_RUNNER_KIND, "claude"),
            (EVAL_RUNNER_VERSION, "2.1.0"),
            (EVAL_RUNNER_MODEL, "claude-sonnet-4-5"),
            (EVAL_RUN_STATUS, "completed"),
        ] {
            assert_eq!(attribute(priced, key), Some(Value::from(value)), "{key}");
        }
        assert_eq!(attribute(priced, EVAL_CACHE_HIT), Some(Value::Bool(false)));
        assert_eq!(
            attribute(run_span(&trace, "run-003"), EVAL_CACHE_HIT),
            Some(Value::Bool(true))
        );
        let agent = trace.span("invoke_agent claude-code").unwrap();
        assert_eq!(attribute(agent, GEN_AI_PROVIDER_NAME), Some(Value::from("anthropic")));
        assert_eq!(attribute(agent, PROCESS_EXECUTABLE_NAME), Some(Value::from("claude")));
    }

    #[test]
    fn failures_are_marked_the_way_a_live_pass_marks_them() {
        let (_, trace, _) = replayed(&bundle(), &options(TracedRuns::Skip));

        let failed = run_span(&trace, "run-002");
        assert_eq!(attribute(failed, ERROR_TYPE), Some(Value::from("nonzero_exit")));
        assert!(matches!(failed.status, Status::Error { .. }));
        let agent = child(
            &trace,
            child(&trace, failed, "attempt").unwrap(),
            "invoke_agent claude-code",
        )
        .unwrap();
        assert_eq!(attribute(agent, ERROR_TYPE), Some(Value::from("nonzero_exit")));
        assert_eq!(attribute(agent, PROCESS_EXIT_CODE), Some(Value::I64(2)));

        let unanswered = child(&trace, run_span(&trace, "run-004"), "attempt").unwrap();
        assert_eq!(attribute(unanswered, ERROR_TYPE), Some(Value::from("runner_error")));
        assert_eq!(
            attribute(unanswered, EVAL_ATTEMPT_OUTCOME),
            Some(Value::from("runner_error"))
        );
        assert_eq!(
            attribute(run_span(&trace, "run-004"), ERROR_TYPE),
            Some(Value::from(FAILURE_KIND_RUNNER))
        );
    }

    #[test]
    fn every_replayed_span_says_it_is_a_replay() {
        let (_, trace, _) = replayed(&bundle(), &options(TracedRuns::Skip));

        assert!(!trace.spans.is_empty());
        for span in &trace.spans {
            assert_eq!(attribute(span, EVAL_REPLAYED), Some(Value::Bool(true)), "{}", span.name);
        }
    }

    #[test]
    fn a_run_already_traced_live_is_left_out_by_default() {
        let (outcome, trace, records) = replayed(&bundle(), &options(TracedRuns::Skip));

        let Replay::Exported { runs, skipped_runs, .. } = outcome else {
            panic!("expected an export, got {outcome:?}");
        };
        assert_eq!(skipped_runs, ["run-005"]);
        assert!(runs.iter().all(|run| run.run_id != "run-005"));
        assert!(grade_span(&trace, "run-005").is_none());
        let traced_run = AnyValue::from("run-005".to_string());
        assert!(records
            .iter()
            .all(|record| log_attribute(record, EVAL_RUN_ID) != Some(&traced_run)));
    }

    #[test]
    fn a_forced_replay_of_a_traced_run_links_it_to_its_live_span() {
        let (outcome, trace, _) = replayed(&bundle(), &options(TracedRuns::Replay));

        let Replay::Exported { skipped_runs, .. } = outcome else {
            panic!("expected an export, got {outcome:?}");
        };
        assert!(skipped_runs.is_empty());
        let live = reference(LIVE_SUITE, LIVE_RUN_SPAN);
        let replayed_run = run_span(&trace, "run-005");
        assert_ne!(replayed_run.span_context.trace_id(), live.trace_id());
        assert!(links_to(replayed_run, &live));
        let graded = grade_span(&trace, "run-005").unwrap();
        assert!(links_to(graded, &live));
        assert!(links_to(graded, &reference(LIVE_SUITE, LIVE_RUN_GRADE_SPAN)));
    }

    fn graded_live() -> ReportDocument {
        let mut document = bundle();
        document.runs[0].grade_trace = Some(reference(LIVE_SUITE, LIVE_GRADE_SPAN));
        document
    }

    fn evaluations_of<'a>(records: &'a [SdkLogRecord], run_id: &str) -> Vec<&'a SdkLogRecord> {
        let run_id = AnyValue::from(run_id.to_string());
        records
            .iter()
            .filter(|record| log_attribute(record, EVAL_RUN_ID) == Some(&run_id))
            .collect()
    }

    #[test]
    fn a_run_whose_grading_was_traced_live_does_not_send_its_evaluations_again() {
        let (outcome, trace, records) = replayed(&graded_live(), &options(TracedRuns::Skip));

        let Replay::Exported {
            runs, skipped_grades, ..
        } = outcome
        else {
            panic!("expected an export, got {outcome:?}");
        };
        assert!(
            runs.iter().any(|run| run.run_id == "run-001"),
            "the run itself is still replayed"
        );
        assert_eq!(skipped_grades, ["run-001", "run-005"]);
        assert!(grade_span(&trace, "run-001").is_none());
        assert!(evaluations_of(&records, "run-001").is_empty());
    }

    #[test]
    fn a_forced_replay_of_a_live_grade_links_it_to_the_live_grade_span() {
        let (outcome, trace, records) = replayed(&graded_live(), &options(TracedRuns::Replay));

        let Replay::Exported { skipped_grades, .. } = outcome else {
            panic!("expected an export, got {outcome:?}");
        };
        assert!(skipped_grades.is_empty());
        let graded = grade_span(&trace, "run-001").expect("the grade is replayed");
        assert!(links_to(graded, &reference(LIVE_SUITE, LIVE_GRADE_SPAN)));
        assert_eq!(evaluations_of(&records, "run-001").len(), 3);
    }

    #[test]
    fn a_report_whose_every_run_and_grade_was_traced_live_exports_nothing() {
        let mut document = bundle();
        document.runs.retain(|run| run.trace.is_some());

        let (outcome, trace, records) = replayed(&document, &options(TracedRuns::Skip));

        assert_eq!(
            outcome,
            Replay::NothingToReplay {
                skipped_runs: vec!["run-005".to_string()],
                skipped_grades: vec!["run-005".to_string()],
            }
        );
        assert!(trace.spans.is_empty());
        assert!(records.is_empty());
    }

    /// The bundle with its live-traced run graded later, with tracing off.
    fn graded_untraced() -> ReportDocument {
        let mut document = bundle();
        let traced = document.runs.iter_mut().find(|run| run.id == "run-005").unwrap();
        traced.grade_trace = None;
        document
    }

    #[test]
    fn an_untraced_grade_of_a_live_run_is_replayed_inside_the_live_trace() {
        let (outcome, trace, records) = replayed(&graded_untraced(), &options(TracedRuns::Skip));

        let Replay::Exported {
            runs,
            skipped_runs,
            live_run_grades,
            skipped_grades,
            ..
        } = outcome
        else {
            panic!("expected an export, got {outcome:?}");
        };
        assert_eq!(skipped_runs, ["run-005"], "the live run is not sent again");
        assert!(runs.iter().all(|run| run.run_id != "run-005"));
        assert!(skipped_grades.is_empty());

        let live = reference(LIVE_SUITE, LIVE_RUN_SPAN);
        let graded = grade_span(&trace, "run-005").expect("the grade is replayed");
        assert_eq!(graded.span_context.trace_id(), live.trace_id());
        assert_eq!(graded.parent_span_id, live.span_id());
        assert_eq!(attribute(graded, EVAL_REPLAYED), Some(Value::Bool(true)));
        assert_eq!(
            live_run_grades,
            [ReplayedRun {
                run_id: "run-005".to_string(),
                span: SpanReference::of_context(&graded.span_context).unwrap(),
            }]
        );

        let evaluations = evaluations_of(&records, "run-005");
        assert_eq!(evaluations.len(), 1);
        let context = evaluations[0].trace_context().expect("span context");
        assert_eq!(context.trace_id, live.trace_id());
        assert_eq!(context.span_id, live.span_id());
        assert_eq!(
            log_attribute(evaluations[0], EVAL_REPLAYED),
            Some(&AnyValue::Boolean(true))
        );
    }

    #[test]
    fn a_report_with_only_untraced_grades_left_exports_them_without_a_suite() {
        let mut document = graded_untraced();
        document.runs.retain(|run| run.trace.is_some());

        let (outcome, trace, _) = replayed(&document, &options(TracedRuns::Skip));

        let Replay::Exported {
            suite,
            runs,
            live_run_grades,
            ..
        } = outcome
        else {
            panic!("expected an export, got {outcome:?}");
        };
        assert_eq!(suite, None);
        assert!(runs.is_empty());
        assert_eq!(live_run_grades.len(), 1);
        assert!(trace.span("invoke_workflow demo-skill").is_none());
        assert!(trace.span("grade").is_none());
    }

    #[test]
    fn evaluations_land_in_the_replayed_run_span() {
        let (outcome, _, records) = replayed(&bundle(), &options(TracedRuns::Skip));
        let Replay::Exported { runs, .. } = outcome else {
            panic!("expected an export");
        };
        let priced = runs.iter().find(|run| run.run_id == "run-001").unwrap().span;

        let evaluations: Vec<_> = records
            .iter()
            .filter(|record| record.event_name() == Some("gen_ai.evaluation.result"))
            .collect();
        assert_eq!(evaluations.len(), 3, "one per recorded result plus the case score");
        for record in &evaluations {
            let context = record.trace_context().expect("an evaluation carries span context");
            assert_eq!(context.trace_id, priced.trace_id());
            assert_eq!(context.span_id, priced.span_id());
            assert_eq!(log_attribute(record, EVAL_REPLAYED), Some(&AnyValue::Boolean(true)));
            assert_eq!(
                log_attribute(record, GEN_AI_EVALUATION_EXPLANATION),
                None,
                "evidence quotes graded content, so it stays off by default"
            );
        }
        let case_score = AnyValue::from(CASE_SCORE_EVALUATION);
        let case_score = evaluations
            .iter()
            .find(|record| log_attribute(record, GEN_AI_EVALUATION_NAME) == Some(&case_score))
            .expect("the case score");
        assert_eq!(
            log_attribute(case_score, GEN_AI_EVALUATION_SCORE_VALUE),
            Some(&AnyValue::Double(0.5))
        );
    }

    #[test]
    fn evidence_is_exported_only_when_event_content_capture_is_on() {
        let options = ReplayOptions {
            content: ContentCapture::from_lookup(&crate::telemetry::fixed(CONTENT_ON)),
            ..options(TracedRuns::Skip)
        };

        let (_, _, records) = replayed(&bundle(), &options);

        let evidence = AnyValue::from("the answer quoted secret notes");
        assert!(records
            .iter()
            .any(|record| log_attribute(record, GEN_AI_EVALUATION_EXPLANATION) == Some(&evidence)));
    }

    #[test]
    fn nothing_is_exported_without_a_trace_exporter() {
        let tracer = opentelemetry::trace::noop::NoopTracer::new();

        let outcome = replay(&tracer, &bundle(), &options(TracedRuns::Skip)).unwrap();

        assert_eq!(outcome, Replay::TracingOff);
    }

    #[test]
    fn a_bundle_with_no_readable_start_is_refused_rather_than_placed_at_an_invented_time() {
        let mut document = bundle();
        document.report.generated_at = "yesterday".to_string();
        let tracer = opentelemetry::trace::noop::NoopTracer::new();

        let error = replay(&tracer, &document, &options(TracedRuns::Skip)).unwrap_err();

        assert!(error.to_string().contains("report.generated_at"), "{error}");
    }
}
