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

use super::harness_spans::{self, HarnessStream, Reconstruction, StreamEnd};
use super::{capture_capped, CapWatch, CapturedProcess, EvalRunOutcome, RunStatus, Runner, RunnerError};
use crate::agentskills::budget::RunCaps;
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
    /// A cap on the run's tool calls or turns stopped the harness.
    BudgetExhausted,
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
            Self::BudgetExhausted => "budget_exhausted",
        }
    }

    pub(crate) fn of_exit(code: Option<i32>) -> Option<Self> {
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
            Self::InvalidOutput { .. } | Self::InvalidOutputSource { .. } => FailureType::InvalidOutput,
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
        caps: RunCaps,
    ) -> Result<CapturedProcess, RunnerError> {
        let result = self.span.in_scope(|| {
            inject_std_command(command);
            capture_capped(command, timeout, CapWatch::of(self.runner, caps))
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
                ended: if captured.cap_reached.is_some() {
                    StreamEnd::CapReached(captured.ended_at)
                } else if captured.timed_out {
                    StreamEnd::TimedOut(captured.ended_at)
                } else {
                    StreamEnd::Exited(captured.ended_at)
                },
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
        let selected_items = [
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
        ];
        for (key, value) in selected_items {
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
            RunStatus::BudgetExhausted => Some(FailureType::BudgetExhausted),
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

        let selected_items = reconstruction.steps.iter().filter(|step| step.is_tool());
        for step in selected_items {
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
#[path = "telemetry_tests.rs"]
mod tests;
