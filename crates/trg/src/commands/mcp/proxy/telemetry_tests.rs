use opentelemetry::trace::{SpanId, Status, TraceId};
use opentelemetry::Value as OtelValue;
use serde_json::json;

use super::*;
use crate::telemetry::testing::{capture, capture_metrics, CapturedTrace};

const HOST_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";
const HOST_SPAN: &str = "b7ad6b7169203331";

fn host(message: Value) -> RxJsonRpcMessage<RoleServer> {
    serde_json::from_value(message).expect("valid host message")
}

fn remote(message: Value) -> RxJsonRpcMessage<RoleClient> {
    serde_json::from_value(message).expect("valid remote message")
}

fn tool_call(id: u32) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": "get_weather", "arguments": { "city": "Lima" } },
    })
}

fn session(content: ContentCapture) -> Session {
    Session::start("weather", Endpoint::parse("https://mcp.example.com/mcp"), content)
}

fn string(trace: &CapturedTrace, span: &str, key: &str) -> Option<String> {
    trace.attribute(span, key).map(|value| value.to_string())
}

fn failed(trace: &CapturedTrace, span: &str) -> bool {
    matches!(trace.span(span).map(|s| &s.status), Some(Status::Error { .. }))
}

#[test]
fn a_tools_call_pairs_with_its_response_under_the_session() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(tool_call(7)));
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0",
            "id": 7,
            "result": { "content": [{ "type": "text", "text": "sunny" }], "isError": false },
        })));
        assert!(session.in_flight.is_empty());
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    let span = "tools/call get_weather";
    trace.assert_child_of(span, "mcp session");
    assert_eq!(string(&trace, span, MCP_METHOD_NAME).as_deref(), Some("tools/call"));
    assert_eq!(string(&trace, span, JSONRPC_REQUEST_ID).as_deref(), Some("7"));
    assert_eq!(string(&trace, span, GEN_AI_TOOL_NAME).as_deref(), Some("get_weather"));
    assert_eq!(
        string(&trace, span, GEN_AI_OPERATION_NAME).as_deref(),
        Some(gen_ai_operation_name::EXECUTE_TOOL)
    );
    assert_eq!(string(&trace, span, SERVER_ADDRESS).as_deref(), Some("mcp.example.com"));
    assert_eq!(trace.attribute(span, SERVER_PORT), Some(OtelValue::I64(443)));
    assert_eq!(string(&trace, span, NETWORK_TRANSPORT).as_deref(), Some(TCP));
    assert_eq!(
        trace.span(span).map(|s| s.span_kind.clone()),
        Some(opentelemetry::trace::SpanKind::Client)
    );
    assert!(trace.attribute(span, ERROR_TYPE).is_none());
    assert!(trace.attribute(span, GEN_AI_TOOL_CALL_ARGUMENTS).is_none());
    assert!(trace.attribute(span, GEN_AI_TOOL_CALL_RESULT).is_none());
    assert_eq!(
        string(&trace, "mcp session", MCP_EXIT_REASON).as_deref(),
        Some("host_eof")
    );
    assert!(!failed(&trace, "mcp session"));
}

#[test]
fn a_request_with_no_named_target_is_named_by_its_method_alone() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(json!({
            "jsonrpc": "2.0", "id": "a", "method": "resources/read",
            "params": { "uri": "file:///notes.md" },
        })));
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0", "id": "a", "result": { "contents": [] },
        })));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    assert_eq!(
        string(&trace, "resources/read", MCP_RESOURCE_URI).as_deref(),
        Some("file:///notes.md")
    );
    assert_eq!(
        string(&trace, "resources/read", JSONRPC_REQUEST_ID).as_deref(),
        Some("a")
    );
}

#[test]
fn a_prompt_request_is_named_after_its_prompt() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(json!({
            "jsonrpc": "2.0", "id": 1, "method": "prompts/get", "params": { "name": "summarize" },
        })));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    assert_eq!(
        string(&trace, "prompts/get summarize", GEN_AI_PROMPT_NAME).as_deref(),
        Some("summarize")
    );
}

#[test]
fn tool_arguments_and_results_are_captured_only_when_content_capture_allows() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::SpanOnly);
        session.on_host_message(&mut host(tool_call(1)));
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "content": [{ "type": "text", "text": "sunny" }] },
        })));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    let span = "tools/call get_weather";
    assert_eq!(
        string(&trace, span, GEN_AI_TOOL_CALL_ARGUMENTS).as_deref(),
        Some(r#"{"city":"Lima"}"#)
    );
    assert!(string(&trace, span, GEN_AI_TOOL_CALL_RESULT).is_some_and(|result| result.contains("sunny")));
}

#[test]
fn a_tool_result_flagged_as_an_error_fails_the_span() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(tool_call(1)));
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": { "content": [{ "type": "text", "text": "no such city" }], "isError": true },
        })));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    assert_eq!(
        string(&trace, "tools/call get_weather", ERROR_TYPE).as_deref(),
        Some(TOOL_ERROR)
    );
    assert!(failed(&trace, "tools/call get_weather"));
}

#[test]
fn a_json_rpc_error_records_its_code() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(tool_call(3)));
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0", "id": 3, "error": { "code": -32602, "message": "unknown tool" },
        })));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    let span = "tools/call get_weather";
    assert_eq!(
        string(&trace, span, RPC_RESPONSE_STATUS_CODE).as_deref(),
        Some("-32602")
    );
    assert_eq!(string(&trace, span, ERROR_TYPE).as_deref(), Some("-32602"));
    assert!(failed(&trace, span));
}

#[test]
fn the_negotiated_protocol_version_lands_on_the_session_and_later_requests() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(json!({
            "jsonrpc": "2.0", "id": 0, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "host", "version": "1" },
            },
        })));
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0", "id": 0,
            "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "serverInfo": { "name": "weather", "version": "1" },
            },
        })));
        session.on_host_message(&mut host(tool_call(1)));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    assert_eq!(
        string(&trace, "mcp session", MCP_PROTOCOL_VERSION).as_deref(),
        Some("2025-06-18")
    );
    assert_eq!(
        string(&trace, "tools/call get_weather", MCP_PROTOCOL_VERSION).as_deref(),
        Some("2025-06-18")
    );
}

#[test]
fn the_session_id_the_transport_saw_lands_on_the_session() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(tool_call(1)));
        session.session_id().observe("abc-123");
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0", "id": 1, "result": { "content": [] },
        })));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    assert_eq!(
        string(&trace, "mcp session", MCP_SESSION_ID).as_deref(),
        Some("abc-123")
    );
    assert_eq!(
        string(&trace, "tools/call get_weather", MCP_SESSION_ID).as_deref(),
        Some("abc-123")
    );
}

#[test]
fn host_trace_context_is_adopted_and_rewritten_to_the_proxy_span() {
    let (forwarded, trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        let mut message = host(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": {
                "traceparent": format!("00-{HOST_TRACE}-{HOST_SPAN}-01"),
                "tracestate": "vendor=value",
                "baggage": "tenant=acme",
                "progressToken": 9,
            } },
        }));
        session.on_host_message(&mut message);
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
        serde_json::to_value(&message).expect("serializable")
    });

    let span = trace.span("tools/list").expect("request span");
    assert_eq!(span.span_context.trace_id(), TraceId::from_hex(HOST_TRACE).unwrap());
    assert_eq!(span.parent_span_id, SpanId::from_hex(HOST_SPAN).unwrap());
    let session = trace.span("mcp session").expect("session span");
    assert!(span
        .links
        .iter()
        .any(|link| link.span_context.span_id() == session.span_context.span_id()));

    let meta = &forwarded["params"]["_meta"];
    assert_eq!(
        meta["traceparent"],
        json!(format!("00-{HOST_TRACE}-{}-01", span.span_context.span_id()))
    );
    assert_eq!(meta["tracestate"], json!("vendor=value"));
    assert_eq!(meta["baggage"], json!("tenant=acme"));
    assert_eq!(meta["progressToken"], json!(9));
}

#[test]
fn a_request_without_trace_context_gains_one_pointing_at_the_proxy_span() {
    let (forwarded, trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        let mut message = host(tool_call(1));
        session.on_host_message(&mut message);
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
        serde_json::to_value(&message).expect("serializable")
    });

    let span = trace.span("tools/call get_weather").expect("request span");
    let meta = &forwarded["params"]["_meta"];
    assert_eq!(
        meta["traceparent"],
        json!(format!(
            "00-{}-{}-01",
            span.span_context.trace_id(),
            span.span_context.span_id()
        ))
    );
    assert!(meta.get("tracestate").is_none());
}

#[test]
fn requests_pass_through_unchanged_when_no_tracer_is_installed() {
    let original = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "get_weather",
            "arguments": { "city": "Lima" },
            "_meta": { "traceparent": format!("00-{HOST_TRACE}-{HOST_SPAN}-01") },
        },
    });
    let without_meta = tool_call(2);

    let forwarded = tracing::subscriber::with_default(tracing_subscriber::registry(), || {
        let mut session = session(ContentCapture::SpanAndEvent);
        let mut with_meta = host(original.clone());
        let mut bare = host(without_meta.clone());
        session.on_host_message(&mut with_meta);
        session.on_host_message(&mut bare);
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
        (
            serde_json::to_value(&with_meta).expect("serializable"),
            serde_json::to_value(&bare).expect("serializable"),
        )
    });

    assert_eq!(forwarded.0, original);
    assert_eq!(forwarded.1, without_meta);
}

#[test]
fn requests_still_in_flight_at_exit_end_with_the_exit_reason() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(tool_call(1)));
        session.finish(SessionEnd::Exited(ExitReason::RemoteClosed));
    });

    assert_eq!(
        string(&trace, "tools/call get_weather", ERROR_TYPE).as_deref(),
        Some("remote_closed")
    );
    assert!(failed(&trace, "tools/call get_weather"));
    assert_eq!(
        string(&trace, "mcp session", MCP_EXIT_REASON).as_deref(),
        Some("remote_closed")
    );
    assert_eq!(
        string(&trace, "mcp session", ERROR_TYPE).as_deref(),
        Some("remote_closed")
    );
    assert!(failed(&trace, "mcp session"));
}

#[test]
fn a_refused_request_records_its_operation_duration_with_the_error_code() {
    let JsonRpcMessage::Request(request) = host(tool_call(3)) else {
        panic!("a tools/call is a request");
    };
    let error = ErrorData::new(rmcp::model::ErrorCode::INTERNAL_ERROR, "no backend", None);

    let (((), trace), points) = capture_metrics(|meter| {
        capture(|| {
            let connection = Connection::new("weather", None);
            RequestSpan::open(
                &request.request,
                &request.id,
                &Span::current(),
                &connection,
                ContentCapture::NoContent,
            )
            .refuse(&error, &connection, &operation_duration(meter));
        })
    });

    let code = error.code.0.to_string();
    assert_eq!(
        string(&trace, "tools/call get_weather", ERROR_TYPE).as_deref(),
        Some(code.as_str())
    );
    let point = points
        .iter()
        .find(|point| point.name == MCP_CLIENT_OPERATION_DURATION)
        .expect("a refused request still has a duration");
    assert_eq!(point.count, 1);
    assert_eq!(point.attribute(ERROR_TYPE).as_deref(), Some(code.as_str()));
    assert_eq!(point.attribute(MCP_METHOD_NAME).as_deref(), Some("tools/call"));
    assert_eq!(point.attribute(GEN_AI_TOOL_NAME).as_deref(), Some("get_weather"));
    assert_eq!(point.attribute(MCP_SERVER_NAME).as_deref(), Some("weather"));
}

#[test]
fn a_session_that_never_bridged_fails_without_an_exit_reason() {
    let ((), trace) = capture(|| {
        let session = session(ContentCapture::NoContent);
        session.record_auth(AuthOutcome::AlreadyAuthorized);
        session.finish(SessionEnd::Failed("not_a_terminal"));
    });

    assert!(trace.attribute("mcp session", MCP_EXIT_REASON).is_none());
    assert_eq!(
        string(&trace, "mcp session", ERROR_TYPE).as_deref(),
        Some("not_a_terminal")
    );
    assert_eq!(
        string(&trace, "mcp session", MCP_AUTH_OUTCOME).as_deref(),
        Some("already_authorized")
    );
}

#[test]
fn notifications_become_session_events() {
    let ((), trace) = capture(|| {
        let mut session = session(ContentCapture::NoContent);
        session.on_host_message(&mut host(json!({
            "jsonrpc": "2.0", "method": "notifications/initialized",
        })));
        session.on_remote_message(&remote(json!({
            "jsonrpc": "2.0", "method": "notifications/tools/list_changed",
        })));
        session.finish(SessionEnd::Exited(ExitReason::HostEof));
    });

    let session = trace.span("mcp session").expect("session span");
    let methods: Vec<String> = session
        .events
        .iter()
        .filter(|event| event.name == "mcp notification")
        .filter_map(|event| {
            event
                .attributes
                .iter()
                .find(|kv| kv.key.as_str() == MCP_METHOD_NAME)
                .map(|kv| kv.value.to_string())
        })
        .collect();
    assert_eq!(
        methods,
        vec!["notifications/initialized", "notifications/tools/list_changed"]
    );
    assert_eq!(trace.spans.len(), 1);
}

#[test]
fn endpoints_carry_the_scheme_default_port() {
    assert_eq!(
        Endpoint::parse("http://localhost/mcp"),
        Some(Endpoint {
            address: "localhost".to_string(),
            port: Some(80),
        })
    );
    assert_eq!(Endpoint::parse("not a url"), None);
}
