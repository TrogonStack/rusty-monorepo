mod constants;
use constants::{INSTALL_HINT, PROGRAM, USAGE_FIELDS};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Command;

use super::capabilities::HarnessControl;
use super::environment::{ConfigHomeOrigin, RunEnvironment};
use super::telemetry::{in_step, AgentInvocation, HarnessTraces};
use super::usage::{HarnessTokenUsage, UsageFieldNames};
use super::{
    budget_exhausted_outcome, check_runner_version, completed_outcome, persist_runner_io, prepare_workspace,
    runner_failure_outcome, timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file,
    EvalRunOutcome, EvalRunRequest, Runner, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::mocks::MaterializedMcpConfig;
use crate::agentskills::outputs::{cleanup_runner_temp_files, outputs_dir, path_within_base, FINAL_MD};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::PermissionGrant;

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

/// codex takes no MCP server table on its command line, so a run's mocks reach it as the
/// `config.toml` of the config home the run was given.
///
/// Nothing about that file is exclusive: codex merges it with whatever else the config home
/// holds, so a home the operator owns would keep serving the operator's own MCP servers
/// beside the mocks and the run would not be the run its report describes. The gate that
/// keeps mocked codex runs to `--environment isolated` is what makes the home a run-owned
/// one; reaching here without that is a bug, and a bug that writes into someone's home is
/// not one to recover from.
fn declare_mock_servers(mcp_config: &MaterializedMcpConfig, environment: &RunEnvironment) -> Result<(), RunnerError> {
    let refuse = |detail: String| RunnerError::InvalidOutput {
        program: PROGRAM.to_string(),
        detail,
    };

    let file_name = Runner::Codex
        .support(HarnessControl::McpServers)
        .config_file_name()
        .ok_or_else(|| refuse("the capability matrix no longer declares a codex mcp config file".to_string()))?;

    let config_home = environment
        .config_home()
        .ok_or_else(|| refuse("this run has no codex config home to declare its mock servers in".to_string()))?;

    let destination = config_home.path().join(file_name);
    if config_home.origin() != ConfigHomeOrigin::Run {
        return Err(refuse(format!(
            "declaring mock servers would write `{}`, which belongs to the operator rather than to this run",
            destination.display()
        )));
    }

    // The run's config home is populated by linking entries out of the operator's own, so
    // this path can already be a symlink pointing back there; writing through it would edit
    // the operator's file rather than the run's.
    if destination.symlink_metadata().is_ok() {
        std::fs::remove_file(&destination)?;
    }
    std::fs::copy(mcp_config.toml(), &destination)?;
    Ok(())
}

/// codex reads its exporters from the `[otel]` table of its `config.toml`, not from
/// `OTEL_*`, so a run forwarding telemetry hands it that table in the config home the
/// run owns. A config home the operator owns is never written into; the run then exports
/// nothing of its own and its spans are rebuilt from stdout instead.
///
/// The table can carry exporter headers, credentials included, so the file is kept
/// readable by its owner alone until [`scrub_otel_credentials`] removes them.
fn declare_otel(environment: &RunEnvironment) -> Result<HarnessTraces, RunnerError> {
    let Some(otel) = environment.codex_otel() else {
        return Ok(HarnessTraces::Silent);
    };
    let Some(config_home) = environment
        .config_home()
        .filter(|home| home.origin() == ConfigHomeOrigin::Run)
    else {
        tracing::warn!("telemetry forwarding to codex needs a config home the run owns; rebuilding its spans instead");
        return Ok(HarnessTraces::Silent);
    };
    let file_name = Runner::Codex
        .support(HarnessControl::McpServers)
        .config_file_name()
        .unwrap_or("config.toml");
    let destination = config_home.path().join(file_name);
    if destination.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) {
        std::fs::remove_file(&destination)?;
    }
    let existing = std::fs::read_to_string(&destination).unwrap_or_default();
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&destination)?;
    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o600))?;
    file.write_all(format!("{separator}{}", otel.as_str()).as_bytes())?;
    Ok(HarnessTraces::exported(otel.exports_traces()))
}

/// The run's config home sits inside the report, so exporter credentials must not outlive
/// the harness, whatever way it ended.
fn scrub_otel_credentials(environment: &RunEnvironment) -> Result<(), RunnerError> {
    let Some(config_home) = environment
        .config_home()
        .filter(|home| home.origin() == ConfigHomeOrigin::Run)
    else {
        return Ok(());
    };
    let file_name = Runner::Codex
        .support(HarnessControl::McpServers)
        .config_file_name()
        .unwrap_or("config.toml");
    let destination = config_home.path().join(file_name);
    let Ok(contents) = std::fs::read_to_string(&destination) else {
        return Ok(());
    };
    let Ok(mut document) = toml::from_str::<toml::Table>(&contents).map(toml::Value::Table) else {
        return Ok(());
    };
    if !strip_otel_headers(&mut document) {
        return Ok(());
    }
    let rendered = toml::to_string(&document).map_err(|source| RunnerError::InvalidOutputSource {
        program: PROGRAM.to_string(),
        detail: format!(
            "could not re-render `{}` without its otel headers: {source}",
            destination.display()
        ),
        source: Box::new(source),
    })?;
    std::fs::write(&destination, rendered)?;
    Ok(())
}

fn strip_otel_headers(document: &mut toml::Value) -> bool {
    let Some(otel) = document.get_mut("otel").and_then(toml::Value::as_table_mut) else {
        return false;
    };
    let mut scrubbed = false;
    for key in ["exporter", "trace_exporter", "metrics_exporter"] {
        let Some(exporter) = otel.get_mut(key).and_then(toml::Value::as_table_mut) else {
            continue;
        };
        for (_, kind) in exporter.iter_mut() {
            if let Some(kind_table) = kind.as_table_mut() {
                scrubbed |= kind_table.remove("headers").is_some();
            }
        }
    }
    scrubbed
}

pub fn run(request: &EvalRunRequest) -> Result<EvalRunOutcome, RunnerError> {
    let prepared = prepare_workspace(request, Runner::Codex)?;
    if let Some(mcp_config) = &request.mcp_config {
        in_step("declare mock servers", || {
            declare_mock_servers(mcp_config, &prepared.environment)
        })?;
    }
    let traces = in_step("declare otel", || declare_otel(&prepared.environment))?;
    let final_text_path = outputs_dir(request.workspace_dir).join(FINAL_MD);
    path_within_base(request.workspace_dir, &final_text_path).map_err(|source| RunnerError::InvalidOutputSource {
        program: PROGRAM.to_string(),
        detail: source.to_string(),
        source: Box::new(source),
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

    let invocation = AgentInvocation::begin(Runner::Codex, request.runner_model);
    let captured = invocation.capture(&mut command, timeout_duration(request.timeout_secs), request.caps)?;
    let outcome = in_step("parse outcome", || {
        if let Some(cap) = captured.cap_reached {
            return Ok(budget_exhausted_outcome(
                Runner::Codex,
                cap,
                captured.duration_ms,
                captured.exit_code,
            ));
        }
        if captured.timed_out {
            let timeout_ms = request.timeout_secs.unwrap_or(0).saturating_mul(1000);
            return Ok(timeout_outcome(Runner::Codex, timeout_ms, captured.exit_code));
        }
        let final_text = std::fs::read_to_string(&final_text_path).unwrap_or_default();
        let exit_ok = captured.exit_code == Some(0);
        Ok(parse_outcome(
            &captured.stdout,
            captured.duration_ms,
            exit_ok,
            captured.exit_code,
            final_text,
        ))
    })?;
    invocation.finish(&captured, &outcome, request.runner_model, traces);
    in_step("scrub otel credentials", || {
        scrub_otel_credentials(&prepared.environment)
    })?;
    persist_runner_io(Runner::Codex, request, &captured)?;

    in_step("write outcome", || {
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
            return runner_failure_outcome(Runner::Codex, wall_ms, exit_code, final_text);
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
        return runner_failure_outcome(Runner::Codex, wall_ms, exit_code, final_text);
    };

    if !exit_ok {
        return runner_failure_outcome(Runner::Codex, wall_ms, exit_code, final_text);
    }

    let tokens = HarnessTokenUsage::read(terminal.get("usage"), Runner::Codex, USAGE_FIELDS);

    completed_outcome(
        wall_ms,
        exit_code,
        tokens,
        Runner::Codex.pricing().price(None),
        final_text,
    )
}

#[cfg(test)]
#[path = "codex_tests.rs"]
mod tests;
