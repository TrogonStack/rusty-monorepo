use std::process::Command;

use super::{
    capture_subprocess, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file,
    EvalRunOutcome, EvalRunRequest, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::outputs::{cleanup_runner_temp_files, outputs_dir, path_within_base, FINAL_MD};
use crate::agentskills::redact::{redact_command_args, redact_env};

const PROGRAM: &str = "codex";
const INSTALL_HINT: &str = "install Codex CLI and ensure `codex` is on PATH";

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request)?;
    let final_text_path = outputs_dir(request.workspace_dir).join(FINAL_MD);
    path_within_base(request.workspace_dir, &final_text_path).map_err(|e| RunnerError::InvalidOutput {
        program: PROGRAM.to_string(),
        detail: e.to_string(),
    })?;

    let mut command = Command::new(PROGRAM);
    command
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg("-s")
        .arg("workspace-write")
        .arg("-C")
        .arg(request.workspace_dir)
        .arg("-o")
        .arg(&final_text_path);

    if let Some(model) = request.runner_model {
        command.arg("-m").arg(model);
    }

    command.arg(&prepared.prompt);

    let mut cmd_args = vec![
        "exec",
        "--json",
        "--skip-git-repo-check",
        "-s",
        "workspace-write",
        "-C",
        request.workspace_dir.to_str().unwrap_or("."),
        "-o",
        final_text_path.to_str().unwrap_or("outputs/final.md"),
    ];
    if let Some(model) = request.runner_model {
        cmd_args.push("-m");
        cmd_args.push(model);
    }
    cmd_args.push(&prepared.prompt);
    if let Some(run_dir) = request.transcript_path.parent() {
        write_runner_invocation_metadata(run_dir, redact_command_args(PROGRAM, &cmd_args), redact_env())?;
    }

    let captured = capture_subprocess(&mut command, timeout_duration(request.timeout_secs))?;
    persist_runner_io(request, &captured)?;

    if captured.timed_out {
        let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
        let outcome = timeout_outcome(timeout_ms, captured.exit_code);
        write_timing(request, &outcome)?;
        return Ok(outcome);
    }

    let final_text = std::fs::read_to_string(&final_text_path).unwrap_or_default();
    let exit_ok = captured.exit_code == Some(0);
    let outcome = parse_outcome(
        &captured.stdout,
        captured.duration_ms,
        exit_ok,
        captured.exit_code,
        final_text,
    );
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

fn parse_outcome(
    stdout: &[u8],
    wall_ms: u64,
    exit_ok: bool,
    exit_code: Option<i32>,
    final_text: String,
) -> EvalRunOutcome {
    let text = match std::str::from_utf8(stdout) {
        Ok(text) => text,
        Err(_) => {
            return runner_failure_outcome(wall_ms, exit_code, final_text);
        }
    };

    let mut terminal: Option<serde_json::Value> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if value.get("type").and_then(|v| v.as_str()) == Some("turn.completed") {
            terminal = Some(value);
        }
    }

    let Some(terminal) = terminal else {
        return runner_failure_outcome(wall_ms, exit_code, final_text);
    };

    if !exit_ok {
        return runner_failure_outcome(wall_ms, exit_code, final_text);
    }

    let usage = terminal.get("usage");
    let input_tokens = usage.and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64());
    let output_tokens = usage.and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64());
    let cached_input = usage
        .and_then(|u| u.get("cached_input_tokens"))
        .and_then(|v| v.as_u64());
    let total_tokens = match (input_tokens, output_tokens) {
        (None, None) => None,
        (i, o) => Some(i.unwrap_or(0) + o.unwrap_or(0) + cached_input.unwrap_or(0)),
    };

    completed_outcome(
        wall_ms,
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
    use super::super::RunStatus;
    use super::*;

    #[test]
    fn parses_turn_completed_event() {
        let stdout = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40,"cached_input_tokens":10}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 5000);
        assert_eq!(outcome.input_tokens, Some(120));
        assert_eq!(outcome.output_tokens, Some(40));
        assert_eq!(outcome.total_tokens, Some(170));
        assert_eq!(outcome.final_text, "final");
        assert_eq!(outcome.exit_code, Some(0));
    }

    #[test]
    fn missing_terminal_event_is_runner_failure() {
        let stdout = br#"{"type":"thread.started"}
"#;
        let outcome = parse_outcome(stdout, 0, true, Some(0), String::new());
        assert!(matches!(outcome.status, RunStatus::Failed));
        assert_eq!(outcome.failure_kind, Some(super::super::FAILURE_KIND_RUNNER));
    }
}
