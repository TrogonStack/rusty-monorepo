//! Spans and metrics for one attempt at a run: preparing its workspace, invoking the
//! harness, and recording what came back.
//!
//! What goes on a span is what describes the step, never what it carried: no argv, no
//! prompt, no environment. Tool arguments and results reach a span only when the
//! operator opted into GenAI content capture.

use opentelemetry::metrics::Histogram;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, PROCESS_EXECUTABLE_NAME, PROCESS_EXIT_CODE, PROCESS_PID,
};
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::harness_spans::{self, HarnessStream, Reconstruction};
use super::{capture_subprocess, CapturedProcess, EvalRunOutcome, RunStatus, Runner, RunnerError};
use crate::agentskills::budget::RunCost;
use crate::telemetry::propagation::inject_std_command;
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, gen_ai_provider_name, GEN_AI_AGENT_NAME, GEN_AI_OPERATION_NAME, GEN_AI_PROVIDER_NAME,
    GEN_AI_REQUEST_MODEL, GEN_AI_RESPONSE_MODEL, GEN_AI_TOOL_NAME, GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS,
    GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS, GEN_AI_USAGE_INPUT_TOKENS, GEN_AI_USAGE_OUTPUT_TOKENS,
};
use crate::telemetry::semconv::generated::metrics::{
    GEN_AI_EXECUTE_TOOL_DURATION, GEN_AI_INVOKE_AGENT_DURATION, GEN_AI_INVOKE_AGENT_INFERENCE_CALLS,
    GEN_AI_INVOKE_AGENT_TOOL_CALLS,
};
use crate::telemetry::semconv::trg::{EVAL_COST_USD, EVAL_HARNESS_API_DURATION_MS, EVAL_HARNESS_DURATION_MS};
use crate::telemetry::ContentCapture;

/// Why a span is marked failed, as its low-cardinality `error.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureType {
    NonzeroExit,
    Signaled,
    Timeout,
    SpawnFailed,
    Io,
    InvalidOutput,
    UnsupportedScenario,
    Json,
    /// The harness exited cleanly but reported the run itself failed.
    HarnessError,
}

impl FailureType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NonzeroExit => "nonzero_exit",
            Self::Signaled => "signaled",
            Self::Timeout => "timeout",
            Self::SpawnFailed => "spawn_failed",
            Self::Io => "io",
            Self::InvalidOutput => "invalid_output",
            Self::UnsupportedScenario => "unsupported_scenario",
            Self::Json => "json",
            Self::HarnessError => "harness_error",
        }
    }

    fn of_exit(code: Option<i32>) -> Option<Self> {
        match code {
            Some(0) => None,
            Some(_) => Some(Self::NonzeroExit),
            None => Some(Self::Signaled),
        }
    }
}

impl RunnerError {
    pub fn failure_type(&self) -> FailureType {
        match self {
            Self::Io(_) => FailureType::Io,
            Self::Json(_) => FailureType::Json,
            Self::UnsupportedScenario(_) => FailureType::UnsupportedScenario,
            Self::Spawn { .. } => FailureType::SpawnFailed,
            Self::InvalidOutput { .. } => FailureType::InvalidOutput,
        }
    }
}

fn record_error(span: &tracing::Span, failure: FailureType) {
    span.record(ERROR_TYPE, failure.as_str());
    span.record("otel.status_code", "ERROR");
}

/// An internal span around one step of an attempt.
pub(crate) fn step_span(name: &'static str) -> tracing::Span {
    tracing::info_span!(
        "attempt step",
        "otel.name" = name,
        "otel.kind" = "internal",
        "otel.status_code" = Empty,
        { ERROR_TYPE } = Empty,
    )
}

/// Runs `work` inside a step span named `name`, marking the span failed when it fails.
pub(crate) fn in_step<T>(name: &'static str, work: impl FnOnce() -> Result<T, RunnerError>) -> Result<T, RunnerError> {
    let span = step_span(name);
    let result = span.in_scope(work);
    if let Err(error) = &result {
        record_error(&span, error.failure_type());
    }
    result
}

/// One child process `trg` runs for an attempt, as an OTel CLI client span named for
/// its executable.
pub(crate) struct CliProcessSpan(tracing::Span);

impl CliProcessSpan {
    pub(crate) fn start(executable: &str) -> Self {
        Self(tracing::info_span!(
            "cli process",
            "otel.name" = executable,
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { PROCESS_EXECUTABLE_NAME } = executable,
            { PROCESS_PID } = Empty,
            { PROCESS_EXIT_CODE } = Empty,
            { ERROR_TYPE } = Empty,
        ))
    }

    pub(crate) fn span(&self) -> &tracing::Span {
        &self.0
    }

    pub(crate) fn spawned(&self, pid: u32) {
        self.0.record(PROCESS_PID, i64::from(pid));
    }

    pub(crate) fn exited(&self, code: Option<i32>) {
        if let Some(code) = code {
            self.0.record(PROCESS_EXIT_CODE, code);
        }
        if let Some(failure) = FailureType::of_exit(code) {
            record_error(&self.0, failure);
        }
    }

    pub(crate) fn failed(&self, failure: FailureType) {
        record_error(&self.0, failure);
    }
}

impl Runner {
    /// `gen_ai.provider.name` for the model behind this harness. cursor-agent routes to
    /// several providers and names none on its stream, so it is reported as itself.
    pub fn gen_ai_provider(self) -> &'static str {
        match self {
            Self::ClaudeCode => gen_ai_provider_name::ANTHROPIC,
            Self::Codex => gen_ai_provider_name::OPENAI,
            Self::CursorAgent => "cursor",
        }
    }
}

/// Whether the harness exports spans of its own for this invocation, which decides
/// whether `trg` rebuilds them from its stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessTraces {
    Exported,
    Silent,
}

impl HarnessTraces {
    pub fn exported(exported: bool) -> Self {
        if exported {
            Self::Exported
        } else {
            Self::Silent
        }
    }
}

/// The `invoke_agent` span around one harness subprocess.
pub(crate) struct AgentInvocation {
    runner: Runner,
    span: tracing::Span,
}

impl AgentInvocation {
    pub(crate) fn begin(runner: Runner, runner_model: Option<&str>) -> Self {
        let span = tracing::info_span!(
            "invoke_agent",
            "otel.name" = format!("{} {}", gen_ai_operation_name::INVOKE_AGENT, runner.display_name()),
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { GEN_AI_OPERATION_NAME } = gen_ai_operation_name::INVOKE_AGENT,
            { GEN_AI_PROVIDER_NAME } = runner.gen_ai_provider(),
            { GEN_AI_AGENT_NAME } = runner.display_name(),
            { GEN_AI_REQUEST_MODEL } = runner_model,
            { GEN_AI_RESPONSE_MODEL } = Empty,
            { GEN_AI_USAGE_INPUT_TOKENS } = Empty,
            { GEN_AI_USAGE_OUTPUT_TOKENS } = Empty,
            { GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS } = Empty,
            { GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS } = Empty,
            { EVAL_COST_USD } = Empty,
            { EVAL_HARNESS_DURATION_MS } = Empty,
            { EVAL_HARNESS_API_DURATION_MS } = Empty,
            { PROCESS_EXECUTABLE_NAME } = runner.program_name(),
            { PROCESS_PID } = Empty,
            { PROCESS_EXIT_CODE } = Empty,
            { ERROR_TYPE } = Empty,
        );
        Self { runner, span }
    }

    /// Runs the harness inside this span, handing it the span's trace context so a
    /// harness that exports spans of its own nests them here.
    pub(crate) fn capture(
        &self,
        command: &mut std::process::Command,
        timeout: Option<std::time::Duration>,
    ) -> Result<CapturedProcess, RunnerError> {
        let result = self.span.in_scope(|| {
            inject_std_command(command);
            capture_subprocess(command, timeout)
        });
        if let Err(error) = &result {
            record_error(&self.span, error.failure_type());
        }
        result
    }

    /// Records what the harness reported, rebuilds its turns and tool calls when it
    /// exported none of its own, and ends the span.
    pub(crate) fn finish(
        self,
        captured: &CapturedProcess,
        outcome: &EvalRunOutcome,
        runner_model: Option<&str>,
        traces: HarnessTraces,
    ) {
        let content = ContentCapture::from_env();
        let reconstruction = harness_spans::reconstruct(
            self.runner,
            HarnessStream {
                stdout: &captured.stdout,
                timeline: &captured.stdout_timeline,
                started_at: captured.started_at,
            },
            runner_model,
            content,
        );
        let failure = self.record(captured, outcome, &reconstruction);

        let parent = self.span.context();
        if traces == HarnessTraces::Silent && harness_spans::records(&parent) {
            harness_spans::emit(
                &opentelemetry::global::tracer("trg"),
                &parent,
                self.runner.gen_ai_provider(),
                &reconstruction.steps,
            );
        }
        self.record_metrics(captured, runner_model, &reconstruction, failure);
    }

    fn record(
        &self,
        captured: &CapturedProcess,
        outcome: &EvalRunOutcome,
        reconstruction: &Reconstruction,
    ) -> Option<FailureType> {
        let span = &self.span;
        span.record(PROCESS_PID, i64::from(captured.pid));
        if let Some(code) = captured.exit_code {
            span.record(PROCESS_EXIT_CODE, code);
        }
        let tokens = &outcome.tokens;
        let cached = tokens.cached_tokens();
        for (key, value) in [
            (GEN_AI_USAGE_INPUT_TOKENS, tokens.input_tokens()),
            (GEN_AI_USAGE_OUTPUT_TOKENS, tokens.output_tokens()),
            (
                GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS,
                cached.and_then(|c| c.read_tokens()),
            ),
            (
                GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS,
                cached.and_then(|c| c.write_tokens()),
            ),
        ] {
            if let Some(value) = value {
                span.record(key, i64::try_from(value).unwrap_or(i64::MAX));
            }
        }
        if let Some(RunCost::Priced { usd }) = outcome.cost {
            span.record(EVAL_COST_USD, usd);
        }
        let summary = &reconstruction.summary;
        if let Some(model) = &summary.response_model {
            span.record(GEN_AI_RESPONSE_MODEL, model.as_str());
        }
        if let Some(duration) = summary.duration_ms {
            span.record(EVAL_HARNESS_DURATION_MS, i64::try_from(duration).unwrap_or(i64::MAX));
        }
        if let Some(duration) = summary.api_duration_ms {
            span.record(
                EVAL_HARNESS_API_DURATION_MS,
                i64::try_from(duration).unwrap_or(i64::MAX),
            );
        }

        let failure = match outcome.status {
            RunStatus::Completed => None,
            RunStatus::Timeout => Some(FailureType::Timeout),
            RunStatus::Failed => Some(FailureType::of_exit(captured.exit_code).unwrap_or(FailureType::HarnessError)),
        };
        if let Some(failure) = failure {
            record_error(span, failure);
        }
        failure
    }

    fn record_metrics(
        &self,
        captured: &CapturedProcess,
        runner_model: Option<&str>,
        reconstruction: &Reconstruction,
        failure: Option<FailureType>,
    ) {
        let instruments = instruments();
        let mut attributes = vec![
            KeyValue::new(GEN_AI_OPERATION_NAME, gen_ai_operation_name::INVOKE_AGENT),
            KeyValue::new(GEN_AI_PROVIDER_NAME, self.runner.gen_ai_provider()),
            KeyValue::new(GEN_AI_AGENT_NAME, self.runner.display_name()),
        ];
        if let Some(model) = runner_model {
            attributes.push(KeyValue::new(GEN_AI_REQUEST_MODEL, model.to_string()));
        }
        if let Some(failure) = failure {
            attributes.push(KeyValue::new(ERROR_TYPE, failure.as_str()));
        }
        instruments
            .invoke_agent_duration
            .record(captured.duration_ms as f64 / 1000.0, &attributes);
        instruments
            .tool_calls
            .record(reconstruction.tool_calls() as u64, &attributes);
        instruments
            .inference_calls
            .record(reconstruction.inference_calls() as u64, &attributes);

        for step in reconstruction.steps.iter().filter(|step| step.is_tool()) {
            let mut tool_attributes = vec![
                KeyValue::new(GEN_AI_OPERATION_NAME, gen_ai_operation_name::EXECUTE_TOOL),
                KeyValue::new(GEN_AI_PROVIDER_NAME, self.runner.gen_ai_provider()),
            ];
            if let harness_spans::StepKind::Tool { name, .. } = &step.kind {
                tool_attributes.push(KeyValue::new(GEN_AI_TOOL_NAME, name.clone()));
            }
            if let Some(failure) = step.failure {
                tool_attributes.push(KeyValue::new(ERROR_TYPE, failure.as_str()));
            }
            instruments
                .execute_tool_duration
                .record(step.duration_secs(), &tool_attributes);
        }
    }
}

struct Instruments {
    invoke_agent_duration: Histogram<f64>,
    tool_calls: Histogram<u64>,
    inference_calls: Histogram<u64>,
    execute_tool_duration: Histogram<f64>,
}

fn instruments() -> Instruments {
    let meter = opentelemetry::global::meter("trg");
    Instruments {
        invoke_agent_duration: meter.f64_histogram(GEN_AI_INVOKE_AGENT_DURATION).with_unit("s").build(),
        tool_calls: meter
            .u64_histogram(GEN_AI_INVOKE_AGENT_TOOL_CALLS)
            .with_unit("{tool_call}")
            .build(),
        inference_calls: meter
            .u64_histogram(GEN_AI_INVOKE_AGENT_INFERENCE_CALLS)
            .with_unit("{inference_call}")
            .build(),
        execute_tool_duration: meter.f64_histogram(GEN_AI_EXECUTE_TOOL_DURATION).with_unit("s").build(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::runner::{completed_outcome, timeout_outcome, HarnessTokenUsage};
    use crate::telemetry::testing::capture;
    use std::process::Command;
    use std::time::Duration;

    fn attribute(trace: &crate::telemetry::testing::CapturedTrace, span: &str, key: &str) -> Option<String> {
        trace.attribute(span, key).map(|value| value.to_string())
    }

    #[test]
    fn streamed_stdout_keeps_every_byte_in_order_with_one_stamp_per_line() {
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg("for n in 1 2 3; do printf '{\"n\":%s}\\n' $n; sleep 0.05; done; printf 'tail'");
        let captured = capture_subprocess(&mut command, Some(Duration::from_secs(10))).unwrap();

        assert_eq!(captured.stdout, b"{\"n\":1}\n{\"n\":2}\n{\"n\":3}\ntail");
        let lines: Vec<_> = captured.stdout_timeline.lines(&captured.stdout).collect();
        assert_eq!(lines.len(), 4);
        assert!(lines.iter().all(|line| line.at >= captured.started_at));
        assert!(lines.windows(2).all(|pair| pair[0].at <= pair[1].at));
        assert!(lines[2].at > lines[0].at, "lines written apart arrive apart");
    }

    fn traceparent_seen_by_harness() -> String {
        let invocation = AgentInvocation::begin(Runner::ClaudeCode, None);
        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg("printf '%s' \"$TRACEPARENT\"")
            .env_remove("TRACEPARENT");
        let captured = invocation.capture(&mut command, Some(Duration::from_secs(10))).unwrap();
        String::from_utf8(captured.stdout).unwrap()
    }

    #[test]
    fn the_harness_is_handed_a_traceparent_only_while_tracing() {
        assert_eq!(traceparent_seen_by_harness(), "");

        let (seen, trace) = capture(traceparent_seen_by_harness);
        let span = trace.span("invoke_agent claude-code").expect("invoke_agent exported");
        assert_eq!(
            seen,
            format!("00-{}-{}-01", span.span_context.trace_id(), span.span_context.span_id())
        );
    }

    #[test]
    fn invoke_agent_carries_the_process_and_what_the_harness_reported() {
        let ((), trace) = capture(|| {
            let invocation = AgentInvocation::begin(Runner::ClaudeCode, Some("claude-sonnet-4-5"));
            let mut command = Command::new("bash");
            command
                .arg("-c")
                .arg(r#"echo '{"type":"result","is_error":false,"duration_ms":40,"duration_api_ms":30}'"#);
            let captured = invocation.capture(&mut command, Some(Duration::from_secs(10))).unwrap();
            let outcome = completed_outcome(
                40,
                captured.exit_code,
                HarnessTokenUsage::reported(Some(10), Some(5), None),
                Some(RunCost::Priced { usd: 0.25 }),
                String::new(),
            );
            invocation.finish(&captured, &outcome, Some("claude-sonnet-4-5"), HarnessTraces::Silent);
        });

        let name = "invoke_agent claude-code";
        let span = trace.span(name).unwrap();
        assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Client);
        assert_eq!(
            attribute(&trace, name, GEN_AI_OPERATION_NAME).as_deref(),
            Some("invoke_agent")
        );
        assert_eq!(
            attribute(&trace, name, GEN_AI_PROVIDER_NAME).as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            attribute(&trace, name, GEN_AI_AGENT_NAME).as_deref(),
            Some("claude-code")
        );
        assert_eq!(
            attribute(&trace, name, GEN_AI_REQUEST_MODEL).as_deref(),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(
            trace.attribute(name, GEN_AI_USAGE_INPUT_TOKENS),
            Some(opentelemetry::Value::I64(10))
        );
        assert_eq!(attribute(&trace, name, EVAL_COST_USD).as_deref(), Some("0.25"));
        assert_eq!(
            trace.attribute(name, EVAL_HARNESS_DURATION_MS),
            Some(opentelemetry::Value::I64(40))
        );
        assert_eq!(
            trace.attribute(name, EVAL_HARNESS_API_DURATION_MS),
            Some(opentelemetry::Value::I64(30))
        );
        assert_eq!(attribute(&trace, name, PROCESS_EXIT_CODE).as_deref(), Some("0"));
        assert_eq!(
            attribute(&trace, name, PROCESS_EXECUTABLE_NAME).as_deref(),
            Some("claude")
        );
        assert!(matches!(
            trace.attribute(name, PROCESS_PID),
            Some(opentelemetry::Value::I64(_))
        ));
        assert_eq!(attribute(&trace, name, ERROR_TYPE), None);
    }

    #[test]
    fn a_timeout_records_its_signals_on_invoke_agent_and_fails_it() {
        let ((), trace) = capture(|| {
            let invocation = AgentInvocation::begin(Runner::Codex, None);
            let mut command = Command::new("bash");
            command.arg("-c").arg("trap '' TERM; sleep 30");
            let captured = invocation.capture(&mut command, Some(Duration::from_secs(1))).unwrap();
            assert!(captured.timed_out);
            let outcome = timeout_outcome(Runner::Codex, 1000, captured.exit_code);
            invocation.finish(&captured, &outcome, None, HarnessTraces::Silent);
        });

        let name = "invoke_agent codex";
        let span = trace.span(name).unwrap();
        let events: Vec<&str> = span.events.iter().map(|event| event.name.as_ref()).collect();
        assert_eq!(events, vec!["SIGTERM", "SIGKILL"]);
        assert_eq!(attribute(&trace, name, ERROR_TYPE).as_deref(), Some("timeout"));
        assert!(matches!(span.status, opentelemetry::trace::Status::Error { .. }));
    }

    #[test]
    fn a_failing_step_is_marked_with_its_error_type() {
        let (result, trace) = capture(|| {
            in_step("stage fixtures", || -> Result<(), RunnerError> {
                Err(RunnerError::Io(std::io::Error::other("gone")))
            })
        });
        assert!(result.is_err());
        assert_eq!(attribute(&trace, "stage fixtures", ERROR_TYPE).as_deref(), Some("io"));
    }
}
