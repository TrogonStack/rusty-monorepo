use std::process::Command;

use super::{
    capture_subprocess, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file,
    EvalRunOutcome, EvalRunRequest, RunStatus, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::outputs::{cleanup_runner_temp_files, persist_final_markdown};
use crate::agentskills::redact::{redact_command_args, redact_env};

const PROGRAM: &str = "cursor-agent";
const INSTALL_HINT: &str = "install Cursor Agent CLI and ensure `cursor-agent` is on PATH";

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request)?;

    let mut command = Command::new(PROGRAM);
    command
        .arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--force")
        .arg("--workspace")
        .arg(request.workspace_dir);

    if let Some(model) = request.runner_model {
        command.arg("--model").arg(model);
    }

    command.arg(&prepared.prompt);

    let mut cmd_args = vec![
        "-p",
        "--output-format",
        "stream-json",
        "--force",
        "--workspace",
        request.workspace_dir.to_str().unwrap_or("."),
    ];
    if let Some(model) = request.runner_model {
        cmd_args.push("--model");
        cmd_args.push(model);
    }
    cmd_args.push(&prepared.prompt);
    if let Some(run_dir) = request.transcript_path.parent() {
        write_runner_invocation_metadata(
            run_dir,
            redact_command_args(PROGRAM, &cmd_args),
            redact_env(),
        )?;
    }

    let captured = capture_subprocess(&mut command, timeout_duration(request.timeout_secs))?;
    persist_runner_io(request, &captured)?;

    if captured.timed_out {
        let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
        let outcome = timeout_outcome(timeout_ms, captured.exit_code);
        write_timing(request, &outcome)?;
        return Ok(outcome);
    }

    let exit_ok = captured.exit_code == Some(0);
    let outcome = parse_outcome(&captured.stdout, captured.duration_ms, exit_ok, captured.exit_code);
    if matches!(outcome.status, RunStatus::Completed) {
        persist_final_markdown(request.workspace_dir, &outcome.final_text).map_err(|e| RunnerError::InvalidOutput {
            program: PROGRAM.to_string(),
            detail: e.to_string(),
        })?;
    }
    cleanup_runner_temp_files(request.workspace_dir)?;
    write_timing(request, &outcome)?;
    Ok(outcome)
}

fn write_timing(request: &EvalRunRequest, outcome: &EvalRunOutcome) -> Result<(), RunnerError> {
    write_timing_file(
        &request
            .transcript_path
            .parent()
            .unwrap_or(request.workspace_dir)
            .join("timing.json"),
        outcome,
    )
    .map_err(RunnerError::from)
}

fn parse_outcome(stdout: &[u8], wall_ms: u64, exit_ok: bool, exit_code: Option<i32>) -> EvalRunOutcome {
    let text = match std::str::from_utf8(stdout) {
        Ok(text) => text,
        Err(_) => {
            return runner_failure_outcome(wall_ms, exit_code, String::new());
        }
    };

    let mut last_result: Option<serde_json::Value> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("result") {
            last_result = Some(value);
        }
    }

    let Some(result) = last_result else {
        return runner_failure_outcome(wall_ms, exit_code, String::new());
    };

    let is_error = !exit_ok || result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    let final_text = result
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let duration_ms = result.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(wall_ms);

    if is_error {
        return runner_failure_outcome(duration_ms, exit_code, final_text);
    }

    let usage = result.get("usage");
    let input_tokens = usage.and_then(|u| u.get("inputTokens")).and_then(|v| v.as_u64());
    let output_tokens = usage.and_then(|u| u.get("outputTokens")).and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(i), Some(o)) => Some(i + o),
        (Some(i), None) => Some(i),
        (None, Some(o)) => Some(o),
        (None, None) => None,
    };

    completed_outcome(
        duration_ms,
        exit_code,
        total_tokens,
        input_tokens,
        output_tokens,
        None,
        final_text,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_terminal_result_event() {
        let stdout = br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"duration_ms":1234,"duration_api_ms":1100,"result":"hello","usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":0}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 1234);
        assert_eq!(outcome.input_tokens, Some(100));
        assert_eq!(outcome.output_tokens, Some(50));
        assert_eq!(outcome.total_tokens, Some(150));
        assert_eq!(outcome.final_text, "hello");
    }

    #[test]
    fn marks_failed_when_is_error_true() {
        let stdout = br#"{"type":"result","is_error":true,"duration_ms":10,"result":"boom"}
"#;
        let outcome = parse_outcome(stdout, 0, true, Some(0));
        assert!(matches!(outcome.status, RunStatus::Failed));
        assert_eq!(outcome.failure_kind, Some(super::super::FAILURE_KIND_RUNNER));
    }

    #[test]
    fn non_zero_exit_is_runner_failure() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":10,"result":"ok"}
"#;
        let outcome = parse_outcome(stdout, 0, false, Some(1));
        assert!(matches!(outcome.status, RunStatus::Failed));
        assert_eq!(outcome.failure_kind, Some(super::super::FAILURE_KIND_RUNNER));
    }

    #[test]
    fn missing_result_event_is_runner_failure() {
        let stdout = br#"{"type":"system"}
"#;
        let outcome = parse_outcome(stdout, 0, true, Some(0));
        assert!(matches!(outcome.status, RunStatus::Failed));
        assert_eq!(outcome.failure_kind, Some(super::super::FAILURE_KIND_RUNNER));
    }
}
