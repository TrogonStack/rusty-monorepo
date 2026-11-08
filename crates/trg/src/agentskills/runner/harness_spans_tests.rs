use super::*;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use std::time::Duration;

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
}

fn line_count(stdout: &[u8]) -> u64 {
    stdout.split_inclusive(|byte| *byte == b'\n').count() as u64
}

/// A stream whose lines arrive one second apart, the first at second 1.
fn stamped(stdout: &[u8]) -> (StdoutTimeline, SystemTime) {
    let times: Vec<SystemTime> = (1..=line_count(stdout)).map(at).collect();
    (StdoutTimeline::stamped(stdout, &times), at(0))
}

/// Rebuilds a stream whose harness exited as its last line arrived.
fn rebuild(runner: Runner, stdout: &[u8], model: Option<&str>, content: ContentCapture) -> Reconstruction {
    let ended = StreamEnd::Exited(at(line_count(stdout)));
    rebuild_ending(runner, stdout, model, content, ended)
}

fn rebuild_ending(
    runner: Runner,
    stdout: &[u8],
    model: Option<&str>,
    content: ContentCapture,
    ended: StreamEnd,
) -> Reconstruction {
    let (timeline, started_at) = stamped(stdout);
    reconstruct(
        runner,
        HarnessStream {
            stdout,
            timeline: &timeline,
            started_at,
            ended,
        },
        model,
        content,
    )
}

fn chat_usages(reconstruction: &Reconstruction) -> Vec<TurnUsage> {
    reconstruction
        .steps
        .iter()
        .filter_map(|step| match step.kind {
            StepKind::Chat { usage, .. } => Some(usage),
            StepKind::Tool { .. } => None,
        })
        .collect()
}

fn tool<'a>(reconstruction: &'a Reconstruction, name: &str) -> &'a HarnessStep {
    reconstruction
        .steps
        .iter()
        .find(|step| matches!(&step.kind, StepKind::Tool { name: tool, .. } if tool == name))
        .unwrap_or_else(|| panic!("no tool step {name}"))
}

fn chats(reconstruction: &Reconstruction) -> Vec<&HarnessStep> {
    reconstruction.steps.iter().filter(|step| !step.is_tool()).collect()
}

const CLAUDE: &[u8] = br#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5"}
{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"Reading."}],"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}
{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"SKILL.md"}}],"usage":{"input_tokens":10,"output_tokens":7,"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"skill body"}]}}
{"type":"assistant","message":{"id":"msg_2","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"false"}}],"usage":{"input_tokens":20,"output_tokens":3}}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"exit 1","is_error":true}]}}
{"type":"assistant","message":{"id":"msg_3","model":"claude-sonnet-4-5","content":[{"type":"text","text":"Done."}],"usage":{"input_tokens":30,"output_tokens":4}}}
{"type":"result","is_error":false,"duration_ms":6500,"duration_api_ms":4200,"total_cost_usd":0.01}
"#;

#[test]
fn claude_code_tools_span_from_use_to_result() {
    let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::NoContent);

    let read = tool(&rebuilt, "Read");
    assert_eq!((read.start, read.end), (at(3), at(4)));
    assert_eq!(read.failure, None);
    assert!(
        matches!(&read.kind, StepKind::Tool { call_id, arguments: None, result: None, .. } if call_id == "toolu_1")
    );

    let bash = tool(&rebuilt, "Bash");
    assert_eq!((bash.start, bash.end), (at(5), at(6)));
    assert_eq!(bash.failure, Some(StepFailure::ToolError));
}

#[test]
fn claude_code_turns_group_by_message_and_carry_their_own_usage() {
    let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::NoContent);
    let turns = chats(&rebuilt);
    assert_eq!(turns.len(), 3);

    assert_eq!((turns[0].start, turns[0].end), (at(1), at(3)));
    assert_eq!(turns[0].name(), "chat claude-sonnet-4-5");
    assert_eq!(
        turns[0].kind,
        StepKind::Chat {
            request_model: None,
            response_model: Some("claude-sonnet-4-5".to_string()),
            response_id: Some("msg_1".to_string()),
            usage: TurnUsage {
                input: Some(10),
                output: Some(7),
                cache_read: Some(100),
                cache_write: Some(5)
            },
        }
    );
    assert_eq!((turns[1].start, turns[1].end), (at(4), at(5)));
    assert_eq!((turns[2].start, turns[2].end), (at(6), at(7)));

    assert_eq!(rebuilt.tool_calls(), 2);
    assert_eq!(rebuilt.inference_calls(), 3);
    assert_eq!(
        rebuilt.summary,
        HarnessSummary {
            duration_ms: Some(6500),
            api_duration_ms: Some(4200),
            response_model: Some("claude-sonnet-4-5".to_string()),
        }
    );
}

#[test]
fn tool_content_is_copied_only_under_span_content_capture() {
    let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::SpanOnly);
    assert!(matches!(
        &tool(&rebuilt, "Read").kind,
        StepKind::Tool { arguments: Some(arguments), result: Some(result), .. }
            if arguments == r#"{"file_path":"SKILL.md"}"# && result == "skill body"
    ));

    let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::EventOnly);
    assert!(matches!(
        &tool(&rebuilt, "Read").kind,
        StepKind::Tool {
            arguments: None,
            result: None,
            ..
        }
    ));
}

const CUT_OFF: &[u8] = br#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}]}}
{"type":"system","subtype":"status"}
"#;

#[test]
fn a_tool_cut_off_by_the_harness_exiting_ends_when_it_exited_and_is_unfinished() {
    let ended = StreamEnd::Exited(at(5));
    let rebuilt = rebuild_ending(Runner::ClaudeCode, CUT_OFF, None, ContentCapture::NoContent, ended);
    let bash = tool(&rebuilt, "Bash");
    assert_eq!((bash.start, bash.end), (at(1), at(5)));
    assert_eq!(bash.failure, Some(StepFailure::Unfinished));
}

#[test]
fn a_tool_cut_off_by_the_time_limit_ends_when_the_harness_was_killed_and_is_a_timeout() {
    let ended = StreamEnd::TimedOut(at(30));
    let rebuilt = rebuild_ending(Runner::ClaudeCode, CUT_OFF, None, ContentCapture::NoContent, ended);
    let bash = tool(&rebuilt, "Bash");
    assert_eq!((bash.start, bash.end), (at(1), at(30)));
    assert_eq!(bash.failure, Some(StepFailure::TimedOut));

    let (spans, _) = exported(&rebuilt.steps);
    let span = spans.iter().find(|span| span.name == "execute_tool Bash").unwrap();
    assert_eq!(span.end_time, at(30));
    let error_type = span
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == ERROR_TYPE)
        .map(|kv| kv.value.clone());
    assert_eq!(error_type, Some(opentelemetry::Value::from("timeout")));
}

const LEAKED_CHANNEL_MARKER: &[u8] = br#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"<|channel|>commentary<|message|>Bash","input":{"command":"false"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"exit 1","is_error":true}]}}
{"type":"result","is_error":true,"duration_ms":500}
"#;

/// This parser never inspects a tool's name beyond reading it as a string, so a name
/// carrying a leaked model-internal channel marker rebuilds exactly like any other
/// name: kept verbatim, nothing panics reading it.
#[test]
fn a_tool_name_carrying_a_leaked_channel_marker_rebuilds_without_panicking() {
    let rebuilt = rebuild(
        Runner::ClaudeCode,
        LEAKED_CHANNEL_MARKER,
        None,
        ContentCapture::NoContent,
    );

    let leaked = tool(&rebuilt, "<|channel|>commentary<|message|>Bash");
    assert_eq!(leaked.failure, Some(StepFailure::ToolError));
}

const TRUNCATED_TOOL_CALL: &[u8] = br#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls -la /very/long/path/that/got/cut/of
{"type":"result","is_error":true,"duration_ms":500}
"#;

/// A tool call's JSON arguments cut off mid-object never finishes the object on its
/// own line, so it fails `serde_json::from_str` and is skipped entirely rather than
/// opening a tool with no arguments or panicking on a line that is not valid JSON.
#[test]
fn a_tool_call_truncated_mid_object_is_dropped_rather_than_opening_a_tool() {
    let rebuilt = rebuild(Runner::ClaudeCode, TRUNCATED_TOOL_CALL, None, ContentCapture::NoContent);

    assert!(
        rebuilt.steps.iter().all(|step| !step.is_tool()),
        "a line that never finished its JSON object cannot have opened a tool"
    );
}

// Case (c), the terminal event missing entirely, is already covered by
// `a_tool_cut_off_by_the_harness_exiting_ends_when_it_exited_and_is_unfinished` above:
// the stream simply stops, and the open tool ends at `StreamEnd::Exited` marked
// `StepFailure::Unfinished` rather than being left dangling or panicking.

#[test]
fn claude_code_chat_usage_is_each_messages_last_snapshot_and_never_reconciled_with_the_result_total() {
    let stdout = br#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"a"}],"usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"b"}],"usage":{"input_tokens":10,"output_tokens":6}}}
{"type":"assistant","message":{"id":"msg_2","model":"claude-sonnet-4-5","content":[{"type":"text","text":"c"}],"usage":{"input_tokens":20,"output_tokens":4}}}
{"type":"result","is_error":false,"duration_ms":10,"usage":{"input_tokens":500,"output_tokens":90}}
"#;
    let rebuilt = rebuild(Runner::ClaudeCode, stdout, None, ContentCapture::NoContent);
    let reported = |input, output| TurnUsage {
        input: Some(input),
        output: Some(output),
        ..TurnUsage::default()
    };
    assert_eq!(chat_usages(&rebuilt), vec![reported(10, 6), reported(20, 4)]);
}

const CODEX: &[u8] = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Reading."}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"cat SKILL.md","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"cat SKILL.md","aggregated_output":"body","exit_code":0,"status":"completed"}}
{"type":"item.started","item":{"id":"item_2","type":"mcp_tool_call","server":"mock","tool":"lookup","arguments":{"q":"x"},"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","server":"mock","tool":"lookup","status":"failed","error":{"message":"boom"}}}
{"type":"item.completed","item":{"id":"item_3","type":"file_change","changes":[{"path":"out.md","kind":"add"}],"status":"completed"}}
{"type":"turn.completed","usage":{"input_tokens":200,"cached_input_tokens":150,"output_tokens":40}}
"#;

#[test]
fn codex_tools_span_from_item_start_to_completion() {
    let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);

    let command = tool(&rebuilt, "command_execution");
    assert_eq!((command.start, command.end), (at(4), at(5)));
    assert_eq!(command.failure, None);

    let lookup = tool(&rebuilt, "lookup");
    assert_eq!((lookup.start, lookup.end), (at(6), at(7)));
    assert_eq!(lookup.failure, Some(StepFailure::ToolError));

    let change = tool(&rebuilt, "file_change");
    assert_eq!((change.start, change.end), (at(8), at(8)));
    assert_eq!(rebuilt.tool_calls(), 3);
}

#[test]
fn codex_model_calls_fill_the_gaps_between_tools_without_overlapping_them() {
    let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);
    let spans: Vec<(SystemTime, SystemTime, bool)> = rebuilt
        .steps
        .iter()
        .map(|step| (step.start, step.end, step.is_tool()))
        .collect();
    assert_eq!(
        spans,
        vec![
            (at(2), at(4), false),
            (at(4), at(5), true),
            (at(5), at(6), false),
            (at(6), at(7), true),
            (at(7), at(8), false),
            (at(8), at(8), true),
            (at(8), at(9), false),
        ]
    );
    assert_eq!(rebuilt.inference_calls(), 4);
    assert!(chats(&rebuilt).iter().all(|chat| chat.name() == "chat gpt-5"));

    let unnamed = rebuild(Runner::Codex, CODEX, None, ContentCapture::NoContent);
    assert_eq!(chats(&unnamed)[0].name(), "chat");
}

#[test]
fn codex_turn_usage_lands_once_on_the_turns_last_model_call() {
    let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);
    let turn = TurnUsage {
        input: Some(200),
        output: Some(40),
        cache_read: Some(150),
        cache_write: None,
    };
    let unattributed = TurnUsage::default();
    assert_eq!(
        chat_usages(&rebuilt),
        vec![unattributed, unattributed, unattributed, turn]
    );
}

#[test]
fn a_codex_turn_without_tools_is_a_single_model_call() {
    let stdout = br#"{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Done."}}
{"type":"turn.completed","usage":{"input_tokens":20,"output_tokens":4}}
"#;
    let rebuilt = rebuild(Runner::Codex, stdout, Some("gpt-5"), ContentCapture::NoContent);
    let turns = chats(&rebuilt);
    assert_eq!(turns.len(), 1);
    assert_eq!((turns[0].start, turns[0].end), (at(1), at(3)));
}

const CODEX_CUT_OFF_AFTER_TOOL: &[u8] = br#"{"type":"turn.started"}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"ls","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"ls","aggregated_output":"","exit_code":0,"status":"completed"}}
{"type":"system","subtype":"status"}
"#;

#[test]
fn a_codex_chat_cut_off_by_the_time_limit_ends_when_the_harness_was_killed_and_is_a_timeout() {
    let ended = StreamEnd::TimedOut(at(30));
    let rebuilt = rebuild_ending(
        Runner::Codex,
        CODEX_CUT_OFF_AFTER_TOOL,
        Some("gpt-5"),
        ContentCapture::NoContent,
        ended,
    );
    let last_chat = chats(&rebuilt).into_iter().last().unwrap();
    assert_eq!(last_chat.end, at(30));
    assert_eq!(last_chat.failure, Some(StepFailure::TimedOut));

    let (spans, _) = exported(&rebuilt.steps);
    let span = spans.iter().rfind(|span| span.name == "chat gpt-5").unwrap();
    assert_eq!(span.end_time, at(30));
    let error_type = span
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == ERROR_TYPE)
        .map(|kv| kv.value.clone());
    assert_eq!(error_type, Some(opentelemetry::Value::from("timeout")));
}

#[test]
fn a_failed_codex_turn_marks_only_its_last_model_call() {
    let stdout = br#"{"type":"turn.started"}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"ls","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"ls","aggregated_output":"","exit_code":0,"status":"completed"}}
{"type":"turn.failed"}
"#;
    let rebuilt = rebuild(Runner::Codex, stdout, None, ContentCapture::NoContent);
    let failures: Vec<Option<StepFailure>> = chats(&rebuilt).iter().map(|chat| chat.failure).collect();
    assert_eq!(failures, vec![None, Some(StepFailure::TurnFailed)]);
}

const CURSOR: &[u8] = br#"{"type":"system","subtype":"init","model":"gpt-5","cwd":"/w"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Reading."}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":" more"}]}}
{"type":"tool_call","subtype":"started","call_id":"call-1","tool_call":{"readToolCall":{"args":{"path":"SKILL.md"}}}}
{"type":"thinking","text":"..."}
{"type":"tool_call","subtype":"completed","call_id":"call-1","tool_call":{"readToolCall":{"args":{"path":"SKILL.md"},"result":{"success":{"content":"body"}}}}}
{"type":"tool_call","subtype":"started","tool_call":{"editToolCall":{"args":{"path":"out.md"}},"toolCallId":"call-2"}}
{"type":"tool_call","subtype":"completed","tool_call":{"editToolCall":{"args":{"path":"out.md"},"result":{"error":{"message":"denied"}}},"toolCallId":"call-2"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}
{"type":"result","is_error":false,"duration_ms":9000,"duration_api_ms":7000,"result":"done"}
"#;

#[test]
fn cursor_agent_tools_span_from_started_to_completed() {
    let rebuilt = rebuild(Runner::CursorAgent, CURSOR, None, ContentCapture::NoContent);

    let read = tool(&rebuilt, "read");
    assert_eq!((read.start, read.end), (at(4), at(6)));
    assert_eq!(read.failure, None);

    let edit = tool(&rebuilt, "edit");
    assert_eq!((edit.start, edit.end), (at(7), at(8)));
    assert_eq!(edit.failure, Some(StepFailure::ToolError));
}

#[test]
fn cursor_agent_turns_are_runs_of_assistant_events_named_by_the_init_model() {
    let rebuilt = rebuild(Runner::CursorAgent, CURSOR, None, ContentCapture::NoContent);
    let turns = chats(&rebuilt);
    assert_eq!(turns.len(), 2);
    assert_eq!((turns[0].start, turns[0].end), (at(1), at(3)));
    assert_eq!((turns[1].start, turns[1].end), (at(8), at(9)));
    assert_eq!(turns[0].name(), "chat gpt-5");
    assert_eq!(
        rebuilt.summary,
        HarnessSummary {
            duration_ms: Some(9000),
            api_duration_ms: Some(7000),
            response_model: Some("gpt-5".to_string()),
        }
    );
}

fn exported(steps: &[HarnessStep]) -> (Vec<SpanData>, SpanData) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let tracer = provider.tracer("trg");
    let parent = tracer.start("invoke_agent Claude Code");
    let parent_cx = Context::current_with_span(parent);
    assert!(records(&parent_cx));
    emit(&tracer, &parent_cx, "anthropic", steps);
    parent_cx.span().end();
    let _ = provider.force_flush();
    let mut spans = exporter.get_finished_spans().unwrap();
    let index = spans
        .iter()
        .position(|span| span.name == "invoke_agent Claude Code")
        .unwrap();
    let parent = spans.remove(index);
    (spans, parent)
}

#[test]
fn emitted_spans_are_children_of_invoke_agent_with_the_rebuilt_times() {
    let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::NoContent);
    let (spans, parent) = exported(&rebuilt.steps);
    assert_eq!(spans.len(), rebuilt.steps.len());
    for span in &spans {
        assert_eq!(span.parent_span_id, parent.span_context.span_id(), "{}", span.name);
        assert_eq!(span.span_context.trace_id(), parent.span_context.trace_id());
    }

    let read = spans.iter().find(|span| span.name == "execute_tool Read").unwrap();
    assert_eq!((read.start_time, read.end_time), (at(3), at(4)));
    assert_eq!(read.span_kind, SpanKind::Internal);
    let attribute = |span: &SpanData, key: &str| {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    };
    assert_eq!(attribute(read, GEN_AI_OPERATION_NAME).as_deref(), Some("execute_tool"));
    assert_eq!(attribute(read, GEN_AI_TOOL_CALL_ID).as_deref(), Some("toolu_1"));
    assert_eq!(attribute(read, GEN_AI_TOOL_CALL_ARGUMENTS), None);

    let bash = spans.iter().find(|span| span.name == "execute_tool Bash").unwrap();
    assert_eq!(attribute(bash, ERROR_TYPE).as_deref(), Some("tool_error"));
    assert!(matches!(bash.status, Status::Error { .. }));

    let chat = spans.iter().find(|span| span.name == "chat claude-sonnet-4-5").unwrap();
    assert_eq!(chat.span_kind, SpanKind::Client);
    assert_eq!(
        value(chat, GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS),
        Some(opentelemetry::Value::I64(100))
    );
    assert_eq!(attribute(chat, GEN_AI_PROVIDER_NAME).as_deref(), Some("anthropic"));
}

fn value(span: &SpanData, key: &str) -> Option<opentelemetry::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

fn chat_models(
    runner: Runner,
    stdout: &[u8],
    requested: Option<&str>,
) -> Vec<(String, Option<String>, Option<String>)> {
    let rebuilt = rebuild(runner, stdout, requested, ContentCapture::NoContent);
    let (spans, _) = exported(&rebuilt.steps);
    let text = |span: &SpanData, key: &str| value(span, key).map(|value| value.to_string());
    spans
        .iter()
        .filter(|span| value(span, GEN_AI_OPERATION_NAME) == Some(opentelemetry::Value::from("chat")))
        .map(|span| {
            (
                span.name.to_string(),
                text(span, GEN_AI_REQUEST_MODEL),
                text(span, GEN_AI_RESPONSE_MODEL),
            )
        })
        .collect()
}

#[test]
fn a_chat_span_names_a_request_model_only_when_trg_requested_one() {
    let unrequested = chat_models(Runner::ClaudeCode, CLAUDE, None);
    assert!(!unrequested.is_empty());
    for (name, request, response) in unrequested {
        assert_eq!(name, "chat claude-sonnet-4-5");
        assert_eq!(request, None);
        assert_eq!(response.as_deref(), Some("claude-sonnet-4-5"));
    }

    for (name, request, response) in chat_models(Runner::ClaudeCode, CLAUDE, Some("sonnet")) {
        assert_eq!(name, "chat sonnet");
        assert_eq!(request.as_deref(), Some("sonnet"));
        assert_eq!(response.as_deref(), Some("claude-sonnet-4-5"));
    }

    for (name, request, response) in chat_models(Runner::CursorAgent, CURSOR, None) {
        assert_eq!(name, "chat gpt-5");
        assert_eq!(request, None);
        assert_eq!(response.as_deref(), Some("gpt-5"));
    }
}

#[test]
fn a_codex_chat_span_carries_the_requested_model_and_no_response_model_its_stream_never_named() {
    for (name, request, response) in chat_models(Runner::Codex, CODEX, Some("gpt-5")) {
        assert_eq!(name, "chat gpt-5");
        assert_eq!(request.as_deref(), Some("gpt-5"));
        assert_eq!(response, None);
    }
    let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);
    assert_eq!(rebuilt.summary.response_model, None);
}

#[test]
fn an_unrecorded_parent_is_recognised() {
    assert!(!records(&Context::new()));
}

#[test]
fn claude_code_measures_turns_tools_results_and_the_heaviest_turn() {
    let metrics = measure(Runner::ClaudeCode, CLAUDE);

    assert_eq!(metrics.turns, TurnCount::new(3));
    assert_eq!(metrics.tool_calls.get(), 2);
    assert_eq!(metrics.peak_turn_input_tokens, Some(115));
    let read = metrics.tools.iter().find(|tool| tool.tool == "Read").unwrap();
    assert_eq!(read.largest_result_bytes.map(|bytes| bytes.get()), Some(10));
}

#[test]
fn a_tool_called_again_with_the_same_arguments_counts_as_repeated() {
    let stdout = br#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"t1","name":"Grep","input":{"q":"x"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"[]"}]}}
{"type":"assistant","message":{"id":"msg_2","content":[{"type":"tool_use","id":"t2","name":"Grep","input":{"q":"x"}}]}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2","content":"[]"}]}}
"#;

    let metrics = measure(Runner::ClaudeCode, stdout);

    assert_eq!(metrics.repeated_tool_calls.get(), 1);
}

#[test]
fn codex_and_cursor_report_no_per_turn_input() {
    assert_eq!(measure(Runner::Codex, CODEX).peak_turn_input_tokens, None);
    assert_eq!(measure(Runner::Codex, CODEX).tool_calls.get(), 3);
    assert_eq!(measure(Runner::CursorAgent, CURSOR).peak_turn_input_tokens, None);
    assert_eq!(measure(Runner::CursorAgent, CURSOR).tool_calls.get(), 2);
}

#[test]
fn a_live_count_agrees_with_the_rebuilt_steps() {
    for (runner, stdout) in [
        (Runner::ClaudeCode, CLAUDE),
        (Runner::Codex, CODEX),
        (Runner::CursorAgent, CURSOR),
    ] {
        let rebuilt = rebuild(runner, stdout, None, ContentCapture::NoContent);
        let mut live = LiveReconstruction::counting(runner, at(0));
        let selected_items = stdout.split(|byte| *byte == b'\n');
        for line in selected_items {
            live.observe_line(line, at(1));
        }

        assert_eq!(live.tool_calls(), rebuilt.tool_calls(), "{runner:?}");
        assert_eq!(live.model_calls(), rebuilt.inference_calls(), "{runner:?}");
    }
}
