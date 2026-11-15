mod constants;
use constants::{INSTALL_HINT, MAX_TURNS_SUBTYPE, PROGRAM, USAGE_FIELDS};
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
use crate::agentskills::budget::{MaxTurns, RunCap};
use crate::agentskills::evals::EvalError;
use crate::agentskills::mocks::MaterializedMcpConfig;
use crate::agentskills::outputs::{cleanup_runner_temp_files, persist_final_markdown};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::PermissionGrant;
use crate::agentskills::system_prompt_appendix::SystemPromptAppendix;
use crate::agentskills::tool_grant::ToolGrant;

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

/// Translate a grant into the `--permission-mode` value claude accepts.
///
/// `acceptEdits` is the narrowest of claude's modes that still lets a run write its
/// deliverables without a prompt, which is what makes it the analogue of codex's
/// `workspace-write`. Claude has no mode meaning "decide for yourself"; that gap is what
/// `PermissionGrant` exists to close.
fn permission_mode(grant: PermissionGrant) -> &'static str {
    match grant {
        PermissionGrant::WorkspaceWrite => "acceptEdits",
        PermissionGrant::Unrestricted => "bypassPermissions",
    }
}

/// The arguments are `OsString`, kept uniform with the other runners even though claude
/// takes no path arguments today: a prompt or model string could still carry bytes that are
/// not valid UTF-8, and building the list as `OsString` from the start means that stays true
/// if a path argument is ever added here.
pub(crate) fn build_args(
    prompt: &str,
    model: Option<&str>,
    permission: PermissionGrant,
    tool_grant: Option<&ToolGrant>,
    mcp_config: Option<&Path>,
    append_system_prompt: Option<&SystemPromptAppendix>,
    max_turns: Option<MaxTurns>,
) -> Vec<OsString> {
    let sandbox_flag = Runner::ClaudeCode
        .support(HarnessControl::SandboxLevels)
        .flag()
        .expect("the capability matrix declares claude-code takes its sandbox level as a flag");
    let mut args = vec![
        OsString::from("-p"),
        OsString::from(prompt),
        OsString::from("--output-format"),
        OsString::from("stream-json"),
        OsString::from("--verbose"),
        OsString::from(sandbox_flag),
        OsString::from(permission_mode(permission)),
    ];
    if let Some(model) = model {
        args.push(OsString::from("--model"));
        args.push(OsString::from(model));
    }
    if let Some(appendix) = append_system_prompt {
        let append_flag = Runner::ClaudeCode
            .support(HarnessControl::SystemPromptAppend)
            .flag()
            .expect("the capability matrix declares claude-code takes its system prompt appendix as a flag");
        args.push(OsString::from(append_flag));
        args.push(OsString::from(appendix.as_str()));
    }
    if let Some(tool_grant) = tool_grant {
        let allowed_tools_flag = Runner::ClaudeCode
            .support(HarnessControl::ToolAllowlist)
            .flag()
            .expect("the capability matrix declares claude-code takes its tool allowlist as a flag");
        args.push(OsString::from(allowed_tools_flag));
        args.push(OsString::from(tool_grant.tools().collect::<Vec<_>>().join(",")));
    }
    if let Some(mcp_config) = mcp_config {
        let mcp_flag = Runner::ClaudeCode
            .support(HarnessControl::McpServers)
            .flag()
            .expect("the capability matrix declares claude-code takes its mcp servers as a flag");
        let guard_flag = Runner::ClaudeCode
            .support(HarnessControl::McpServers)
            .guard_flag()
            .expect("the capability matrix guards claude-code's mcp servers flag");
        args.push(OsString::from(mcp_flag));
        args.push(mcp_config.as_os_str().to_os_string());
        args.push(OsString::from(guard_flag));
    }
    if let Some(max_turns) = max_turns {
        let turn_cap_flag = Runner::ClaudeCode
            .support(HarnessControl::TurnCap)
            .flag()
            .expect("the capability matrix declares claude-code takes its turn cap as a flag");
        args.push(OsString::from(turn_cap_flag));
        args.push(OsString::from(max_turns.to_string()));
    }
    args
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request, Runner::ClaudeCode)?;

    let args = build_args(
        &prepared.prompt,
        request.runner_model,
        request.permission,
        request.tool_grant.as_ref(),
        request.mcp_config.as_ref().map(MaterializedMcpConfig::json),
        request.eval.append_system_prompt.as_ref(),
        request.caps.turns,
    );

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

    let invocation = AgentInvocation::begin(Runner::ClaudeCode, request.runner_model);
    let captured = invocation.capture(&mut command, timeout_duration(request.timeout_secs), request.caps)?;
    let outcome = in_step("parse outcome", || {
        if let Some(cap) = captured.cap_reached {
            return Ok(budget_exhausted_outcome(
                Runner::ClaudeCode,
                cap,
                captured.duration_ms,
                captured.exit_code,
            ));
        }
        if captured.timed_out {
            let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
            return Ok(timeout_outcome(Runner::ClaudeCode, timeout_ms, captured.exit_code));
        }
        let exit_ok = captured.exit_code == Some(0);
        Ok(parse_outcome(
            &captured.stdout,
            captured.duration_ms,
            exit_ok,
            captured.exit_code,
            request.caps.turns,
        ))
    })?;
    invocation.finish(
        &captured,
        &outcome,
        request.runner_model,
        HarnessTraces::exported(prepared.environment.harness_exports_traces()),
    );
    persist_runner_io(Runner::ClaudeCode, request, &captured)?;

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
        cleanup_runner_temp_files(request.workspace_dir)?;
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

fn parse_outcome(
    stdout: &[u8],
    wall_ms: u64,
    exit_ok: bool,
    exit_code: Option<i32>,
    max_turns: Option<MaxTurns>,
) -> EvalRunOutcome {
    let text = match std::str::from_utf8(stdout) {
        Ok(text) => text,
        Err(_) => {
            return runner_failure_outcome(Runner::ClaudeCode, wall_ms, exit_code, String::new());
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
        return runner_failure_outcome(Runner::ClaudeCode, wall_ms, exit_code, String::new());
    };

    let is_error = !exit_ok || result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
    let final_text = result
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let duration_ms = result.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(wall_ms);
    let tokens = HarnessTokenUsage::read(result.get("usage"), Runner::ClaudeCode, USAGE_FIELDS);
    let cost = Runner::ClaudeCode
        .pricing()
        .price(result.get("total_cost_usd").and_then(|v| v.as_f64()));

    if let Some(max_turns) = max_turns {
        if result.get("subtype").and_then(|v| v.as_str()) == Some(MAX_TURNS_SUBTYPE) {
            return EvalRunOutcome {
                tokens,
                cost,
                final_text,
                ..budget_exhausted_outcome(Runner::ClaudeCode, RunCap::Turns(max_turns), duration_ms, exit_code)
            };
        }
    }

    if is_error {
        return runner_failure_outcome(Runner::ClaudeCode, duration_ms, exit_code, final_text);
    }

    completed_outcome(duration_ms, exit_code, tokens, cost, final_text)
}

#[cfg(test)]
#[path = "claude_code_tests.rs"]
mod tests;
