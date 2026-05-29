use std::process::Command;

use super::{
    capture_subprocess, completed_outcome, persist_runner_io, prepare_workspace, runner_failure_outcome,
    timeout_duration, timeout_outcome, EvalRunOutcome, EvalRunRequest, RunnerError,
};

pub fn run_bash(request: &EvalRunRequest, script: &str) -> Result<EvalRunOutcome, RunnerError> {
    let _prepared = prepare_workspace(request)?;

    let mut command = Command::new("bash");
    command.arg("-c").arg(script).current_dir(request.workspace_dir);

    let captured = capture_subprocess(&mut command, timeout_duration(request.timeout_secs))?;
    persist_runner_io(request, &captured)?;

    if captured.timed_out {
        let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
        return Ok(timeout_outcome(timeout_ms, captured.exit_code));
    }

    let exit_ok = captured.exit_code == Some(0);
    let stdout = String::from_utf8_lossy(&captured.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&captured.stderr).into_owned();
    let final_text = if stdout.is_empty() {
        stderr.clone()
    } else {
        stdout.clone()
    };

    let has_result = stdout.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .and_then(|value| value.get("type").and_then(|v| v.as_str()).map(|t| t == "result"))
            .unwrap_or(false)
    });

    if !exit_ok || !has_result {
        return Ok(runner_failure_outcome(
            captured.duration_ms,
            captured.exit_code,
            final_text,
        ));
    }

    Ok(completed_outcome(
        captured.duration_ms,
        captured.exit_code,
        None,
        None,
        None,
        None,
        final_text,
    ))
}

pub fn is_transient_failure(outcome: &EvalRunOutcome) -> bool {
    outcome.is_transient_failure()
}

pub fn invoke_with_retries(
    request: &EvalRunRequest,
    script: &str,
    retries: u32,
) -> Result<(EvalRunOutcome, u32), RunnerError> {
    let max_attempts = retries.saturating_add(1);
    let mut invocations = 0u32;
    let mut last_outcome = None;

    for _ in 0..max_attempts {
        invocations += 1;
        let outcome = run_bash(request, script)?;
        if !is_transient_failure(&outcome) {
            return Ok((outcome, invocations));
        }
        last_outcome = Some(outcome);
    }

    Ok((last_outcome.expect("at least one invocation"), invocations))
}
