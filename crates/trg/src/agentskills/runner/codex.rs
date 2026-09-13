use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use super::capabilities::HarnessControl;
use super::{
    capture_subprocess, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file,
    EvalRunOutcome, EvalRunRequest, Runner, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::outputs::{cleanup_runner_temp_files, outputs_dir, path_within_base, FINAL_MD};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::PermissionGrant;

const PROGRAM: &str = "codex";
const INSTALL_HINT: &str = "install Codex CLI and ensure `codex` is on PATH";

pub fn check_available() -> Result<(), EvalError> {
    check_runner_version(PROGRAM, INSTALL_HINT)
}

/// Translate a grant into the value codex's `-s` sandbox flag accepts.
///
/// `workspace-write` keeps codex's behavior exactly what it was before trg made the grant
/// explicit. `danger-full-access` is codex's widest sandbox; codex has no third option that
/// means "decide for yourself", which is the gap `PermissionGrant` closes.
fn permission_sandbox(grant: PermissionGrant) -> &'static str {
    match grant {
        PermissionGrant::WorkspaceWrite => "workspace-write",
        PermissionGrant::Unrestricted => "danger-full-access",
    }
}

/// The arguments are `OsString` because two of them are paths, and a path is not always
/// valid UTF-8. Rendering one into a `String` to build the list would hand the harness a
/// lossily rewritten directory to work in.
pub(crate) fn build_args(
    workspace_dir: &Path,
    final_text_path: &Path,
    model: Option<&str>,
    permission: PermissionGrant,
    prompt: &str,
) -> Vec<OsString> {
    let sandbox_flag = Runner::Codex
        .support(HarnessControl::SandboxLevels)
        .flag()
        .expect("the capability matrix declares codex takes its sandbox level as a flag");
    let mut args = vec![
        OsString::from("exec"),
        OsString::from("--json"),
        OsString::from("--skip-git-repo-check"),
        OsString::from(sandbox_flag),
        OsString::from(permission_sandbox(permission)),
        OsString::from("-C"),
        workspace_dir.as_os_str().to_os_string(),
        OsString::from("-o"),
        final_text_path.as_os_str().to_os_string(),
    ];
    if let Some(model) = model {
        args.push(OsString::from("-m"));
        args.push(OsString::from(model));
    }
    args.push(OsString::from(prompt));
    args
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request, Runner::Codex)?;
    let final_text_path = outputs_dir(request.workspace_dir).join(FINAL_MD);
    path_within_base(request.workspace_dir, &final_text_path).map_err(|e| RunnerError::InvalidOutput {
        program: PROGRAM.to_string(),
        detail: e.to_string(),
    })?;

    let args = build_args(
        request.workspace_dir,
        &final_text_path,
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
            prepared.environment.recorded_vars(),
        )?;
    }

    prepared.environment.apply(&mut command);

    let captured = capture_subprocess(&mut command, timeout_duration(request.timeout_secs))?;
    persist_runner_io(Runner::Codex, request, &captured)?;

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

    #[test]
    fn workspace_write_keeps_codexs_original_sandbox_value() {
        assert_eq!(permission_sandbox(PermissionGrant::WorkspaceWrite), "workspace-write");
    }

    #[test]
    fn unrestricted_asks_codex_for_danger_full_access() {
        assert_eq!(permission_sandbox(PermissionGrant::Unrestricted), "danger-full-access");
    }

    #[test]
    fn the_built_invocation_carries_the_translated_sandbox_flag() {
        let args = build_args(
            Path::new("/ws"),
            Path::new("/ws/outputs/final.md"),
            None,
            PermissionGrant::Unrestricted,
            "do it",
        );
        let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();
        assert_eq!(
            borrowed,
            vec![
                "exec",
                "--json",
                "--skip-git-repo-check",
                "-s",
                "danger-full-access",
                "-C",
                "/ws",
                "-o",
                "/ws/outputs/final.md",
                "do it",
            ]
        );
    }

    /// The value has to be the one the grant names rather than a constant. Reading the
    /// grant to pick a value that never varies would translate nothing, and the flag would
    /// go on meaning whatever it was hardcoded to.
    #[test]
    fn the_sandbox_flag_actually_follows_the_requested_grant() {
        let expected_flag = Runner::Codex
            .support(HarnessControl::SandboxLevels)
            .flag()
            .expect("codex declares a sandbox flag in the capability matrix");
        let sandbox_value_for = |grant: PermissionGrant| {
            let args = build_args(
                Path::new("/ws"),
                Path::new("/ws/outputs/final.md"),
                None,
                grant,
                "do it",
            );
            let position = args
                .windows(2)
                .position(|pair| pair[0] == expected_flag)
                .expect("the invocation carries the flag the matrix declares");
            args[position + 1].to_str().expect("test args are utf8").to_string()
        };

        assert_eq!(sandbox_value_for(PermissionGrant::WorkspaceWrite), "workspace-write");
        assert_eq!(sandbox_value_for(PermissionGrant::Unrestricted), "danger-full-access");
    }
}
