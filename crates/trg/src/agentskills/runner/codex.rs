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
    check_runner_version, completed_outcome, persist_runner_io, prepare_workspace, runner_failure_outcome,
    timeout_duration, timeout_outcome, write_runner_invocation_metadata, write_timing_file, EvalRunOutcome,
    EvalRunRequest, Runner, RunnerError,
};
use crate::agentskills::evals::EvalError;
use crate::agentskills::mocks::MaterializedMcpConfig;
use crate::agentskills::outputs::{cleanup_runner_temp_files, outputs_dir, path_within_base, FINAL_MD};
use crate::agentskills::redact::redact_command_args;
use crate::agentskills::report::PermissionGrant;

const PROGRAM: &str = "codex";
const INSTALL_HINT: &str = "install Codex CLI and ensure `codex` is on PATH";

/// How codex spells the counts in its `turn.completed` usage block.
const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "input_tokens",
    output: "output_tokens",
    cache_read: "cached_input_tokens",
    cache_write: "cache_write_input_tokens",
};

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
    let rendered = toml::to_string(&document).map_err(|source| RunnerError::InvalidOutput {
        program: PROGRAM.to_string(),
        detail: format!(
            "could not re-render `{}` without its otel headers: {source}",
            destination.display()
        ),
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
    let captured = invocation.capture(&mut command, timeout_duration(request.timeout_secs))?;
    let outcome = in_step("parse outcome", || {
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
mod tests {
    use std::path::PathBuf;

    use super::super::RunStatus;
    use super::*;
    use crate::agentskills::report::{CacheTokens, EnvironmentPolicy};

    #[test]
    fn parses_turn_completed_event() {
        let stdout = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40,"cached_input_tokens":10}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert!(matches!(outcome.status, RunStatus::Completed));
        assert_eq!(outcome.duration_ms, 5000);
        assert_eq!(outcome.tokens.input_tokens(), Some(120));
        assert_eq!(outcome.tokens.output_tokens(), Some(40));
        // input + output only: the same rule claude-code and cursor-agent use, so a cached
        // count is never folded into a total that is supposed to compare across harnesses.
        assert_eq!(outcome.tokens.total_tokens(), Some(160));
        assert_eq!(
            outcome.tokens.cached_tokens(),
            Some(CacheTokens::parse(Some(10), None).unwrap())
        );
        assert_eq!(outcome.final_text, "final");
        assert_eq!(outcome.exit_code, Some(0));
    }

    #[test]
    fn a_cache_write_is_recorded_on_its_own_side_and_never_folded_into_the_total() {
        let stdout = br#"{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40,"cached_input_tokens":10,"cache_write_input_tokens":55}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert_eq!(outcome.tokens.total_tokens(), Some(160));
        assert_eq!(
            outcome.tokens.cached_tokens(),
            Some(CacheTokens::parse(Some(10), Some(55)).unwrap()),
            "cache writes are billed at a premium, so dropping them understates what the run cost"
        );
    }

    #[test]
    fn a_turn_that_only_wrote_to_the_cache_still_reports_cache_activity() {
        let stdout =
            br#"{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40,"cache_write_input_tokens":55}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert_eq!(
            outcome.tokens.cached_tokens(),
            Some(CacheTokens::parse(None, Some(55)).unwrap())
        );
    }

    #[test]
    fn a_turn_with_no_cached_input_tokens_reports_no_cache_activity() {
        let stdout = br#"{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert_eq!(
            outcome.tokens.cached_tokens(),
            None,
            "a harness that never mentions cache tokens is not the same as one that measured zero"
        );
    }

    #[test]
    fn a_usage_block_the_harness_filled_in_leaves_nothing_unreadable() {
        let stdout = br#"{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert_eq!(outcome.tokens.input_tokens(), Some(120));
        assert_eq!(outcome.tokens.output_tokens(), Some(40));
        assert!(outcome.tokens.unreadable().is_empty());
    }

    #[test]
    fn a_turn_carrying_no_usage_block_at_all_reports_no_tokens_and_nothing_to_warn_about() {
        let stdout = br#"{"type":"turn.completed"}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert_eq!(outcome.tokens.total_tokens(), None);
        assert!(
            outcome.tokens.unreadable().is_empty(),
            "a harness that reported nothing contradicted nothing"
        );
    }

    #[test]
    fn a_usage_block_that_leaves_a_field_out_reports_no_count_for_it_and_nothing_to_warn_about() {
        let stdout = br#"{"type":"turn.completed","usage":{"input_tokens":120}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert_eq!(outcome.tokens.input_tokens(), Some(120));
        assert_eq!(outcome.tokens.output_tokens(), None);
        assert!(outcome.tokens.unreadable().is_empty());
    }

    #[test]
    fn a_usage_field_the_harness_garbled_is_reported_as_unreadable_rather_than_as_a_count_nobody_sent() {
        let stdout = br#"{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":null}}
"#;
        let outcome = parse_outcome(stdout, 5000, true, Some(0), "final".to_string());
        assert_eq!(outcome.tokens.input_tokens(), Some(120));
        assert_eq!(outcome.tokens.output_tokens(), None);
        let warnings: Vec<String> = outcome
            .tokens
            .unreadable()
            .iter()
            .map(|field| field.warning())
            .collect();
        assert_eq!(
            warnings.len(),
            1,
            "a harness that names a field and fills it with nothing knew the field existed"
        );
        assert!(
            warnings[0].contains("codex") && warnings[0].contains("output_tokens"),
            "unexpected warning: {}",
            warnings[0]
        );
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

    /// Helpers for the mock-server declaration tests: a config home the run owns, and one
    /// that belongs to the operator, built the same way a real run builds them.
    fn environment_for(policy: EnvironmentPolicy, root: &Path) -> RunEnvironment {
        let host = std::collections::BTreeMap::from([(
            "HOME".to_string(),
            root.join("host-home").to_string_lossy().into_owned(),
        )]);
        std::fs::create_dir_all(root.join("host-home/.codex")).unwrap();
        RunEnvironment::prepare_from(Runner::Codex, &root.join("run"), policy, None, &host).unwrap()
    }

    fn materialized_config(root: &Path) -> MaterializedMcpConfig {
        let json = root.join("mcp-config.json");
        let toml = root.join("mcp-config.toml");
        std::fs::write(&json, "{}").unwrap();
        std::fs::write(&toml, "[mcp_servers.github]\ncommand = \"/bin/trg\"\n").unwrap();
        MaterializedMcpConfig::parse(json, toml).unwrap()
    }

    #[test]
    fn mock_servers_are_declared_in_the_config_file_the_capability_matrix_names() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment_for(EnvironmentPolicy::Isolated, temp.path());
        let config = materialized_config(temp.path());

        declare_mock_servers(&config, &environment).unwrap();

        let written = environment.config_home().unwrap().path().join("config.toml");
        assert_eq!(
            std::fs::read_to_string(&written).unwrap(),
            std::fs::read_to_string(config.toml()).unwrap()
        );
    }

    /// The config home a run does not own is the operator's own, where their MCP servers,
    /// model choice and every other setting live. Writing mocks there would edit a file
    /// nobody asked trg to touch, and would still not exclude the servers already in it.
    #[test]
    fn declaring_mock_servers_refuses_a_config_home_that_belongs_to_the_operator() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment_for(EnvironmentPolicy::Scrubbed, temp.path());
        let config = materialized_config(temp.path());
        let operator_config = environment.config_home().unwrap().path().join("config.toml");
        std::fs::write(&operator_config, "model = \"the operator's own\"").unwrap();

        let error = declare_mock_servers(&config, &environment)
            .expect_err("a config home the run does not own must not be written into");

        assert!(
            matches!(error, RunnerError::InvalidOutput { .. }),
            "unexpected: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&operator_config).unwrap(),
            "model = \"the operator's own\"",
            "the operator's config is left exactly as they wrote it"
        );
    }

    /// A run's config home is populated by linking entries out of the operator's own, so a
    /// `config.toml` found there can be a symlink pointing back at the operator's file.
    /// Writing through it would edit the operator's machine from inside an isolated run.
    #[test]
    fn declaring_mock_servers_replaces_a_linked_config_rather_than_writing_through_it() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment_for(EnvironmentPolicy::Isolated, temp.path());
        let config = materialized_config(temp.path());

        let operator_config = temp.path().join("host-home/.codex/config.toml");
        std::fs::write(&operator_config, "model = \"the operator's own\"").unwrap();
        let linked = environment.config_home().unwrap().path().join("config.toml");
        std::os::unix::fs::symlink(&operator_config, &linked).unwrap();

        declare_mock_servers(&config, &environment).unwrap();

        assert!(!linked.symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&operator_config).unwrap(),
            "model = \"the operator's own\"",
            "the operator's config is left exactly as they wrote it"
        );
    }

    fn forwarding_environment(policy: EnvironmentPolicy, root: &Path) -> RunEnvironment {
        let mut environment = environment_for(policy, root);
        let host = std::collections::BTreeMap::from([(
            "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
            "http://collector:4318".to_string(),
        )]);
        environment.forward_telemetry(
            Runner::Codex,
            crate::agentskills::runner::TelemetryForwarding::On,
            &host,
            crate::telemetry::ContentCapture::NoContent,
        );
        environment
    }

    #[test]
    fn a_forwarding_run_appends_its_otel_table_after_the_mock_servers() {
        let temp = tempfile::tempdir().unwrap();
        let environment = forwarding_environment(EnvironmentPolicy::Isolated, temp.path());
        let config = materialized_config(temp.path());

        declare_mock_servers(&config, &environment).unwrap();
        assert_eq!(declare_otel(&environment).unwrap(), HarnessTraces::Exported);

        let written = environment.config_home().unwrap().path().join("config.toml");
        let parsed: toml::Value = toml::from_str(&std::fs::read_to_string(&written).unwrap()).unwrap();
        assert!(parsed.get("mcp_servers").is_some());
        assert_eq!(
            parsed["otel"]["trace_exporter"]["otlp-http"]["endpoint"].as_str(),
            Some("http://collector:4318/v1/traces")
        );
        let mode = std::fs::metadata(&written).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn a_run_without_forwarding_writes_no_otel_table() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment_for(EnvironmentPolicy::Isolated, temp.path());

        assert_eq!(declare_otel(&environment).unwrap(), HarnessTraces::Silent);
        assert!(!environment.config_home().unwrap().path().join("config.toml").exists());
    }

    #[test]
    fn forwarding_never_writes_into_the_operators_config_home() {
        let temp = tempfile::tempdir().unwrap();
        let environment = forwarding_environment(EnvironmentPolicy::Scrubbed, temp.path());
        let operator_config = environment.config_home().unwrap().path().join("config.toml");
        std::fs::write(&operator_config, "model = \"the operator's own\"").unwrap();

        assert_eq!(declare_otel(&environment).unwrap(), HarnessTraces::Silent);
        assert_eq!(
            std::fs::read_to_string(&operator_config).unwrap(),
            "model = \"the operator's own\""
        );
    }

    /// The config home a forwarding run writes its `[otel]` table into sits inside the run
    /// directory a report keeps, so once codex has read the credentials the table carried,
    /// nothing may still hold them: a report shipped whole, zipped, or copied to a teammate
    /// would otherwise ship the operator's exporter credentials along with it.
    #[test]
    fn scrubbing_drops_the_header_values_but_keeps_the_rest_of_the_otel_table() {
        let temp = tempfile::tempdir().unwrap();
        let environment = forwarding_environment(EnvironmentPolicy::Isolated, temp.path());
        let config = materialized_config(temp.path());

        declare_mock_servers(&config, &environment).unwrap();
        declare_otel(&environment).unwrap();
        scrub_otel_credentials(&environment).unwrap();

        let written = environment.config_home().unwrap().path().join("config.toml");
        let contents = std::fs::read_to_string(&written).unwrap();
        assert!(
            !contents.contains("Bearer abc") && !contents.contains("evals"),
            "the otel headers are still present: {contents}"
        );

        let parsed: toml::Value = toml::from_str(&contents).unwrap();
        assert!(
            parsed.get("mcp_servers").is_some(),
            "mock server declarations survive scrubbing"
        );
        assert_eq!(
            parsed["otel"]["trace_exporter"]["otlp-http"]["endpoint"].as_str(),
            Some("http://collector:4318/v1/traces"),
            "the exporter endpoint stays, only its credentials are gone"
        );
        assert!(parsed["otel"]["trace_exporter"]["otlp-http"].get("headers").is_none());
    }

    /// The property the fix exists for: nothing under the run directory a report keeps
    /// contains the literal header value a forwarding run was handed, once the run is over.
    #[test]
    fn no_file_in_the_run_directory_carries_the_otel_header_value_after_scrubbing() {
        let temp = tempfile::tempdir().unwrap();
        let environment = forwarding_environment(EnvironmentPolicy::Isolated, temp.path());
        let config = materialized_config(temp.path());

        declare_mock_servers(&config, &environment).unwrap();
        declare_otel(&environment).unwrap();
        scrub_otel_credentials(&environment).unwrap();

        let run_dir = temp.path().join("run");
        for entry in walkdir(&run_dir) {
            let contents = std::fs::read(&entry).unwrap();
            let text = String::from_utf8_lossy(&contents);
            assert!(
                !text.contains("Bearer abc"),
                "`{}` still carries the forwarded otel header value",
                entry.display()
            );
        }
    }

    #[test]
    fn scrubbing_is_a_no_op_when_no_otel_table_was_ever_written() {
        let temp = tempfile::tempdir().unwrap();
        let environment = environment_for(EnvironmentPolicy::Isolated, temp.path());

        scrub_otel_credentials(&environment).unwrap();

        assert!(!environment.config_home().unwrap().path().join("config.toml").exists());
    }

    #[test]
    fn scrubbing_never_touches_the_operators_config_home() {
        let temp = tempfile::tempdir().unwrap();
        let environment = forwarding_environment(EnvironmentPolicy::Scrubbed, temp.path());
        let operator_config = environment.config_home().unwrap().path().join("config.toml");
        std::fs::write(&operator_config, "model = \"the operator's own\"").unwrap();

        scrub_otel_credentials(&environment).unwrap();

        assert_eq!(
            std::fs::read_to_string(&operator_config).unwrap(),
            "model = \"the operator's own\""
        );
    }

    fn walkdir(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir(&path));
            } else {
                files.push(path);
            }
        }
        files
    }
}
