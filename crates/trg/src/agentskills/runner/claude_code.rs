use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use super::capabilities::HarnessControl;
use super::{
    capture_subprocess, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, total_tokens_from, write_runner_invocation_metadata,
    write_timing_file, EvalRunOutcome, EvalRunRequest, RunStatus, Runner, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::outputs::{cleanup_runner_temp_files, persist_final_markdown};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::{CacheTokens, PermissionGrant};
use crate::agentskills::tool_grant::ToolGrant;

const PROGRAM: &str = "claude";
const INSTALL_HINT: &str = "install Claude Code and ensure `claude` is on PATH";

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
    args
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request, Runner::ClaudeCode)?;

    let args = build_args(
        &prepared.prompt,
        request.runner_model,
        request.permission,
        request.tool_grant.as_ref(),
        request.mcp_config_path.as_deref(),
    );

    let mut command = Command::new(PROGRAM);
    command.current_dir(request.workspace_dir).args(&args);

    if let Some(run_dir) = request.transcript_path.parent() {
        let recorded: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let borrowed: Vec<&str> = recorded.iter().map(String::as_str).collect();
        write_runner_invocation_metadata(
            run_dir,
            redact_command_args(PROGRAM, &borrowed),
            prepared.environment.recorded_vars(),
        )?;
    }

    prepared.environment.apply(&mut command);

    let captured = capture_subprocess(&mut command, timeout_duration(request.timeout_secs))?;
    persist_runner_io(Runner::ClaudeCode, request, &captured)?;

    if captured.timed_out {
        let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
        let outcome = timeout_outcome(timeout_ms, captured.exit_code);
        cleanup_runner_temp_files(request.workspace_dir)?;
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
    let input_tokens = usage.and_then(|u| u.get("input_tokens")).and_then(|v| v.as_u64());
    let output_tokens = usage.and_then(|u| u.get("output_tokens")).and_then(|v| v.as_u64());
    let cache_read = usage
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(|v| v.as_u64());
    let cache_creation = usage
        .and_then(|u| u.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_u64());
    let total_tokens = total_tokens_from(input_tokens, output_tokens);
    let cached_tokens = match (cache_read, cache_creation) {
        (None, None) => None,
        (read, write) => Some(CacheTokens::parse(read, write).expect("read or write is Some by the match arm")),
    };
    let cost_usd = result.get("total_cost_usd").and_then(|v| v.as_f64());

    completed_outcome(
        duration_ms,
        exit_code,
        total_tokens,
        input_tokens,
        output_tokens,
        cached_tokens,
        cost_usd,
        final_text,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_result_with_cost_and_cache_tokens() {
        let stdout = br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","total_cost_usd":0.0123,"usage":{"input_tokens":80,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":0}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 2000);
        assert_eq!(outcome.input_tokens, Some(80));
        assert_eq!(outcome.output_tokens, Some(20));
        // input + output only: cache reads and creations are billed separately and are
        // never folded into the total every runner agrees on.
        assert_eq!(outcome.total_tokens, Some(100));
        assert_eq!(
            outcome.cached_tokens,
            Some(CacheTokens::parse(Some(5), Some(0)).unwrap())
        );
        assert_eq!(outcome.cost_usd, Some(0.0123));
        assert_eq!(outcome.final_text, "final text");
    }

    #[test]
    fn a_result_with_no_cache_fields_reports_no_cache_activity() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","usage":{"input_tokens":80,"output_tokens":20}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(
            outcome.cached_tokens, None,
            "a harness that never mentions cache tokens is not the same as one that measured zero"
        );
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

    #[test]
    fn workspace_write_asks_claude_to_accept_edits_without_prompting() {
        assert_eq!(permission_mode(PermissionGrant::WorkspaceWrite), "acceptEdits");
    }

    #[test]
    fn unrestricted_asks_claude_to_bypass_permissions_entirely() {
        assert_eq!(permission_mode(PermissionGrant::Unrestricted), "bypassPermissions");
    }

    /// Regression test for the defect this change fixes: claude-code used to build its
    /// argument list with no permission flag at all, leaving what a run could do to
    /// whatever settings happened to be saved on the operator's machine. Checking the
    /// flag actually changes with the requested grant, rather than only that one fixed
    /// pairing is present, is what would have caught the flag being hardcoded to one
    /// value regardless of what was asked for.
    #[test]
    fn the_permission_mode_flag_actually_follows_the_requested_grant() {
        let mode_value_for = |grant: PermissionGrant| {
            let args = build_args("do the thing", None, grant, None, None);
            let position = args
                .windows(2)
                .position(|pair| pair[0] == "--permission-mode")
                .expect("the invocation carries a --permission-mode flag");
            args[position + 1].to_str().expect("test args are utf8").to_string()
        };

        assert_eq!(mode_value_for(PermissionGrant::WorkspaceWrite), "acceptEdits");
        assert_eq!(mode_value_for(PermissionGrant::Unrestricted), "bypassPermissions");
    }

    #[test]
    fn the_built_invocation_carries_the_matrixs_sandbox_flag() {
        let expected_flag = Runner::ClaudeCode
            .support(HarnessControl::SandboxLevels)
            .flag()
            .expect("claude-code declares a sandbox flag in the capability matrix");
        let args = build_args("do the thing", None, PermissionGrant::Unrestricted, None, None);
        let position = args
            .windows(2)
            .position(|pair| pair[0] == expected_flag)
            .expect("the invocation carries the flag the matrix declares");
        assert_eq!(args[position + 1], OsString::from("bypassPermissions"));
    }

    /// Regression test for the defect a lone `--mcp-config` would be: without
    /// `--strict-mcp-config` claude-code still falls back to whatever MCP servers happen to
    /// be configured on the machine running the eval, defeating the whole point of a mock.
    #[test]
    fn a_declared_mcp_config_always_carries_the_guard_flag_alongside_it() {
        let args = build_args(
            "do the thing",
            None,
            PermissionGrant::Unrestricted,
            None,
            Some(Path::new("/workspace/mcp-config.json")),
        );
        let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();
        let mcp_config_position = borrowed
            .iter()
            .position(|a| *a == "--mcp-config")
            .expect("the invocation carries --mcp-config");
        assert_eq!(borrowed[mcp_config_position + 1], "/workspace/mcp-config.json");
        assert!(
            borrowed.contains(&"--strict-mcp-config"),
            "a declared mcp config must never be handed to claude-code without --strict-mcp-config"
        );
    }

    #[test]
    fn no_declared_mcp_config_carries_neither_flag() {
        let args = build_args("do the thing", None, PermissionGrant::Unrestricted, None, None);
        let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();
        assert!(!borrowed.contains(&"--mcp-config"));
        assert!(!borrowed.contains(&"--strict-mcp-config"));
    }

    #[test]
    fn a_declared_tool_grant_is_carried_as_allowed_tools() {
        let grant = ToolGrant::parse(["Bash", "Read"]).unwrap();
        let args = build_args("do the thing", None, PermissionGrant::Unrestricted, Some(&grant), None);
        let position = args
            .windows(2)
            .position(|pair| pair[0] == "--allowedTools")
            .expect("the invocation carries --allowedTools");
        assert_eq!(args[position + 1], OsString::from("Bash,Read"));
    }

    #[test]
    fn no_declared_tool_grant_carries_no_allowed_tools_flag() {
        let args = build_args("do the thing", None, PermissionGrant::Unrestricted, None, None);
        let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();
        assert!(!borrowed.contains(&"--allowedTools"));
    }

    #[test]
    fn the_permission_flag_sits_before_an_optional_model_flag() {
        let args = build_args(
            "do the thing",
            Some("claude-opus-5"),
            PermissionGrant::Unrestricted,
            None,
            None,
        );
        let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();
        assert_eq!(
            borrowed,
            vec![
                "-p",
                "do the thing",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
                "--model",
                "claude-opus-5",
            ]
        );
    }
}
