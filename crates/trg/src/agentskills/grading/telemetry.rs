//! Spans, evaluation events and counters for grading, and the CLI client span
//! every eval subcommand that starts a helper process records it under.

use std::path::Path;
use std::process::{Command, ExitStatus};

use opentelemetry::trace::TraceContextExt;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, PROCESS_EXECUTABLE_NAME, PROCESS_EXIT_CODE, PROCESS_PID,
};
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::{AssertionGradeResult, AssertionOutcome};
use crate::agentskills::evals::EvalError;
use crate::agentskills::span_reference::SpanReference;
use crate::telemetry::propagation::inject_std_command;
use crate::telemetry::semconv::generated::attributes::{
    GEN_AI_EVALUATION_EXPLANATION, GEN_AI_EVALUATION_NAME, GEN_AI_EVALUATION_SCORE_LABEL, GEN_AI_EVALUATION_SCORE_VALUE,
};
use crate::telemetry::semconv::trg::{EVAL_ASSERTIONS_METRIC, EVAL_CASE_ID, EVAL_GRADER_KIND, EVAL_RUN_ID};
use crate::telemetry::{ContentCapture, LOGS_ONLY_TARGET};

const NONZERO_EXIT: &str = "nonzero_exit";
const SIGNALED: &str = "signaled";
const SPAWN_FAILED: &str = "spawn_failed";

/// The name of the case-level evaluation that carries a run's `case_score`.
pub(crate) const CASE_SCORE_EVALUATION: &str = "case_score";

pub(crate) fn record_error(span: &tracing::Span, error_type: &str) {
    span.record(ERROR_TYPE, error_type);
    span.record("otel.status_code", "ERROR");
}

pub(crate) fn eval_error_type(error: &EvalError) -> &'static str {
    match error {
        EvalError::Io(_) => "io",
        EvalError::Json(_) => "json",
        EvalError::Validation(_) => "validation",
    }
}

/// Marks `span` failed when `result` is an error, so a phase that aborted
/// reads as failed in the trace.
pub(crate) fn record_result<T>(span: &tracing::Span, result: &Result<T, EvalError>) {
    if let Err(error) = result {
        record_error(span, eval_error_type(error));
    }
}

/// An INFO span named `$name` that a failed phase can mark with `error.type`.
macro_rules! phase_span {
    ($name:literal) => {
        tracing::info_span!(
            $name,
            "otel.status_code" = tracing::field::Empty,
            { opentelemetry_semantic_conventions::attribute::ERROR_TYPE } = tracing::field::Empty,
        )
    };
}
pub(crate) use phase_span;

/// Runs `work` inside `span`, marking it failed when `work` fails.
pub(crate) fn phase<T>(span: tracing::Span, work: impl FnOnce() -> Result<T, EvalError>) -> Result<T, EvalError> {
    let result = span.in_scope(work);
    record_result(&span, &result);
    result
}

/// Links `span` to a span an earlier invocation recorded, when it recorded one.
pub(crate) fn link_to(span: &tracing::Span, reference: Option<&SpanReference>) {
    if let Some(reference) = reference {
        span.add_link(reference.span_context());
    }
}

/// One helper process (a grader or judge script, a real MCP server) as an OTel
/// CLI client span. Only the executable's file name is recorded: a script
/// path can name a home directory and its arguments can carry anything.
pub(crate) struct CliSpan(tracing::Span);

impl CliSpan {
    pub(crate) fn start(program: &str) -> Self {
        let executable = Path::new(program)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(program)
            .to_string();
        Self(tracing::info_span!(
            "process",
            "otel.name" = executable.as_str(),
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { PROCESS_EXECUTABLE_NAME } = executable.as_str(),
            { PROCESS_PID } = Empty,
            { PROCESS_EXIT_CODE } = Empty,
            { ERROR_TYPE } = Empty,
        ))
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        &self.0
    }

    /// Hands this span's trace context to the process through `TRACEPARENT`.
    pub(crate) fn inject(&self, command: &mut Command) {
        self.0.in_scope(|| inject_std_command(command));
    }

    pub(crate) fn spawned(&self, pid: u32) {
        self.0.record(PROCESS_PID, i64::from(pid));
    }

    pub(crate) fn spawn_failed(&self) {
        record_error(&self.0, SPAWN_FAILED);
    }

    pub(crate) fn exited(&self, status: &ExitStatus) {
        match status.code() {
            Some(code) => {
                self.0.record(PROCESS_EXIT_CODE, code);
                if code != 0 {
                    record_error(&self.0, NONZERO_EXIT);
                }
            }
            None => record_error(&self.0, SIGNALED),
        }
    }

    pub(crate) fn failed(&self, error_type: &str) {
        record_error(&self.0, error_type);
    }
}

/// The low-cardinality `gen_ai.evaluation.score.label` an evaluation is
/// reported under. `pass` and `fail` are scored; the rest say why nothing was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvaluationLabel {
    Pass,
    Fail,
    Unsupported,
    Excluded,
    Ungraded,
}

impl EvaluationLabel {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unsupported => "unsupported",
            Self::Excluded => "excluded",
            Self::Ungraded => "ungraded",
        }
    }

    fn of(result: &AssertionGradeResult) -> Self {
        match result.outcome() {
            AssertionOutcome::Excluded => Self::Excluded,
            AssertionOutcome::Unsupported => Self::Unsupported,
            AssertionOutcome::Ungraded => Self::Ungraded,
            AssertionOutcome::Scored if result.passed => Self::Pass,
            AssertionOutcome::Scored => Self::Fail,
        }
    }
}

/// One `gen_ai.evaluation.result` event.
pub(crate) struct Evaluation<'a> {
    pub name: &'a str,
    pub label: Option<&'a str>,
    pub score: Option<f64>,
    pub explanation: Option<&'a str>,
    pub grader_kind: Option<&'a str>,
    pub case_id: &'a str,
    pub run_id: Option<&'a str>,
}

impl<'a> Evaluation<'a> {
    /// `name` is the declared grader name when there is one, its kind otherwise.
    pub(crate) fn of_assertion(
        result: &'a AssertionGradeResult,
        name: &'a str,
        grader_kind: &'a str,
        case_id: &'a str,
        run_id: &'a str,
    ) -> Self {
        let label = EvaluationLabel::of(result);
        let score = match (label, result.votes) {
            (EvaluationLabel::Pass | EvaluationLabel::Fail, Some(votes)) if votes.total() > 0 => {
                Some(f64::from(votes.passed) / f64::from(votes.total()))
            }
            (EvaluationLabel::Pass, _) => Some(1.0),
            (EvaluationLabel::Fail, _) => Some(0.0),
            _ => None,
        };
        Self {
            name,
            label: Some(label.as_str()),
            score,
            explanation: Some(result.evidence.as_str()),
            grader_kind: Some(grader_kind),
            case_id,
            run_id: Some(run_id),
        }
    }

    /// Emits the event through the logs bridge in the context of `parent` when
    /// the report recorded one, so it lands in the run's trace even though that
    /// run span ended in an earlier pass, and in the current span's otherwise.
    /// The explanation quotes what was graded, so it is content, and the event goes
    /// to the logs signal alone: as a span event it would put that content on a trace
    /// whatever the capture mode allows there.
    pub(crate) fn emit(&self, parent: Option<&SpanReference>, content: ContentCapture) {
        let explanation = self.explanation.filter(|_| content.includes_event());
        let _context = parent.map(|reference| {
            opentelemetry::Context::current()
                .with_remote_span_context(reference.span_context())
                .attach()
        });
        tracing::event!(
            name: "gen_ai.evaluation.result",
            target: LOGS_ONLY_TARGET,
            tracing::Level::INFO,
            { GEN_AI_EVALUATION_NAME } = self.name,
            { GEN_AI_EVALUATION_SCORE_VALUE } = self.score,
            { GEN_AI_EVALUATION_SCORE_LABEL } = self.label,
            { GEN_AI_EVALUATION_EXPLANATION } = explanation,
            { EVAL_GRADER_KIND } = self.grader_kind,
            { EVAL_CASE_ID } = self.case_id,
            { EVAL_RUN_ID } = self.run_id
        );
    }
}

/// Counts one graded assertion under its label and grader kind.
pub(crate) fn count_assertion(result: &AssertionGradeResult, grader_kind: &str) {
    opentelemetry::global::meter("trg")
        .u64_counter(EVAL_ASSERTIONS_METRIC)
        .with_unit("{assertion}")
        .build()
        .add(
            1,
            &[
                KeyValue::new(GEN_AI_EVALUATION_SCORE_LABEL, EvaluationLabel::of(result).as_str()),
                KeyValue::new(EVAL_GRADER_KIND, grader_kind.to_string()),
            ],
        );
}

#[cfg(test)]
pub(crate) mod testing {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
    use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLogRecord, SdkLoggerProvider};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing_subscriber::prelude::*;

    use crate::telemetry::testing::CapturedTrace;

    /// Like [`crate::telemetry::testing::capture`], also keeping the log records the tracing bridge
    /// turned events into. Both layers carry the filters `trg` exports through, so what
    /// reaches each signal here is what reaches it in a real process.
    pub(crate) fn capture_with_logs<T>(work: impl FnOnce() -> T) -> (T, CapturedTrace, Vec<SdkLogRecord>) {
        crate::telemetry::testing::consult_every_dispatcher();
        let spans = InMemorySpanExporter::default();
        let tracer_provider = SdkTracerProvider::builder().with_simple_exporter(spans.clone()).build();
        let logs = InMemoryLogExporter::default();
        let logger_provider = SdkLoggerProvider::builder().with_simple_exporter(logs.clone()).build();
        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_opentelemetry::layer()
                    .with_tracer(tracer_provider.tracer("trg"))
                    .with_filter(crate::telemetry::trace_filter()),
            )
            .with(OpenTelemetryTracingBridge::new(&logger_provider).with_filter(crate::telemetry::log_filter()));
        let output = tracing::subscriber::with_default(subscriber, work);
        let _ = tracer_provider.force_flush();
        let _ = logger_provider.force_flush();
        let trace = CapturedTrace {
            spans: spans
                .get_finished_spans()
                .expect("in-memory exporter is never shut down here"),
        };
        let records = logs
            .get_emitted_logs()
            .expect("in-memory exporter is never shut down here")
            .into_iter()
            .map(|log| log.record)
            .collect();
        (output, trace, records)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::capture_with_logs;
    use super::*;

    fn evaluation() -> Evaluation<'static> {
        Evaluation {
            name: "contains",
            label: Some("fail"),
            score: Some(0.0),
            explanation: Some("the answer quoted the customer's address"),
            grader_kind: Some("contains"),
            case_id: "case",
            run_id: Some("run"),
        }
    }

    #[test]
    fn an_event_only_evaluation_carries_its_explanation_on_the_log_and_nothing_onto_the_span() {
        let ((), trace, logs) = capture_with_logs(|| {
            tracing::info_span!("grade").in_scope(|| evaluation().emit(None, ContentCapture::EventOnly));
        });

        let grade = trace.span("grade").expect("grade span exported");
        assert!(
            grade.events.is_empty(),
            "an evaluation never becomes a span event: {:?}",
            grade.events
        );
        assert!(grade
            .attributes
            .iter()
            .all(|kv| kv.key.as_str() != GEN_AI_EVALUATION_EXPLANATION));

        let record = logs
            .iter()
            .find(|record| record.event_name() == Some("gen_ai.evaluation.result"))
            .expect("the evaluation reaches the logs signal");
        let explanation = record
            .attributes_iter()
            .find(|(key, _)| key.as_str() == GEN_AI_EVALUATION_EXPLANATION)
            .map(|(_, value)| format!("{value:?}"));
        assert!(
            explanation.is_some_and(|value| value.contains("the customer's address")),
            "the log record keeps the explanation event capture allows"
        );
        assert_eq!(
            record.trace_context().map(|context| context.span_id),
            Some(grade.span_context.span_id()),
            "the log record still correlates with the span it was emitted under"
        );
    }
}
