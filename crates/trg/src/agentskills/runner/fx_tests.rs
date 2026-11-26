use super::*;
use crate::agentskills::report::CacheTokens;
use std::ffi::OsString;

/// The last line of a captured `trogonai-agent ask --output-format stream-json` run,
/// against a provider that reports cached prompt tokens.
const RESULT: &[u8] = br#"{"protocol_version":1,"seq":1,"session_id":"s","turn_id":1,"event":{"type":"text_chunk","origin":"assistant","text":"Reading the file."}}
{"type":"result","output":"Reading the file.\n\nThe file says hello.","final_output":"The file says hello.","exit_code":0,"model":"stub-model","resolved_provider":null,"session_id":"","steps":1,"tool_calls":[{"name":"read_file","status":"error"}],"usage":{"input_tokens":200,"output_tokens":40,"cache_read_tokens":80}}
"#;

#[test]
fn a_finished_run_reports_its_last_message_and_its_tokens() {
    let outcome = parse_outcome(RESULT, 4321, Some(0));
    assert!(matches!(outcome.status, RunStatus::Completed));
    assert_eq!(outcome.final_text, "The file says hello.");
    assert_eq!(
        outcome.duration_ms, 4321,
        "fx reports no duration, so trg's wall time stands"
    );
    assert_eq!(outcome.tokens.input_tokens(), Some(200));
    assert_eq!(outcome.tokens.output_tokens(), Some(40));
    assert_eq!(outcome.tokens.total_tokens(), Some(240));
    assert_eq!(
        outcome.tokens.cached_tokens(),
        Some(CacheTokens::parse(Some(80), None).unwrap())
    );
    assert_eq!(outcome.cost.and_then(|cost| cost.usd()), None, "fx publishes no price");
}

#[test]
fn a_result_without_a_final_output_falls_back_to_the_whole_output() {
    let stdout = br#"{"type":"result","output":"all of it","exit_code":0,"usage":{"input_tokens":1,"output_tokens":1}}
"#;
    assert_eq!(parse_outcome(stdout, 1, Some(0)).final_text, "all of it");
}

#[test]
fn a_count_the_provider_never_reported_is_absent_rather_than_unreadable() {
    let stdout =
        br#"{"type":"result","final_output":"ok","exit_code":0,"usage":{"input_tokens":12,"output_tokens":null}}
"#;
    let outcome = parse_outcome(stdout, 1, Some(0));
    assert_eq!(outcome.tokens.input_tokens(), Some(12));
    assert_eq!(outcome.tokens.output_tokens(), None);
    assert!(
        outcome.tokens.unreadable().is_empty(),
        "null is how fx says the provider was silent, not a garbled count"
    );
}

#[test]
fn a_count_that_is_not_a_number_is_still_reported_as_unreadable() {
    let stdout =
        br#"{"type":"result","final_output":"ok","exit_code":0,"usage":{"input_tokens":"lots","output_tokens":3}}
"#;
    let outcome = parse_outcome(stdout, 1, Some(0));
    assert_eq!(outcome.tokens.input_tokens(), None);
    assert_eq!(outcome.tokens.unreadable().len(), 1);
}

#[test]
fn a_result_that_names_an_error_is_a_failed_run_even_when_the_exit_code_is_zero() {
    let stdout = br#"{"type":"result","final_output":"","exit_code":0,"error":"ProviderFailed","usage":{}}
"#;
    let outcome = parse_outcome(stdout, 1, Some(0));
    assert!(matches!(outcome.status, RunStatus::Failed));
}

#[test]
fn a_nonzero_exit_is_a_failed_run() {
    let stdout =
        br#"{"type":"result","final_output":"partial","exit_code":6,"usage":{"input_tokens":1,"output_tokens":1}}
"#;
    let outcome = parse_outcome(stdout, 1, Some(6));
    assert!(matches!(outcome.status, RunStatus::Failed));
    assert_eq!(outcome.exit_code, Some(6));
    assert_eq!(outcome.final_text, "partial");
}

#[test]
fn a_stream_with_no_result_line_is_a_failed_run() {
    let stdout = br#"{"protocol_version":1,"seq":1,"session_id":"s","turn_id":1,"event":{"type":"text_chunk","origin":"assistant","text":"cut off"}}
"#;
    let outcome = parse_outcome(stdout, 1, None);
    assert!(matches!(outcome.status, RunStatus::Failed));
}

#[test]
fn each_grant_becomes_its_own_permission_mode() {
    let mode_after_flag = |grant| {
        let args = build_args(None, grant, "do the thing");
        let flag = args
            .iter()
            .position(|arg| arg == "--permission-mode")
            .expect("every invocation names its permission mode");
        args[flag + 1].clone()
    };
    assert_eq!(mode_after_flag(PermissionGrant::WorkspaceWrite), OsString::from("auto"));
    assert_eq!(mode_after_flag(PermissionGrant::Unrestricted), OsString::from("yolo"));
    assert_eq!(
        Runner::Fx.effective_permission_grant(PermissionGrant::WorkspaceWrite),
        PermissionGrant::WorkspaceWrite
    );
}

#[test]
fn a_prompt_that_starts_with_a_dash_reaches_the_model_as_text() {
    let args = build_args(Some("gpt-6.2-astra"), PermissionGrant::WorkspaceWrite, "--help me");
    let tail: Vec<&OsString> = args.iter().rev().take(2).collect();
    assert_eq!(tail, vec![&OsString::from("--help me"), &OsString::from("--")]);
    assert!(args
        .windows(2)
        .any(|pair| pair == [OsString::from("--model"), OsString::from("gpt-6.2-astra")]));
    assert!(args.contains(&OsString::from("--no-save")));
    assert_eq!(args[0], OsString::from("ask"));
}
