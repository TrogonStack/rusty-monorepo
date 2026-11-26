use super::*;
use crate::agentskills::report::CacheTokens;
use std::ffi::OsString;

/// Shaped after a captured `pi --mode json -p` run: one call that ran a tool, then the
/// call that answered.
const STREAM: &[u8] = br#"{"type":"session","version":3,"id":"s","cwd":"/w"}
{"type":"agent_start"}
{"type":"turn_start"}
{"type":"message_end","message":{"role":"user","content":[{"type":"text","text":"read it"}]}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_1","name":"read","arguments":{"path":"notes.txt"}}],"stopReason":"toolUse","usage":{"input":3,"output":27,"cacheRead":0,"cacheWrite":1157,"reasoning":0}}}
{"type":"tool_execution_start","toolCallId":"call_1","toolName":"read","args":{"path":"notes.txt"}}
{"type":"tool_execution_end","toolCallId":"call_1","toolName":"read","result":{"content":[{"type":"text","text":"hello"}]},"isError":false}
{"type":"turn_end","message":{"role":"assistant"},"toolResults":[]}
{"type":"turn_start"}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"The file says hello."}],"stopReason":"stop","usage":{"input":4,"output":9,"cacheRead":1157,"cacheWrite":30,"reasoning":5}}}
{"type":"turn_end","message":{"role":"assistant"},"toolResults":[]}
{"type":"agent_end","willRetry":false}
{"type":"agent_settled","aborted":false}
"#;

#[test]
fn a_finished_run_answers_with_its_last_message_and_adds_up_every_call() {
    let outcome = parse_outcome(STREAM, 4321, Some(0));
    assert!(matches!(outcome.status, RunStatus::Completed));
    assert_eq!(outcome.final_text, "The file says hello.");
    assert_eq!(
        outcome.duration_ms, 4321,
        "pi reports no run duration, so trg's wall time stands"
    );
    assert_eq!(outcome.tokens.input_tokens(), Some(7));
    assert_eq!(
        outcome.tokens.output_tokens(),
        Some(36),
        "pi's output already covers reasoning, which is not added again"
    );
    assert_eq!(
        outcome.tokens.cached_tokens(),
        Some(CacheTokens::parse(Some(1157), Some(1187)).unwrap())
    );
    assert_eq!(
        outcome.cost.and_then(|cost| cost.usd()),
        None,
        "pi's cost is not a published price"
    );
}

/// pi exits zero when the provider refused it; the stop reason is what says so.
#[test]
fn a_provider_error_fails_the_run_though_pi_exits_zero() {
    let stdout = br#"{"type":"turn_start"}
{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"homelab API error (401): authentication failed"}}
{"type":"agent_end","willRetry":false}
"#;
    let outcome = parse_outcome(stdout, 1, Some(0));
    assert!(matches!(outcome.status, RunStatus::Failed));
    assert!(outcome.final_text.contains("401"), "{}", outcome.final_text);
}

#[test]
fn a_nonzero_exit_fails_the_run() {
    assert!(matches!(parse_outcome(STREAM, 1, Some(1)).status, RunStatus::Failed));
}

#[test]
fn a_run_that_never_answered_fails() {
    assert!(matches!(
        parse_outcome(br#"{"type":"agent_start"}"#, 1, Some(0)).status,
        RunStatus::Failed
    ));
}

#[test]
fn every_run_is_one_ephemeral_prompt_and_a_prompt_that_starts_with_a_dash_stays_text() {
    let args = build_args(Some("homelab/model"), "-x");
    for flag in ["--mode", "json", "-p", "--no-session"] {
        assert!(args.contains(&OsString::from(flag)), "{flag}");
    }
    let tail: Vec<_> = args.iter().rev().take(2).collect();
    assert_eq!(tail, [&OsString::from("-x"), &OsString::from("--")]);
}
