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
