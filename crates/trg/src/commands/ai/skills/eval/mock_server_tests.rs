use serde_json::json;

use super::*;
use crate::agentskills::mocks::MockType;
use crate::telemetry::testing::capture_metrics;

fn untracked() -> OperationDuration {
    OperationDuration::on(&opentelemetry::global::meter("trg"))
}

fn declaration(body: &str) -> MockDeclaration {
    MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::new(),
        error: None,
        body: body.to_string(),
    }
}

/// A declared tool nobody called still owes a reader the artifact saying so, and the
/// only moment that can be claimed honestly is once this server holds its mocks.
#[test]
fn a_server_that_loaded_its_mocks_leaves_a_log_before_any_call_reaches_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("mock-calls.jsonl");

    open_call_log(&path).unwrap();

    assert!(path.is_file());
    assert_eq!(fs::read_to_string(&path).unwrap(), "");
}

/// Two servers share one log, and a second one to come up must not truncate what the
/// first has already answered for.
#[test]
fn a_second_server_coming_up_keeps_the_calls_the_first_already_logged() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("mock-calls.jsonl");
    fs::write(&path, "{}\n").unwrap();

    open_call_log(&path).unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
}

#[test]
fn initialize_echoes_the_requested_protocol_version() {
    let server = ServerName::from("github");
    let tools = BTreeMap::new();
    let request =
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}});
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls, &untracked())
        .unwrap()
        .unwrap();

    assert_eq!(response["result"]["protocolVersion"], "2025-03-26");
}

#[test]
fn tools_list_names_every_declared_tool() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(ToolName::from("create_issue"), declaration("created"));
    let request = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls, &untracked())
        .unwrap()
        .unwrap();

    let names: Vec<&str> = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["create_issue"]);
}

#[test]
fn tools_call_answers_from_the_mock_and_logs_the_call() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(ToolName::from("create_issue"), declaration("issue created"));
    let request = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "create_issue", "arguments": {"repo": "acme/repo"}},
    });
    let calls_path = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls_path, &untracked())
        .unwrap()
        .unwrap();

    assert_eq!(response["result"]["content"][0]["text"], "issue created");
    assert_eq!(response["result"]["isError"], false);

    let logged = fs::read_to_string(&calls_path).unwrap();
    let entry: MockCallLogEntry = serde_json::from_str(logged.trim()).unwrap();
    assert_eq!(entry.tool.as_str(), "create_issue");
}

fn declaration_with_expect(
    body: &str,
    path: &str,
    constraint: crate::agentskills::mocks::ExpectConstraint,
) -> MockDeclaration {
    MockDeclaration {
        mock_type: MockType::Fixed,
        expect: BTreeMap::from([(crate::agentskills::mocks::ExpectPath::from(path), constraint)]),
        error: None,
        body: body.to_string(),
    }
}

#[test]
fn a_violated_expectation_is_logged_against_the_call_even_though_the_call_still_succeeds() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(
        ToolName::from("create_issue"),
        declaration_with_expect(
            "issue created",
            "repo",
            crate::agentskills::mocks::ExpectConstraint::Literal {
                value: "acme/widgets".to_string(),
            },
        ),
    );
    let request = json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "create_issue", "arguments": {"repo": "acme/other-repo"}},
    });
    let calls_path = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls_path, &untracked())
        .unwrap()
        .unwrap();

    assert_eq!(
        response["result"]["content"][0]["text"], "issue created",
        "a violation never stops the mock from answering the call"
    );

    let logged = fs::read_to_string(&calls_path).unwrap();
    let entry: MockCallLogEntry = serde_json::from_str(logged.trim()).unwrap();
    assert_eq!(entry.violations.len(), 1, "logged: {logged}");
    assert_eq!(entry.violations[0].path.as_str(), "repo");
}

#[test]
fn a_violation_is_still_logged_when_the_same_call_fails_substitution() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(
        ToolName::from("create_issue"),
        declaration_with_expect(
            "created {{input.issue_number}}",
            "repo",
            crate::agentskills::mocks::ExpectConstraint::Literal {
                value: "acme/widgets".to_string(),
            },
        ),
    );
    let request = json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {"name": "create_issue", "arguments": {"repo": "acme/other-repo"}},
    });
    let calls_path = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls_path, &untracked())
        .unwrap()
        .unwrap();

    assert_eq!(response["error"]["code"], MOCK_SUBSTITUTION_FAILED);

    let logged = fs::read_to_string(&calls_path).unwrap();
    let entry: MockCallLogEntry = serde_json::from_str(logged.trim()).unwrap();
    assert_eq!(
        entry.violations.len(),
        1,
        "a call that breaks expect keeps its violation even when the mock cannot answer it: {logged}"
    );
    assert_eq!(entry.violations[0].path.as_str(), "repo");
}

#[test]
fn an_unknown_tool_is_an_invalid_params_error_not_a_panic() {
    let server = ServerName::from("github");
    let tools = BTreeMap::new();
    let request = json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "does_not_exist", "arguments": {}},
    });
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls, &untracked())
        .unwrap()
        .unwrap();

    assert_eq!(response["error"]["code"], INVALID_PARAMS);
}

#[test]
fn any_other_method_is_a_method_not_found_error() {
    let server = ServerName::from("github");
    let tools = BTreeMap::new();
    let request = json!({"jsonrpc": "2.0", "id": 5, "method": "resources/list"});
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls, &untracked())
        .unwrap()
        .unwrap();

    assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
}

#[test]
fn a_notification_without_an_id_gets_no_response() {
    let server = ServerName::from("github");
    let tools = BTreeMap::new();
    let request = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls, &untracked()).unwrap();

    assert!(response.is_none());
}

#[test]
fn an_unresolved_placeholder_is_a_jsonrpc_error_not_a_success_with_empty_text() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(ToolName::from("create_issue"), declaration("{{input.missing}}"));
    let request = json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {"name": "create_issue", "arguments": {}},
    });
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let response = handle_request(&request, &server, &tools, &calls, &untracked())
        .unwrap()
        .unwrap();

    assert_eq!(response["error"]["code"], MOCK_SUBSTITUTION_FAILED);
}

#[test]
fn the_full_stdio_loop_answers_a_request_written_on_one_line() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(ToolName::from("create_issue"), declaration("created"));
    let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/list\"}\n".to_vec();
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();
    let mut output = Vec::new();

    run_stdio_loop(input.as_slice(), &mut output, &server, &tools, &calls, &untracked()).unwrap();

    let response: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(response["result"]["tools"][0]["name"], "create_issue");
}

#[test]
fn a_tool_call_is_a_server_span_named_for_the_tool() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(ToolName::from("create_issue"), declaration("issue created"));
    let request = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "create_issue", "arguments": {}},
    });
    let calls_path = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let (_, trace) = crate::telemetry::testing::capture(|| {
        handle_request(&request, &server, &tools, &calls_path, &untracked()).unwrap()
    });

    let span = trace.span("tools/call create_issue").expect("server span exported");
    assert_eq!(span.span_kind, opentelemetry::trace::SpanKind::Server);
    let name = "tools/call create_issue";
    assert_eq!(trace.attribute(name, MCP_METHOD_NAME).unwrap().as_str(), "tools/call");
    assert_eq!(
        trace.attribute(name, GEN_AI_TOOL_NAME).unwrap().as_str(),
        "create_issue"
    );
    assert_eq!(trace.attribute(name, JSONRPC_REQUEST_ID).unwrap().as_str(), "7");
    assert_eq!(trace.attribute(name, EVAL_MOCK_MATCH).unwrap().as_str(), "matched");
    assert_eq!(trace.attribute(name, EVAL_MOCK_SERVER).unwrap().as_str(), "github");

    let logged = fs::read_to_string(&calls_path).unwrap();
    let entry: MockCallLogEntry = serde_json::from_str(logged.trim()).unwrap();
    assert!(entry.received_at.is_some());
}

#[test]
fn a_call_to_an_undeclared_tool_marks_its_span_failed() {
    let server = ServerName::from("github");
    let tools = BTreeMap::new();
    let request = json!({
        "jsonrpc": "2.0", "id": 8, "method": "tools/call",
        "params": {"name": "delete_repo", "arguments": {}},
    });
    let calls_path = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let (_, trace) = crate::telemetry::testing::capture(|| {
        handle_request(&request, &server, &tools, &calls_path, &untracked()).unwrap()
    });

    let name = "tools/call delete_repo";
    let span = trace.span(name).expect("server span exported");
    assert!(matches!(span.status, opentelemetry::trace::Status::Error { .. }));
    assert_eq!(trace.attribute(name, EVAL_MOCK_MATCH).unwrap().as_str(), "unknown_tool");
    assert_eq!(
        trace.attribute(name, RPC_RESPONSE_STATUS_CODE).unwrap().as_str(),
        INVALID_PARAMS.to_string()
    );
}

#[test]
fn every_answered_request_records_its_server_duration_with_why_it_failed() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(ToolName::from("create_issue"), declaration("issue created"));
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "create_issue", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "delete_repo", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ];

    let ((), points) = capture_metrics(|meter| {
        let durations = OperationDuration::on(meter);
        for request in &requests {
            handle_request(request, &server, &tools, &calls, &durations).unwrap();
        }
    });

    let points: Vec<_> = points
        .iter()
        .filter(|point| point.name == MCP_SERVER_OPERATION_DURATION)
        .collect();
    assert_eq!(points.len(), 2, "one series per outcome, none for the notification");
    let answered = points
        .iter()
        .find(|point| point.attribute(GEN_AI_TOOL_NAME).as_deref() == Some("create_issue"))
        .expect("the answered call has a duration");
    assert_eq!(answered.count, 1);
    assert_eq!(answered.attribute(MCP_METHOD_NAME).as_deref(), Some("tools/call"));
    assert_eq!(answered.attribute(NETWORK_TRANSPORT).as_deref(), Some(PIPE));
    assert_eq!(answered.attribute(ERROR_TYPE), None);

    let refused = points
        .iter()
        .find(|point| point.attribute(GEN_AI_TOOL_NAME).as_deref() == Some("delete_repo"))
        .expect("the refused call has a duration");
    let code = INVALID_PARAMS.to_string();
    assert_eq!(refused.attribute(ERROR_TYPE).as_deref(), Some(code.as_str()));
    assert_eq!(
        refused.attribute(RPC_RESPONSE_STATUS_CODE).as_deref(),
        Some(code.as_str())
    );
}

#[test]
fn a_tool_answer_flagged_as_an_error_is_a_tool_error() {
    let server = ServerName::from("github");
    let mut tools = BTreeMap::new();
    tools.insert(
        ToolName::from("create_issue"),
        MockDeclaration {
            error: Some("rate limited".to_string()),
            ..declaration("")
        },
    );
    let request = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                             "params": {"name": "create_issue", "arguments": {}}});
    let calls = tempfile::NamedTempFile::new().unwrap().into_temp_path().to_path_buf();

    let ((response, trace), points) = capture_metrics(|meter| {
        let durations = OperationDuration::on(meter);
        crate::telemetry::testing::capture(|| {
            handle_request(&request, &server, &tools, &calls, &durations)
                .unwrap()
                .unwrap()
        })
    });

    assert_eq!(response["result"]["isError"], true);
    let name = "tools/call create_issue";
    assert_eq!(trace.attribute(name, ERROR_TYPE).unwrap().as_str(), TOOL_ERROR);
    let point = points
        .iter()
        .find(|point| point.name == MCP_SERVER_OPERATION_DURATION)
        .expect("the call has a duration");
    assert_eq!(point.attribute(ERROR_TYPE).as_deref(), Some(TOOL_ERROR));
}
