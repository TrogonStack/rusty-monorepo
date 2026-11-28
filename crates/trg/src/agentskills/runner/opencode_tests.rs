use super::*;
use crate::agentskills::report::CacheTokens;
use std::ffi::OsString;

/// Shaped after a captured `opencode run --format json` run: one step that called a tool,
/// then the step that answered.
const STREAM: &[u8] = br#"{"type":"step_start","timestamp":1,"sessionID":"s","part":{"type":"step-start"}}
{"type":"tool_use","timestamp":2,"sessionID":"s","part":{"type":"tool","id":"call_1","tool":"read","state":{"status":"completed","input":{"path":"notes.txt"},"output":"hello","time":{"start":2,"end":3}}}}
{"type":"step_finish","timestamp":4,"sessionID":"s","part":{"type":"step-finish","reason":"tool-calls","cost":0,"tokens":{"input":3,"output":18,"reasoning":2,"cache":{"read":0,"write":9000}}}}
{"type":"step_start","timestamp":5,"sessionID":"s","part":{"type":"step-start"}}
{"type":"text","timestamp":6,"sessionID":"s","part":{"type":"text","text":"The file says hello."}}
{"type":"step_finish","timestamp":7,"sessionID":"s","part":{"type":"step-finish","reason":"stop","cost":0,"tokens":{"input":4,"output":9,"reasoning":0,"cache":{"read":9000,"write":30}}}}
"#;

#[test]
fn a_finished_run_answers_with_its_last_step_and_adds_up_every_step() {
    let outcome = parse_outcome(STREAM, 4321, Some(0));
    assert!(matches!(outcome.status, RunStatus::Completed));
    assert_eq!(outcome.final_text, "The file says hello.");
    assert_eq!(
        outcome.duration_ms, 4321,
        "opencode reports no duration, so trg's wall time stands"
    );
    assert_eq!(outcome.tokens.input_tokens(), Some(7));
    assert_eq!(
        outcome.tokens.output_tokens(),
        Some(29),
        "reasoning is output the model generated"
    );
    assert_eq!(
        outcome.tokens.cached_tokens(),
        Some(CacheTokens::parse(Some(9000), Some(9030)).unwrap())
    );
    assert_eq!(
        outcome.cost.and_then(|cost| cost.usd()),
        None,
        "opencode's cost is not a published price"
    );
}

#[test]
fn an_error_event_fails_the_run_and_names_the_error() {
    let stdout = br#"{"type":"step_start","timestamp":1,"sessionID":"s","part":{"type":"step-start"}}
{"type":"error","timestamp":2,"sessionID":"s","error":{"type":"provider.auth","message":"Provider request failed with HTTP 401: authentication failed"}}
"#;
    let outcome = parse_outcome(stdout, 1, Some(0));
    assert!(matches!(outcome.status, RunStatus::Failed));
    assert!(outcome.final_text.contains("HTTP 401"), "{}", outcome.final_text);
}

#[test]
fn a_nonzero_exit_fails_the_run() {
    assert!(matches!(parse_outcome(STREAM, 1, Some(1)).status, RunStatus::Failed));
}

#[test]
fn a_run_that_never_started_a_step_fails() {
    assert!(matches!(parse_outcome(b"", 1, Some(0)).status, RunStatus::Failed));
}

#[test]
fn a_provider_that_reports_no_cache_reports_no_cached_tokens() {
    let stdout = br#"{"type":"step_start","part":{}}
{"type":"text","part":{"text":"ok"}}
{"type":"step_finish","part":{"tokens":{"input":5,"output":1}}}
"#;
    let outcome = parse_outcome(stdout, 1, Some(0));
    assert_eq!(outcome.tokens.input_tokens(), Some(5));
    assert_eq!(outcome.tokens.cached_tokens(), None);
}

/// A step that garbles one count must not leave a smaller total that looks complete.
#[test]
fn a_garbled_count_in_one_step_is_reported_unreadable_rather_than_summed_around() {
    let stdout = br#"{"type":"step_start","part":{}}
{"type":"step_finish","part":{"tokens":{"input":5,"output":1}}}
{"type":"step_start","part":{}}
{"type":"text","part":{"text":"ok"}}
{"type":"step_finish","part":{"tokens":{"input":"lots","output":2}}}
"#;
    let outcome = parse_outcome(stdout, 1, Some(0));
    assert_eq!(outcome.tokens.input_tokens(), None);
    assert_eq!(outcome.tokens.output_tokens(), Some(3));
    assert_eq!(outcome.tokens.unreadable().len(), 1);
    assert!(
        outcome.tokens.unreadable()[0].warning().contains("\"lots\""),
        "{}",
        outcome.tokens.unreadable()[0].warning()
    );
}

#[test]
fn only_an_unrestricted_grant_auto_approves() {
    let unrestricted = build_args(None, PermissionGrant::Unrestricted, "go");
    assert!(unrestricted.contains(&OsString::from("--auto")));
    let workspace = build_args(None, PermissionGrant::WorkspaceWrite, "go");
    assert!(!workspace.contains(&OsString::from("--auto")));
}

#[test]
fn every_run_gets_a_private_server_and_a_prompt_that_starts_with_a_dash_stays_text() {
    let args = build_args(Some("homelab/model"), PermissionGrant::WorkspaceWrite, "-x");
    assert!(args.contains(&OsString::from("--standalone")));
    let tail: Vec<_> = args.iter().rev().take(2).collect();
    assert_eq!(tail, [&OsString::from("-x"), &OsString::from("--")]);
    let model_at = args.iter().position(|arg| arg == "--model").unwrap();
    assert_eq!(args[model_at + 1], OsString::from("homelab/model"));
}
