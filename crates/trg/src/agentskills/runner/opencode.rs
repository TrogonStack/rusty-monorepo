mod constants;
use constants::{INSTALL_HINT, PROGRAM, USAGE_FIELDS};
use std::ffi::OsString;
use std::process::Command;

use serde_json::Value;

use super::capabilities::HarnessControl;
use super::telemetry::{in_step, AgentInvocation, HarnessTraces};
use super::usage::{HarnessTokenUsage, SummedCount, UsageFieldNames};
use super::{
    budget_exhausted_outcome, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file,
    EvalRunOutcome, EvalRunRequest, RunStatus, Runner, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::outputs::{cleanup_runner_temp_files, persist_final_markdown};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::PermissionGrant;
use crate::agentskills::transcript::{OPENCODE_ERROR_TYPE, OPENCODE_STEP_FINISH_TYPE, OPENCODE_STEP_START_TYPE};

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

/// `--standalone` gives the run a private server. Without it `opencode run` attaches to a
/// background service shared across invocations, which started with whatever environment
/// the first caller had, so a run's environment policy and config home would not hold.
///
/// `--archived` keeps the run's session out of session lists. An isolated run's data
/// directory is thrown away with its run directory, and a scrubbed or inherited run would
/// otherwise add a session to the operator's own list.
///
/// opencode allows edits and commands inside its project by default and asks about the
/// rest; with no terminal, a question is refused, which is the closest it comes to
/// codex's `workspace-write`. `--auto` approves every request no rule denies.
///
/// The prompt follows `--`, so a prompt that begins with a dash reaches the model as text.
/// The workspace is the working directory the command starts in.
pub(crate) fn build_args(model: Option<&str>, permission: PermissionGrant, prompt: &str) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("run"),
        OsString::from("--format"),
        OsString::from("json"),
        OsString::from("--standalone"),
        OsString::from("--archived"),
    ];
    if matches!(permission, PermissionGrant::Unrestricted) {
        let sandbox_flag = Runner::OpenCode
            .support(HarnessControl::SandboxLevels)
            .flag()
            .expect("the capability matrix declares opencode takes its sandbox level as a flag");
        args.push(OsString::from(sandbox_flag));
    }
    if let Some(model) = model {
        args.push(OsString::from("--model"));
        args.push(OsString::from(model));
    }
    args.push(OsString::from("--"));
    args.push(OsString::from(prompt));
    args
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request, Runner::OpenCode)?;

    let args = build_args(request.runner_model, request.permission, &prepared.prompt);

    let mut command = Command::new(PROGRAM);
    command.current_dir(request.workspace_dir).args(&args);

    in_step("write runner invocation", || {
        let recorded: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let borrowed: Vec<&str> = recorded.iter().map(String::as_str).collect();
        write_runner_invocation_metadata(
            request.run_dir(),
            redact_command_args(PROGRAM, &borrowed),
            prepared.environment.record(),
        )?;
        Ok(())
    })?;

    prepared.environment.apply(&mut command);

    let invocation = AgentInvocation::begin(Runner::OpenCode, request.runner_model);
    let captured = invocation.capture(&mut command, timeout_duration(request.timeout_secs), request.caps)?;
    let outcome = in_step("parse outcome", || {
        if let Some(cap) = captured.cap_reached {
            return Ok(budget_exhausted_outcome(
                Runner::OpenCode,
                cap,
                captured.duration_ms,
                captured.exit_code,
            ));
        }
        if captured.timed_out {
            let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
            return Ok(timeout_outcome(Runner::OpenCode, timeout_ms, captured.exit_code));
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
    persist_runner_io(Runner::OpenCode, request, &captured)?;

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
        write_timing_file(&request.run_dir().join("timing.json"), &outcome).map_err(RunnerError::from)
    })?;
    Ok(outcome)
}

/// Read the run from opencode's `--format json` event stream.
///
/// opencode writes no closing summary: each model call is a `step_start` and a
/// `step_finish` carrying that call's tokens, so the run's usage is their sum, and its
/// answer is the text of the last step. A run failed when opencode exited nonzero or
/// streamed an `error` event; opencode exits nonzero for a provider error, but the event
/// is what names it, so either counts.
///
/// opencode reports no duration of its own, so the wall time trg measured stands in.
fn parse_outcome(stdout: &[u8], wall_ms: u64, exit_code: Option<i32>) -> EvalRunOutcome {
    let Ok(text) = std::str::from_utf8(stdout) else {
        return runner_failure_outcome(Runner::OpenCode, wall_ms, exit_code, String::new());
    };

    let mut last_step_text = String::new();
    let mut usage = StepUsage::default();
    let mut error = None;
    let mut saw_step = false;
    let events = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok());
    for event in events {
        match event.get("type").and_then(Value::as_str) {
            Some(OPENCODE_STEP_START_TYPE) => {
                saw_step = true;
                last_step_text.clear();
            }
            Some("text") => {
                if let Some(chunk) = event.pointer("/part/text").and_then(Value::as_str) {
                    if !last_step_text.is_empty() {
                        last_step_text.push('\n');
                    }
                    last_step_text.push_str(chunk);
                }
            }
            Some(OPENCODE_STEP_FINISH_TYPE) => usage.add(event.pointer("/part/tokens")),
            Some(OPENCODE_ERROR_TYPE) => {
                error = Some(
                    event
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("opencode reported an error")
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    if exit_code != Some(0) || error.is_some() || !saw_step {
        let detail = error.unwrap_or(last_step_text);
        return runner_failure_outcome(Runner::OpenCode, wall_ms, exit_code, detail);
    }

    let tokens = HarnessTokenUsage::read(Some(&usage.block()), Runner::OpenCode, USAGE_FIELDS);
    completed_outcome(
        wall_ms,
        exit_code,
        tokens,
        Runner::OpenCode.pricing().price(None),
        last_step_text,
    )
}

/// The run's usage, added up from the per-step `tokens` blocks.
///
/// A count stays absent until some step reports it, so a run whose provider reported no
/// cache figures reports none rather than a zero. opencode counts reasoning apart from
/// output; it is output the model generated, so it is folded into the output count.
#[derive(Default)]
struct StepUsage {
    input: SummedCount,
    output: SummedCount,
    cache_read: SummedCount,
    cache_write: SummedCount,
}

impl StepUsage {
    fn add(&mut self, tokens: Option<&Value>) {
        let Some(tokens) = tokens else { return };
        self.input.add(tokens.pointer("/input"));
        self.output.add(tokens.pointer("/output"));
        self.output.add(tokens.pointer("/reasoning"));
        self.cache_read.add(tokens.pointer("/cache/read"));
        self.cache_write.add(tokens.pointer("/cache/write"));
    }

    fn block(&self) -> Value {
        let mut block = serde_json::Map::new();
        for (name, count) in [
            (USAGE_FIELDS.input, &self.input),
            (USAGE_FIELDS.output, &self.output),
            (USAGE_FIELDS.cache_read, &self.cache_read),
            (USAGE_FIELDS.cache_write, &self.cache_write),
        ] {
            if let Some(value) = count.block_value() {
                block.insert(name.to_string(), value);
            }
        }
        Value::Object(block)
    }
}

#[cfg(test)]
#[path = "opencode_tests.rs"]
mod tests;
