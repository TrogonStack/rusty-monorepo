mod constants;
use constants::{INSTALL_HINT, PROGRAM, USAGE_FIELDS};
use std::ffi::OsString;
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
use crate::agentskills::transcript::{trogonai_agent_result_names_error, TROGONAI_AGENT_RESULT_TYPE};

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

/// Translate a grant into the `--permission-mode` value trogonai-agent accepts.
///
/// `auto` runs writes inside the workspace without asking and asks about anything it
/// cannot resolve as safe; with no terminal to ask, `ask` denies those and hands the model
/// the refusal, which is the closest trogonai-agent comes to codex's `workspace-write`. `yolo` (trogonai-agent's
/// full access) applies no permission rule at all.
fn permission_mode(grant: PermissionGrant) -> &'static str {
    match grant {
        PermissionGrant::WorkspaceWrite => "auto",
        PermissionGrant::Unrestricted => "yolo",
    }
}

/// `--no-save` keeps a run's session out of whichever profile the run was given: an
/// isolated run's profile is thrown away with its run directory, and a scrubbed or
/// inherited run would otherwise add a session to the operator's own history.
///
/// The prompt follows `--`, so a prompt that happens to begin with a dash reaches the model
/// as text instead of being refused as an unknown flag. The workspace is the working
/// directory the command starts in; trogonai-agent takes no flag naming it.
pub(crate) fn build_args(model: Option<&str>, permission: PermissionGrant, prompt: &str) -> Vec<OsString> {
    let sandbox_flag = Runner::TrogonaiAgent
        .support(HarnessControl::SandboxLevels)
        .flag()
        .expect("the capability matrix declares trogonai-agent takes its sandbox level as a flag");
    let mut args = vec![
        OsString::from("ask"),
        OsString::from("--output-format"),
        OsString::from("stream-json"),
        OsString::from("--no-save"),
        OsString::from(sandbox_flag),
        OsString::from(permission_mode(permission)),
    ];
    if let Some(model) = model {
        args.push(OsString::from("--model"));
        args.push(OsString::from(model));
    }
    args.push(OsString::from("--"));
    args.push(OsString::from(prompt));
    args
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request, Runner::TrogonaiAgent)?;

    let args = build_args(request.runner_model, request.permission, &prepared.prompt);

    let mut command = Command::new(PROGRAM);
    command.current_dir(request.workspace_dir).args(&args);

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

    let invocation = AgentInvocation::begin(Runner::TrogonaiAgent, request.runner_model);
    let captured = invocation.capture(&mut command, timeout_duration(request.timeout_secs), request.caps)?;
    let outcome = in_step("parse outcome", || {
        if let Some(cap) = captured.cap_reached {
            return Ok(budget_exhausted_outcome(
                Runner::TrogonaiAgent,
                cap,
                captured.duration_ms,
                captured.exit_code,
            ));
        }
        if captured.timed_out {
            let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
            return Ok(timeout_outcome(Runner::TrogonaiAgent, timeout_ms, captured.exit_code));
        }
        Ok(parse_outcome(
            &captured.stdout,
            captured.duration_ms,
            captured.exit_code,
        ))
    })?;
    invocation.finish(
        &captured,
        &outcome,
        request.runner_model,
        HarnessTraces::exported(prepared.environment.harness_exports_traces()),
    );
    persist_runner_io(Runner::TrogonaiAgent, request, &captured)?;

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
    write_timing_file(&request.run_dir().join("timing.json"), outcome).map_err(RunnerError::from)
}

/// Read the run from the `result` object trogonai-agent writes as its last line.
///
/// trogonai-agent reports no duration of its own, so the wall time trg measured stands in for it. A run
/// failed when trogonai-agent exited nonzero or its result names an `error`: trogonai-agent writes the result line
/// even for a run that never reached the model, such as a refused flag, so the line being
/// present says nothing about success on its own.
///
/// `final_output` is the model's last message, which is what the other runners report as
/// a run's answer. `output` also carries the text of every earlier step, so it is only the
/// fallback for a result that has no `final_output`.
fn parse_outcome(stdout: &[u8], wall_ms: u64, exit_code: Option<i32>) -> EvalRunOutcome {
    let Ok(text) = std::str::from_utf8(stdout) else {
        return runner_failure_outcome(Runner::TrogonaiAgent, wall_ms, exit_code, String::new());
    };

    // The result is the last line, so the stream is read from its end.
    let result = text
        .lines()
        .rev()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value.get("type").and_then(|v| v.as_str()) == Some(TROGONAI_AGENT_RESULT_TYPE));

    let Some(result) = result else {
        return runner_failure_outcome(Runner::TrogonaiAgent, wall_ms, exit_code, String::new());
    };

    let final_text = ["final_output", "output"]
        .into_iter()
        .find_map(|key| result.get(key).and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();

    let failed = exit_code != Some(0) || trogonai_agent_result_names_error(&result);
    if failed {
        return runner_failure_outcome(Runner::TrogonaiAgent, wall_ms, exit_code, final_text);
    }

    let usage = result.get("usage").map(without_unreported_counts);
    let tokens = HarnessTokenUsage::read(usage.as_ref(), Runner::TrogonaiAgent, USAGE_FIELDS);

    completed_outcome(
        wall_ms,
        exit_code,
        tokens,
        Runner::TrogonaiAgent.pricing().price(None),
        final_text,
    )
}

/// trogonai-agent writes a count the provider never reported as `null` rather than leaving it out:
/// its own record keeps a missing count apart from a zero, and `null` is how it says the
/// provider was silent. That is the absent count trg already has a reading for, not a
/// garbled one, so it is dropped before the block is read. Any other value that is not a
/// count is left in place for `HarnessTokenUsage` to report.
fn without_unreported_counts(usage: &serde_json::Value) -> serde_json::Value {
    let mut usage = usage.clone();
    if let Some(fields) = usage.as_object_mut() {
        fields.retain(|_, value| !value.is_null());
    }
    usage
}

#[cfg(test)]
#[path = "trogonai_agent_tests.rs"]
mod tests;
