mod constants;
use constants::{INSTALL_HINT, PROGRAM, USAGE_FIELDS};
use std::ffi::OsString;
use std::process::Command;

use serde_json::Value;

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
use crate::agentskills::transcript::{pi_assistant_message, pi_stop_failed};

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

/// `--mode json -p` prints pi's event stream for one prompt and exits; `--no-session` keeps
/// the run's session out of whichever config directory it was given.
///
/// pi has no permission system to translate a grant into: every tool it offers runs
/// without asking, which is why `effective_permission_grant` reports every pi run as
/// unrestricted.
///
/// The prompt follows `--`, so a prompt that begins with a dash reaches the model as text.
/// The workspace is the working directory the command starts in.
pub(crate) fn build_args(model: Option<&str>, prompt: &str) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--mode"),
        OsString::from("json"),
        OsString::from("-p"),
        OsString::from("--no-session"),
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
    let prepared = prepare_workspace(request, Runner::Pi)?;

    let args = build_args(request.runner_model, &prepared.prompt);

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

    let invocation = AgentInvocation::begin(Runner::Pi, request.runner_model);
    let captured = invocation.capture(&mut command, timeout_duration(request.timeout_secs), request.caps)?;
    let outcome = in_step("parse outcome", || {
        if let Some(cap) = captured.cap_reached {
            return Ok(budget_exhausted_outcome(
                Runner::Pi,
                cap,
                captured.duration_ms,
                captured.exit_code,
            ));
        }
        if captured.timed_out {
            let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
            return Ok(timeout_outcome(Runner::Pi, timeout_ms, captured.exit_code));
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
    persist_runner_io(Runner::Pi, request, &captured)?;

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

/// Read the run from pi's `--mode json` event stream.
///
/// pi writes no closing summary: each model call ends in an assistant `message_end`
/// carrying that call's usage, so the run's usage is their sum and its answer is the text
/// of the last one. pi exits zero even when the provider refused it, so a run failed when
/// its last assistant message stopped on `error` or `aborted`, as well as when pi exited
/// nonzero or never produced an answer at all.
///
/// pi reports no duration for the run, so the wall time trg measured stands in.
fn parse_outcome(stdout: &[u8], wall_ms: u64, exit_code: Option<i32>) -> EvalRunOutcome {
    let Ok(text) = std::str::from_utf8(stdout) else {
        return runner_failure_outcome(Runner::Pi, wall_ms, exit_code, String::new());
    };

    let mut usage = CallUsage::default();
    let mut last = None;
    // pi frames on LF alone; a Unicode line separator inside a string is not a boundary.
    let messages = text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|event| pi_assistant_message(&event).cloned());
    for message in messages {
        usage.add(message.get("usage"));
        last = Some(message);
    }

    let Some(last) = last else {
        return runner_failure_outcome(Runner::Pi, wall_ms, exit_code, String::new());
    };
    let answer = message_text(&last);
    if exit_code != Some(0) || pi_stop_failed(&last) {
        let detail = last
            .get("errorMessage")
            .and_then(Value::as_str)
            .map_or(answer, str::to_string);
        return runner_failure_outcome(Runner::Pi, wall_ms, exit_code, detail);
    }

    let tokens = HarnessTokenUsage::read(Some(&usage.block()), Runner::Pi, USAGE_FIELDS);
    completed_outcome(wall_ms, exit_code, tokens, Runner::Pi.pricing().price(None), answer)
}

/// The text blocks of an assistant message, in order.
fn message_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The run's usage, added up from each model call's.
///
/// A count stays absent until some call reports it. pi's `output` already covers the
/// reasoning the model generated, which it also reports apart as `reasoning`, so that
/// figure is not added a second time.
#[derive(Default)]
struct CallUsage {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
}

impl CallUsage {
    fn add(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else { return };
        let count = |key: &str| usage.get(key).and_then(Value::as_u64);
        let sum = |total: &mut Option<u64>, value: Option<u64>| {
            if let Some(value) = value {
                *total = Some(total.unwrap_or(0).saturating_add(value));
            }
        };
        sum(&mut self.input, count("input"));
        sum(&mut self.output, count("output"));
        sum(&mut self.cache_read, count("cacheRead"));
        sum(&mut self.cache_write, count("cacheWrite"));
    }

    fn block(&self) -> Value {
        let mut block = serde_json::Map::new();
        for (name, value) in [
            (USAGE_FIELDS.input, self.input),
            (USAGE_FIELDS.output, self.output),
            (USAGE_FIELDS.cache_read, self.cache_read),
            (USAGE_FIELDS.cache_write, self.cache_write),
        ] {
            if let Some(value) = value {
                block.insert(name.to_string(), Value::from(value));
            }
        }
        Value::Object(block)
    }
}

#[cfg(test)]
#[path = "pi_tests.rs"]
mod tests;
