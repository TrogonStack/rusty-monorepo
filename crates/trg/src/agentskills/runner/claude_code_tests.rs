use super::*;
use crate::agentskills::budget::RunCost;
use crate::agentskills::report::CacheTokens;

#[test]
fn parses_result_with_cost_and_cache_tokens() {
    let stdout = br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","total_cost_usd":0.0123,"usage":{"input_tokens":80,"output_tokens":20,"cache_read_input_tokens":5,"cache_creation_input_tokens":0}}
"#;
    let outcome = parse_outcome(stdout, 9999, true, Some(0), None);
    assert!(matches!(outcome.status, RunStatus::Completed));
    assert_eq!(outcome.duration_ms, 2000);
    assert_eq!(outcome.tokens.input_tokens(), Some(80));
    assert_eq!(outcome.tokens.output_tokens(), Some(20));
    // input + output only: cache reads and creations are billed separately and are
    // never folded into the total every runner agrees on.
    assert_eq!(outcome.tokens.total_tokens(), Some(100));
    assert_eq!(
        outcome.tokens.cached_tokens(),
        Some(CacheTokens::parse(Some(5), Some(0)).unwrap())
    );
    assert_eq!(outcome.cost, Some(RunCost::Priced { usd: 0.0123 }));
    assert_eq!(outcome.final_text, "final text");
}

#[test]
fn a_result_with_no_cache_fields_reports_no_cache_activity() {
    let stdout = br#"{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","usage":{"input_tokens":80,"output_tokens":20}}
"#;
    let outcome = parse_outcome(stdout, 9999, true, Some(0), None);
    assert_eq!(
        outcome.tokens.cached_tokens(),
        None,
        "a harness that never mentions cache tokens is not the same as one that measured zero"
    );
}

#[test]
fn non_zero_exit_is_runner_failure() {
    let stdout = br#"{"type":"result","is_error":false,"duration_ms":10,"result":"ok"}
"#;
    let outcome = parse_outcome(stdout, 0, false, Some(1), None);
    assert!(matches!(outcome.status, RunStatus::Failed));
    assert_eq!(outcome.failure_kind, Some(super::super::FAILURE_KIND_RUNNER));
}

#[test]
fn a_usage_block_the_harness_filled_in_leaves_nothing_unreadable() {
    let stdout = br#"{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","usage":{"input_tokens":80,"output_tokens":20}}
"#;
    let outcome = parse_outcome(stdout, 9999, true, Some(0), None);
    assert_eq!(outcome.tokens.input_tokens(), Some(80));
    assert_eq!(outcome.tokens.output_tokens(), Some(20));
    assert!(outcome.tokens.unreadable().is_empty());
}

#[test]
fn a_result_carrying_no_usage_block_at_all_reports_no_tokens_and_nothing_to_warn_about() {
    let stdout = br#"{"type":"result","is_error":false,"duration_ms":2000,"result":"final text"}
"#;
    let outcome = parse_outcome(stdout, 9999, true, Some(0), None);
    assert_eq!(outcome.tokens.total_tokens(), None);
    assert!(
        outcome.tokens.unreadable().is_empty(),
        "a harness that reported nothing contradicted nothing"
    );
}

#[test]
fn a_usage_block_that_leaves_a_field_out_reports_no_count_for_it_and_nothing_to_warn_about() {
    let stdout =
        br#"{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","usage":{"input_tokens":80}}
"#;
    let outcome = parse_outcome(stdout, 9999, true, Some(0), None);
    assert_eq!(outcome.tokens.input_tokens(), Some(80));
    assert_eq!(outcome.tokens.output_tokens(), None);
    assert!(outcome.tokens.unreadable().is_empty());
}

#[test]
fn a_usage_field_the_harness_garbled_is_reported_as_unreadable_rather_than_as_a_count_nobody_sent() {
    let stdout = br#"{"type":"result","is_error":false,"duration_ms":2000,"result":"final text","usage":{"input_tokens":"80","output_tokens":20}}
"#;
    let outcome = parse_outcome(stdout, 9999, true, Some(0), None);
    assert_eq!(outcome.tokens.input_tokens(), None);
    assert_eq!(outcome.tokens.output_tokens(), Some(20));
    let warnings: Vec<String> = outcome
        .tokens
        .unreadable()
        .iter()
        .map(|field| field.warning())
        .collect();
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("claude-code") && warnings[0].contains("input_tokens"),
        "unexpected warning: {}",
        warnings[0]
    );
}

#[test]
fn the_result_total_is_the_runs_usage_even_when_per_message_usage_sums_to_something_else() {
    let stdout =
        br#"{"type":"assistant","message":{"id":"msg_1","content":[],"usage":{"input_tokens":10,"output_tokens":6}}}
{"type":"assistant","message":{"id":"msg_2","content":[],"usage":{"input_tokens":20,"output_tokens":4}}}
{"type":"result","is_error":false,"duration_ms":10,"usage":{"input_tokens":500,"output_tokens":90}}
"#;
    let outcome = parse_outcome(stdout, 0, true, Some(0), None);
    assert_eq!(outcome.tokens.input_tokens(), Some(500));
    assert_eq!(outcome.tokens.output_tokens(), Some(90));
}

#[test]
fn missing_result_event_is_runner_failure() {
    let stdout = br#"{"type":"system"}
"#;
    let outcome = parse_outcome(stdout, 0, true, Some(0), None);
    assert!(matches!(outcome.status, RunStatus::Failed));
    assert_eq!(outcome.failure_kind, Some(super::super::FAILURE_KIND_RUNNER));
}

/// A tool name carrying a leaked model-internal channel marker is just a string to
/// this parser: it never inspects a tool's name, so nothing here needs to recognize
/// the marker. The run still fails on its own `is_error`, with the harness's own
/// explanation of why carried through as `final_text` rather than dropped.
#[test]
fn a_tool_name_carrying_a_leaked_channel_marker_still_fails_with_a_useful_reason() {
    let stdout = br#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"<|channel|>commentary<|message|>Bash","input":{"command":"false"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"exit 1","is_error":true}]}}
{"type":"result","is_error":true,"duration_ms":500,"result":"the model leaked an internal channel marker into a tool call"}
"#;
    let outcome = parse_outcome(stdout, 0, true, Some(0), None);
    assert!(matches!(outcome.status, RunStatus::Failed));
    assert_eq!(outcome.failure_kind, Some(super::super::FAILURE_KIND_RUNNER));
    assert_eq!(
        outcome.final_text,
        "the model leaked an internal channel marker into a tool call"
    );
}

/// A tool call's JSON arguments cut off mid-object, the way a harness that died
/// mid-write would leave them, parse as nothing: the line fails
/// `serde_json::from_str` and is skipped like any other unparseable line, rather than
/// panicking or being read as a tool call with no arguments. With no terminal
/// `result` event following it either, since the harness that cut this line off
/// never got to write one, the run still falls back to a runner failure.
#[test]
fn a_tool_call_truncated_mid_object_is_skipped_rather_than_panicking() {
    let stdout = br#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls -la /very/long/path/that/got/cut/of"#;
    let outcome = parse_outcome(stdout, 0, false, None, None);
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
        let args = build_args("do the thing", None, grant, None, None, None, None);
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
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        None,
        None,
        None,
    );
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
fn a_case_appendix_reaches_the_harness_as_the_flag_the_matrix_names() {
    let appendix = SystemPromptAppendix::parse("Answer in British English.").unwrap();
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        None,
        Some(&appendix),
        None,
    );
    let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();

    let position = borrowed
        .iter()
        .position(|arg| *arg == "--append-system-prompt")
        .expect("the invocation carries the flag the capability matrix declares");
    assert_eq!(borrowed[position + 1], "Answer in British English.");
}

/// The flag is what tells claude-code it was given an appendix, so putting it there
/// with nothing behind it would record a case as having steered its agent by a run that
/// added nothing.
#[test]
fn a_case_that_declares_no_appendix_leaves_the_flag_off_entirely() {
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        None,
        None,
        None,
    );
    let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();

    assert!(!borrowed.contains(&"--append-system-prompt"), "{borrowed:?}");
}

#[test]
fn a_declared_mcp_config_always_carries_the_guard_flag_alongside_it() {
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        Some(Path::new("/workspace/mcp-config.json")),
        None,
        None,
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
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        None,
        None,
        None,
    );
    let borrowed: Vec<&str> = args.iter().map(|a| a.to_str().expect("test args are utf8")).collect();
    assert!(!borrowed.contains(&"--mcp-config"));
    assert!(!borrowed.contains(&"--strict-mcp-config"));
}

#[test]
fn a_declared_tool_grant_is_carried_as_allowed_tools() {
    let grant = ToolGrant::parse(["Bash", "Read"]).unwrap();
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        Some(&grant),
        None,
        None,
        None,
    );
    let position = args
        .windows(2)
        .position(|pair| pair[0] == "--allowedTools")
        .expect("the invocation carries --allowedTools");
    assert_eq!(args[position + 1], OsString::from("Bash,Read"));
}

#[test]
fn no_declared_tool_grant_carries_no_allowed_tools_flag() {
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        None,
        None,
        None,
    );
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

#[test]
fn a_turn_cap_reaches_claude_as_max_turns() {
    let args = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        None,
        None,
        Some("7".parse().unwrap()),
    );
    let position = args
        .windows(2)
        .position(|pair| pair[0] == "--max-turns")
        .expect("the invocation carries --max-turns");
    assert_eq!(args[position + 1], OsString::from("7"));
    let uncapped = build_args(
        "do the thing",
        None,
        PermissionGrant::Unrestricted,
        None,
        None,
        None,
        None,
    );
    assert!(!uncapped.contains(&OsString::from("--max-turns")));
}

#[test]
fn a_run_claude_stopped_at_its_turn_cap_is_budget_exhausted_and_keeps_what_it_spent() {
    let stdout = br#"{"type":"result","subtype":"error_max_turns","is_error":true,"num_turns":3,"duration_ms":1500,"total_cost_usd":0.02,"usage":{"input_tokens":50,"output_tokens":5}}
"#;
    let outcome = parse_outcome(stdout, 9999, false, Some(1), Some("2".parse().unwrap()));

    assert_eq!(outcome.status, RunStatus::BudgetExhausted);
    assert_eq!(
        outcome.failure_kind,
        Some(crate::agentskills::budget::FAILURE_KIND_TURN_CAP)
    );
    assert_eq!(outcome.tokens.input_tokens(), Some(50));
    assert_eq!(outcome.cost, Some(RunCost::Priced { usd: 0.02 }));
    assert!(!outcome.is_transient_failure());
}
