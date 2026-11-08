mod constants;
use constants::{INSTALL_HINT, PROGRAM, USAGE_FIELDS};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use super::capabilities::HarnessControl;
use super::telemetry::{in_step, AgentInvocation, HarnessTraces};
use super::usage::{HarnessTokenUsage, UsageFieldNames};
use super::{
    budget_exhausted_outcome, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file,
    EvalRunOutcome, EvalRunRequest, RunStatus, Runner, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::outputs::{cleanup_runner_temp_files, persist_final_markdown};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::PermissionGrant;

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

/// Translate a grant into cursor-agent's own flags.
///
/// `--force` ("Force allow commands unless explicitly denied") is cursor-agent's only
/// documented non-interactive grant, so both levels collapse into it: cursor-agent draws no
/// boundary between them for us to translate. It also has `--sandbox enabled|disabled`, but
/// that flag's boundary is undocumented, so trg does not reach for it here.
///
/// `Runner::effective_permission_grant` is the authority on what running under `--force`
/// actually means; the assertion below exists so the two cannot silently drift apart if
/// cursor-agent ever grows a second grant one of them forgets to learn about.
fn permission_args(grant: PermissionGrant) -> [&'static str; 1] {
    debug_assert_eq!(
        Runner::CursorAgent.effective_permission_grant(grant),
        PermissionGrant::Unrestricted,
        "cursor-agent's only non-interactive grant is unrestricted, regardless of what was requested"
    );
    let flag = Runner::CursorAgent
        .support(HarnessControl::SandboxLevels)
        .flag()
        .expect("the capability matrix declares cursor-agent takes its sandbox level as a flag");
    [flag]
}

/// The arguments are `OsString` because one of them is a path, and a path is not always
/// valid UTF-8. Rendering it into a `String` to build the list would hand the harness a
/// lossily rewritten workspace to work in.
pub(crate) fn build_args(
    workspace_dir: &Path,
    model: Option<&str>,
    permission: PermissionGrant,
    prompt: &str,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-p"),
        OsString::from("--output-format"),
        OsString::from("stream-json"),
    ];
    args.extend(permission_args(permission).iter().map(OsString::from));
    args.push(OsString::from("--workspace"));
    args.push(workspace_dir.as_os_str().to_os_string());

    if let Some(model) = model {
        args.push(OsString::from("--model"));
        args.push(OsString::from(model));
    }

    args.push(OsString::from(prompt));
    args
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request, Runner::CursorAgent)?;

    let args = build_args(
        request.workspace_dir,
        request.runner_model,
        request.permission,
        &prepared.prompt,
    );

    let mut command = Command::new(PROGRAM);
    command.args(&args);

    in_step("write runner invocation", || {
        if let Some(run_dir) = request.transcript_path.parent() {
            let recorded: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
            let borrowed: Vec<&str> = recorded.iter().map(String::as_str).collect();
            write_runner_invocation_metadata(
                run_dir,
                redact_command_args(PROGRAM, &borrowed),
                prepared.environment.record(),
            )?;
        }
        Ok(())
    })?;

    prepared.environment.apply(&mut command);

    let invocation = AgentInvocation::begin(Runner::CursorAgent, request.runner_model);
    let captured = invocation.capture(&mut command, timeout_duration(request.timeout_secs), request.caps)?;
    let outcome = in_step("parse outcome", || {
        if let Some(cap) = captured.cap_reached {
            return Ok(budget_exhausted_outcome(
                Runner::CursorAgent,
                cap,
                captured.duration_ms,
                captured.exit_code,
            ));
        }
        if captured.timed_out {
            let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
            return Ok(timeout_outcome(Runner::CursorAgent, timeout_ms, captured.exit_code));
        }
        let exit_ok = captured.exit_code == Some(0);
        Ok(parse_outcome(
            &captured.stdout,
            captured.duration_ms,
            exit_ok,
            captured.exit_code,
        ))
    })?;
    invocation.finish(
        &captured,
        &outcome,
        request.runner_model,
        HarnessTraces::exported(prepared.environment.harness_exports_traces()),
    );
    persist_runner_io(Runner::CursorAgent, request, &captured)?;

    in_step("write outcome", || {
        if matches!(outcome.status, RunStatus::Completed) {
            persist_final_markdown(request.workspace_dir, &outcome.final_text).map_err(|e| {
                RunnerError::InvalidOutputSource {
                    program: PROGRAM.to_string(),
                    detail: e.to_string(),
                    source: Box::new(e),
                }
            })?;
        }
        if !captured.timed_out {
            cleanup_runner_temp_files(request.workspace_dir)?;
        }
        write_timing(request, &outcome)
    })?;
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
            return runner_failure_outcome(Runner::CursorAgent, wall_ms, exit_code, String::new());
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
        return runner_failure_outcome(Runner::CursorAgent, wall_ms, exit_code, String::new());
    };

    let is_error = !exit_ok || result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    let final_text = result
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let duration_ms = result.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(wall_ms);

    if is_error {
        return runner_failure_outcome(Runner::CursorAgent, duration_ms, exit_code, final_text);
    }

    let tokens = HarnessTokenUsage::read(result.get("usage"), Runner::CursorAgent, USAGE_FIELDS);

    completed_outcome(
        duration_ms,
        exit_code,
        tokens,
        Runner::CursorAgent.pricing().price(None),
        final_text,
    )
}

#[cfg(test)]
#[path = "cursor_agent_tests.rs"]
mod tests;
