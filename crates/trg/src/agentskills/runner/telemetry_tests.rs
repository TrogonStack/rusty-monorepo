use super::*;
use crate::agentskills::runner::{capture_subprocess, completed_outcome, timeout_outcome, HarnessTokenUsage};
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

#[test]
fn a_harness_killed_at_its_time_limit_ends_after_its_last_line_and_the_limit() {
    let mut command = Command::new("bash");
    command.arg("-c").arg("trap '' TERM; echo started; sleep 30");
    let captured = capture_subprocess(&mut command, Some(Duration::from_secs(1))).unwrap();

    assert!(captured.timed_out);
    let last_line = captured.stdout_timeline.lines(&captured.stdout).last().unwrap().at;
    assert!(captured.ended_at > last_line);
    assert!(captured.ended_at >= captured.started_at + Duration::from_secs(1));
}

fn traceparent_seen_by_harness() -> String {
    let invocation = AgentInvocation::begin(Runner::ClaudeCode, None);
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg("printf '%s' \"$TRACEPARENT\"")
        .env_remove("TRACEPARENT");
    let captured = invocation
        .capture(&mut command, Some(Duration::from_secs(10)), RunCaps::default())
        .unwrap();
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
        let captured = invocation
            .capture(&mut command, Some(Duration::from_secs(10)), RunCaps::default())
            .unwrap();
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
        let captured = invocation
            .capture(&mut command, Some(Duration::from_secs(1)), RunCaps::default())
            .unwrap();
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
