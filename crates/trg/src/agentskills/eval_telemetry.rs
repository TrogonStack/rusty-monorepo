//! Spans and metrics shared by the steps of an eval pass.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use tracing::field::Empty;

use super::budget::RunCost;
use super::exit_code::ExitCode;
use super::report::RunRecord;
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, GEN_AI_OPERATION_NAME, GEN_AI_WORKFLOW_NAME,
};
use crate::telemetry::semconv::generated::metrics::GEN_AI_INVOKE_WORKFLOW_DURATION;
use crate::telemetry::semconv::trg::{
    EVAL_ATTEMPTS_METRIC, EVAL_ATTEMPT_OUTCOME, EVAL_CACHE_HIT, EVAL_CACHE_LOOKUPS_METRIC, EVAL_COST_METRIC,
    EVAL_RUNS_METRIC, EVAL_RUN_STATUS, EVAL_SCENARIO,
};

const METER: &str = "trg";

/// One step of a pass, named `{verb} {object}`.
pub fn step_span(name: &'static str) -> tracing::Span {
    tracing::info_span!(
        "eval step",
        "otel.name" = name,
        "otel.status_code" = Empty,
        { ERROR_TYPE } = Empty,
    )
}

/// Marks `span` failed with a low-cardinality `error_type`.
pub fn record_failure(span: &tracing::Span, error_type: &str) {
    span.record(ERROR_TYPE, error_type);
    span.record("otel.status_code", "ERROR");
}

/// `error.type` for a pass or step that ended with `code`, or `None` when it succeeded.
pub fn exit_error_type(code: ExitCode) -> Option<&'static str> {
    match code {
        ExitCode::Success => None,
        ExitCode::GateFailed => Some("gate_failed"),
        ExitCode::BudgetExhausted => Some("budget_exhausted"),
        ExitCode::InfrastructureFailure => Some("infrastructure_failure"),
        ExitCode::Interrupted(_) => Some("interrupted"),
    }
}

/// How one harness invocation of a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Completed,
    TransientFailure,
    RunnerError,
}

impl AttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::TransientFailure => "transient_failure",
            Self::RunnerError => "runner_error",
        }
    }

    pub fn is_transient(self) -> bool {
        matches!(self, Self::TransientFailure)
    }
}

/// `gen_ai.invoke_workflow.duration` for a whole pass, in seconds.
///
/// `workflow` is `None` for a pass that stopped before it learned the skill's name.
pub fn record_workflow_duration(workflow: Option<&str>, elapsed: Duration, outcome: ExitCode) {
    let mut attributes = vec![KeyValue::new(
        GEN_AI_OPERATION_NAME,
        gen_ai_operation_name::INVOKE_WORKFLOW,
    )];
    if let Some(workflow) = workflow {
        attributes.push(KeyValue::new(GEN_AI_WORKFLOW_NAME, workflow.to_string()));
    }
    if let Some(error_type) = exit_error_type(outcome) {
        attributes.push(KeyValue::new(ERROR_TYPE, error_type));
    }
    opentelemetry::global::meter(METER)
        .f64_histogram(GEN_AI_INVOKE_WORKFLOW_DURATION)
        .with_unit("s")
        .with_description("Duration of an eval pass")
        .build()
        .record(elapsed.as_secs_f64(), &attributes);
}

pub fn record_cache_lookup(hit: bool) {
    opentelemetry::global::meter(METER)
        .u64_counter(EVAL_CACHE_LOOKUPS_METRIC)
        .with_unit("{lookup}")
        .with_description("Run cache lookups, by whether they hit")
        .build()
        .add(1, &[KeyValue::new(EVAL_CACHE_HIT, hit)]);
}

pub fn record_attempt(outcome: AttemptOutcome) {
    opentelemetry::global::meter(METER)
        .u64_counter(EVAL_ATTEMPTS_METRIC)
        .with_unit("{attempt}")
        .with_description("Harness invocations, by how they ended")
        .build()
        .add(1, &[KeyValue::new(EVAL_ATTEMPT_OUTCOME, outcome.as_str())]);
}

/// One finished run, counted by status, with what it cost when the harness priced it.
///
/// A run served from cache spent nothing this pass, so its recorded cost is not counted
/// again.
pub fn record_run(run: &RunRecord) {
    let attributes = [
        KeyValue::new(EVAL_RUN_STATUS, run.status.clone()),
        KeyValue::new(EVAL_SCENARIO, run.scenario_id.as_str()),
    ];
    let meter = opentelemetry::global::meter(METER);
    meter
        .u64_counter(EVAL_RUNS_METRIC)
        .with_unit("{run}")
        .with_description("Eval runs, by final status")
        .build()
        .add(1, &attributes);
    let spent = run
        .cache
        .is_none()
        .then(|| run.metrics.cost.as_ref().and_then(RunCost::usd))
        .flatten()
        .filter(|usd| usd.is_finite() && *usd >= 0.0);
    if let Some(usd) = spent {
        meter
            .f64_counter(EVAL_COST_METRIC)
            .with_unit("USD")
            .with_description("What eval runs cost, as the harness priced them")
            .build()
            .add(usd, &attributes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_outcomes_are_the_values_the_registry_documents() {
        let emitted = [
            AttemptOutcome::Completed,
            AttemptOutcome::TransientFailure,
            AttemptOutcome::RunnerError,
        ]
        .map(AttemptOutcome::as_str);

        assert_eq!(emitted, ["completed", "transient_failure", "runner_error"]);
    }

    #[test]
    fn only_a_transient_failure_is_transient() {
        assert!(AttemptOutcome::TransientFailure.is_transient());
        assert!(!AttemptOutcome::Completed.is_transient());
        assert!(!AttemptOutcome::RunnerError.is_transient());
    }
}
