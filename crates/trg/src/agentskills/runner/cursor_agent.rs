use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use super::capabilities::HarnessControl;
use super::usage::{HarnessTokenUsage, UsageFieldNames};
use super::{
    capture_subprocess, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file,
    EvalRunOutcome, EvalRunRequest, RunStatus, Runner, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::outputs::{cleanup_runner_temp_files, persist_final_markdown};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::PermissionGrant;

const PROGRAM: &str = "cursor-agent";
const INSTALL_HINT: &str = "install Cursor Agent CLI and ensure `cursor-agent` is on PATH";

/// How cursor-agent spells the counts in its terminal `result` event usage block.
const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "inputTokens",
    output: "outputTokens",
    cache_read: "cacheReadTokens",
    cache_write: "cacheWriteTokens",
};

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

    if let Some(run_dir) = request.transcript_path.parent() {
        let recorded: Vec<String> = args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let borrowed: Vec<&str> = recorded.iter().map(String::as_str).collect();
        write_runner_invocation_metadata(
            run_dir,
            redact_command_args(PROGRAM, &borrowed),
            prepared.environment.record(),
        )?;
    }

    prepared.environment.apply(&mut command);

    let captured = capture_subprocess(&mut command, timeout_duration(request.timeout_secs))?;
    persist_runner_io(Runner::CursorAgent, request, &captured)?;

    if captured.timed_out {
        let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
        let outcome = timeout_outcome(Runner::CursorAgent, timeout_ms, captured.exit_code);
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
mod tests {
    use super::*;
    use crate::agentskills::report::CacheTokens;

    #[test]
    fn parses_terminal_result_event() {
        let stdout = br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"duration_ms":1234,"duration_api_ms":1100,"result":"hello","usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":0}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 1234);
        assert_eq!(outcome.tokens.input_tokens(), Some(100));
        assert_eq!(outcome.tokens.output_tokens(), Some(50));
        assert_eq!(outcome.tokens.total_tokens(), Some(150));
        // A reported zero is still a report: distinct from the harness saying nothing.
        assert_eq!(
            outcome.tokens.cached_tokens(),
            Some(CacheTokens::parse(Some(0), None).unwrap())
        );
        assert_eq!(outcome.final_text, "hello");
    }

    #[test]
    fn a_nonzero_cache_read_is_surfaced_but_never_folded_into_the_total() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":15}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(outcome.tokens.total_tokens(), Some(150));
        assert_eq!(
            outcome.tokens.cached_tokens(),
            Some(CacheTokens::parse(Some(15), None).unwrap())
        );
    }

    #[test]
    fn a_cache_write_is_recorded_on_its_own_side_and_never_folded_into_the_total() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":15,"cacheWriteTokens":40}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(outcome.tokens.total_tokens(), Some(150));
        assert_eq!(
            outcome.tokens.cached_tokens(),
            Some(CacheTokens::parse(Some(15), Some(40)).unwrap()),
            "cache writes are billed at a premium, so dropping them understates what the run cost"
        );
    }

    #[test]
    fn a_turn_that_only_wrote_to_the_cache_still_reports_cache_activity() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100,"outputTokens":50,"cacheWriteTokens":40}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(
            outcome.tokens.cached_tokens(),
            Some(CacheTokens::parse(None, Some(40)).unwrap())
        );
    }

    #[test]
    fn a_result_with_no_cache_field_reports_no_cache_activity() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100,"outputTokens":50}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(
            outcome.tokens.cached_tokens(),
            None,
            "a harness that never mentions cache tokens is not the same as one that measured zero"
        );
    }

    #[test]
    fn a_usage_block_the_harness_filled_in_leaves_nothing_unreadable() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100,"outputTokens":50}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(outcome.tokens.input_tokens(), Some(100));
        assert_eq!(outcome.tokens.output_tokens(), Some(50));
        assert!(outcome.tokens.unreadable().is_empty());
    }

    #[test]
    fn a_result_carrying_no_usage_block_at_all_reports_no_tokens_and_nothing_to_warn_about() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello"}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(outcome.tokens.total_tokens(), None);
        assert!(
            outcome.tokens.unreadable().is_empty(),
            "a harness that reported nothing contradicted nothing"
        );
    }

    #[test]
    fn a_usage_block_that_leaves_a_field_out_reports_no_count_for_it_and_nothing_to_warn_about() {
        let stdout =
            br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(outcome.tokens.input_tokens(), Some(100));
        assert_eq!(outcome.tokens.output_tokens(), None);
        assert!(outcome.tokens.unreadable().is_empty());
    }

    #[test]
    fn a_usage_field_the_harness_garbled_is_reported_as_unreadable_rather_than_as_a_count_nobody_sent() {
        let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":-1}}
"#;
        let outcome = parse_outcome(stdout, 9999, true, Some(0));
        assert_eq!(outcome.tokens.total_tokens(), Some(150));
        assert_eq!(
            outcome.tokens.cached_tokens(),
            None,
            "a count nobody can read is not a cache read of zero"
        );
        let warnings: Vec<String> = outcome
            .tokens
            .unreadable()
            .iter()
            .map(|field| field.warning())
            .collect();
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("cursor-agent") && warnings[0].contains("cacheReadTokens"),
            "unexpected warning: {}",
            warnings[0]
        );
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

    #[test]
    fn both_grants_collapse_to_the_same_force_flag() {
        assert_eq!(permission_args(PermissionGrant::WorkspaceWrite), ["--force"]);
        assert_eq!(permission_args(PermissionGrant::Unrestricted), ["--force"]);
    }

    #[test]
    fn the_built_invocation_carries_the_matrixs_sandbox_flag() {
        let expected_flag = Runner::CursorAgent
            .support(HarnessControl::SandboxLevels)
            .flag()
            .expect("cursor-agent declares a sandbox flag in the capability matrix");
        let args = build_args(Path::new("/ws"), None, PermissionGrant::WorkspaceWrite, "do it");
        let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();
        assert_eq!(
            borrowed,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                expected_flag,
                "--workspace",
                "/ws",
                "do it"
            ]
        );
    }
}
