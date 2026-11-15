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
    let stdout = br#"{"type":"result","is_error":false,"duration_ms":1234,"result":"hello","usage":{"inputTokens":100}}
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
