use serde_json::json;

use super::super::*;
use crate::agentskills::evals::EvalDirName;
use crate::agentskills::mocks::{resolve_call, resolve_mock_set, MockCallAnswer};
use crate::telemetry::semconv::generated::attributes::{GEN_AI_TOOL_NAME, MCP_METHOD_NAME};
use opentelemetry::trace::SpanKind;
use opentelemetry::Value;
use opentelemetry_semantic_conventions::attribute::{ERROR_TYPE, PROCESS_EXIT_CODE};
use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

const FIXTURE_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0"}}}\n' "$id"
      ;;
    *'"method":"notifications/initialized"'*)
      ;;
    *'"name":"echo"'*)
      msg=$(printf '%s' "$line" | sed -n 's/.*"message":"\([^"]*\)".*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo: %s"}],"isError":false}}\n' "$id" "$msg"
      ;;
    *'"name":"fail"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"boom"}],"isError":true}}\n' "$id"
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
      ;;
  esac
done
"#;

fn fixture_server(dir: &std::path::Path) -> RealServerCommand {
    custom_fixture_server(dir, FIXTURE_SERVER)
}

fn custom_fixture_server(dir: &std::path::Path, script: &str) -> RealServerCommand {
    let path = dir.join("fixture-mcp-server.sh");
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
    RealServerCommand::new(path.display().to_string(), vec![])
}

const FIXTURE_ID_COLLISION_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0"}}}\n' "$id"
      ;;
    *'"method":"notifications/initialized"'*)
      ;;
    *'"name":"echo"'*)
      printf '{"jsonrpc":"2.0","id":%s,"method":"sampling/createMessage","params":{}}\n' "$id"
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo: collided"}],"isError":false}}\n' "$id"
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
      ;;
  esac
done
"#;

const FIXTURE_PING_SERVER: &str = r#"#!/bin/sh
call_id=""
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0"}}}\n' "$id"
      ;;
    *'"method":"notifications/initialized"'*)
      ;;
    *'"name":"echo"'*)
      call_id=$id
      printf '{"jsonrpc":"2.0","id":999,"method":"ping"}\n'
      ;;
    *'"id":999'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo: after ping"}],"isError":false}}\n' "$call_id"
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
      ;;
  esac
done
"#;

const FIXTURE_BANNER_SERVER: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"0"}}}\n' "$id"
      ;;
    *'"method":"notifications/initialized"'*)
      ;;
    *'"name":"echo"'*)
      printf 'fixture server starting up...\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"echo: after banner"}],"isError":false}}\n' "$id"
      ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"method not found"}}\n' "$id"
      ;;
  esac
done
"#;

#[test]
fn recording_against_a_fixture_server_writes_a_fixed_mock_file() {
    let temp = tempdir().unwrap();
    let command = fixture_server(temp.path());
    let mut session = RealMcpServerSession::spawn(&command).unwrap();
    session.initialize(Duration::from_secs(5)).unwrap();

    let server = ServerName::from("echoserver");
    let tool = ToolName::from("echo");
    let input = json!({"message": "hello"});
    let answer = session.call_tool(&tool, &input, Duration::from_secs(5)).unwrap();
    assert_eq!(answer.text, "echo: hello");
    assert!(!answer.is_error);

    let mocks_dir = temp.path().join("mocks");
    let path = write_recorded_mock(&mocks_dir, &server, &tool, &input, &answer).unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("type: fixed"), "content:\n{content}");
    assert!(content.contains("\"message\": string"), "content:\n{content}");
    assert!(content.ends_with("echo: hello"), "content:\n{content}");

    session.finish(Duration::from_secs(5)).unwrap();
}

#[test]
fn each_request_to_the_real_server_is_an_mcp_client_span_under_its_process_span() {
    let temp = tempdir().unwrap();
    let command = fixture_server(temp.path());

    let ((), trace) = crate::telemetry::testing::capture(|| {
        let mut session = RealMcpServerSession::spawn(&command).unwrap();
        session.initialize(Duration::from_secs(5)).unwrap();
        session
            .call_tool(
                &ToolName::from("echo"),
                &json!({"message": "hello"}),
                Duration::from_secs(5),
            )
            .unwrap();
        session
            .call_tool(&ToolName::from("fail"), &json!({}), Duration::from_secs(5))
            .unwrap();
        session.finish(Duration::from_secs(5)).unwrap();
    });

    let process = "fixture-mcp-server.sh";
    trace.assert_child_of("initialize", process);
    trace.assert_child_of("tools/call echo", process);
    trace.assert_child_of("finish", process);
    assert_eq!(trace.span("tools/call echo").unwrap().span_kind, SpanKind::Client);
    assert_eq!(
        trace.attribute("tools/call echo", MCP_METHOD_NAME),
        Some(Value::from("tools/call"))
    );
    assert_eq!(
        trace.attribute("tools/call echo", GEN_AI_TOOL_NAME),
        Some(Value::from("echo"))
    );
    assert_eq!(
        trace.attribute("tools/call fail", ERROR_TYPE),
        Some(Value::from("tool_error"))
    );
    assert_eq!(trace.attribute(process, PROCESS_EXIT_CODE), Some(Value::I64(0)));
}

#[test]
fn a_session_dropped_without_finishing_does_not_leave_the_server_running() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("hang.sh");
    fs::write(&path, "#!/bin/sh\nsleep 100\n").unwrap();
    fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
    let command = RealServerCommand::new(path.display().to_string(), vec![]);

    let session = RealMcpServerSession::spawn(&command).unwrap();
    let pid = session.child.id();

    drop(session);

    let still_running = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .unwrap()
        .success();
    assert!(
        !still_running,
        "pid {pid} is still running after the session was dropped"
    );
}

#[test]
fn a_session_dropped_without_finishing_also_stops_a_grandchild_forked_by_a_wrapper() {
    let temp = tempdir().unwrap();
    let pidfile = temp.path().join("grandchild.pid");
    let path = temp.path().join("wrapper.sh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh
sleep 100 &
echo $! > {pidfile}.tmp
mv {pidfile}.tmp {pidfile}
wait
",
            pidfile = pidfile.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
    let command = RealServerCommand::new(path.display().to_string(), vec![]);

    let session = RealMcpServerSession::spawn(&command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pidfile.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    let grandchild_pid: i32 = fs::read_to_string(&pidfile)
        .expect("wrapper wrote the grandchild pid in time")
        .trim()
        .parse()
        .unwrap();
    assert!(
        crate::agentskills::runner::group::process_is_alive(grandchild_pid),
        "grandchild {grandchild_pid} should be running before the session is dropped"
    );

    drop(session);

    assert!(
        !crate::agentskills::runner::group::process_is_alive(grandchild_pid),
        "grandchild {grandchild_pid} is still running after the session was dropped"
    );
}

#[test]
fn recording_an_error_answer_writes_the_error_field_instead_of_a_body() {
    let temp = tempdir().unwrap();
    let command = fixture_server(temp.path());
    let mut session = RealMcpServerSession::spawn(&command).unwrap();
    session.initialize(Duration::from_secs(5)).unwrap();

    let tool = ToolName::from("fail");
    let answer = session.call_tool(&tool, &json!({}), Duration::from_secs(5)).unwrap();
    assert!(answer.is_error);
    assert_eq!(answer.text, "boom");

    session.finish(Duration::from_secs(5)).unwrap();
}

#[test]
fn a_recorded_mock_round_trips_through_resolve_mock_set() {
    let temp = tempdir().unwrap();
    let command = fixture_server(temp.path());
    let mut session = RealMcpServerSession::spawn(&command).unwrap();
    session.initialize(Duration::from_secs(5)).unwrap();

    let server = ServerName::from("echoserver");
    let tool = ToolName::from("echo");
    let input = json!({"message": "round trip"});
    let answer = session.call_tool(&tool, &input, Duration::from_secs(5)).unwrap();

    let skill = temp.path().join("skill");
    let mocks_dir = skill.join("evals/mocks");
    write_recorded_mock(&mocks_dir, &server, &tool, &input, &answer).unwrap();
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    session.finish(Duration::from_secs(5)).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let declaration = set.tools_for(&server).unwrap().get(&tool).unwrap();
    assert_eq!(declaration.body, "echo: round trip");
    assert_eq!(
        declaration.expect.get(&ExpectPath::from("message")),
        Some(&ExpectConstraint::TypeName {
            type_name: JsonTypeName::String
        })
    );

    let outcome = resolve_call(declaration, &server, &tool, &json!({"message": "anything at all"}));
    match outcome.answer {
        MockCallAnswer::Answered { text, is_error } => {
            assert_eq!(text, "echo: round trip");
            assert!(!is_error);
        }
        other => panic!("expected an answered call: {other:?}"),
    }
}

#[test]
fn a_field_name_that_looks_like_a_yaml_integer_survives_the_expect_key_round_trip() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let mocks_dir = skill.join("evals/mocks");
    let server = ServerName::from("probeserver");
    let tool = ToolName::from("probe");
    // "0755", "+5" and "0x2a" are YAML plain scalars that resolve to an integer, whose
    // canonical decimal spelling differs from the source text; "true"/"on"/"yes"/"null"
    // all round-trip identically already and are included here only as a check that
    // quoting every key does not itself introduce a regression for them. "1.5" is left
    // out of the survivors: it contains a literal `.`, so it collides with `ExpectPath`'s
    // own delimiter the same way a field like "user.name" would, and is covered instead
    // by the dotted-key test below.
    let input = json!({
        "0755": "a",
        "+5": "b",
        "0x2a": "c",
        "true": "d",
        "1.5": "e",
        "on": "f",
        "yes": "g",
        "null": "h",
    });
    let answer = RecordedToolAnswer {
        text: "ok".to_string(),
        is_error: false,
    };
    write_recorded_mock(&mocks_dir, &server, &tool, &input, &answer).unwrap();
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let declaration = set.tools_for(&server).unwrap().get(&tool).unwrap();
    let keys: std::collections::BTreeSet<&str> = declaration.expect.keys().map(ExpectPath::as_str).collect();

    assert!(!keys.contains("1.5"), "keys:{keys:?}");
    for original in ["0755", "+5", "0x2a", "true", "on", "yes", "null"] {
        assert!(
            keys.contains(original),
            "expected key {original:?} to survive the round trip, got {keys:?}"
        );
    }
}

#[test]
fn a_field_name_containing_a_dot_is_skipped_so_the_recorded_call_replays_clean() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let mocks_dir = skill.join("evals/mocks");
    let server = ServerName::from("probeserver");
    let tool = ToolName::from("probe");
    let input = json!({
        "user.name": "alice",
        "repo": {"owner.id": 7, "owner": "acme"},
    });
    let answer = RecordedToolAnswer {
        text: "ok".to_string(),
        is_error: false,
    };
    write_recorded_mock(&mocks_dir, &server, &tool, &input, &answer).unwrap();
    fs::create_dir_all(skill.join("evals/one")).unwrap();

    let set = resolve_mock_set(&skill, &EvalDirName::default(), "one").unwrap();
    let declaration = set.tools_for(&server).unwrap().get(&tool).unwrap();
    let keys: std::collections::BTreeSet<&str> = declaration.expect.keys().map(ExpectPath::as_str).collect();
    assert!(!keys.contains("user.name"), "keys:{keys:?}");
    assert!(!keys.contains("repo.owner.id"), "keys:{keys:?}");
    assert!(keys.contains("repo.owner"), "keys:{keys:?}");

    let violations = declaration.check_expectations(&server, &tool, &input);
    assert!(violations.is_empty(), "violations:{violations:?}");
}

#[test]
fn a_server_request_sharing_our_in_flight_id_is_not_mistaken_for_the_response() {
    let temp = tempdir().unwrap();
    let command = custom_fixture_server(temp.path(), FIXTURE_ID_COLLISION_SERVER);
    let mut session = RealMcpServerSession::spawn(&command).unwrap();
    session.initialize(Duration::from_secs(5)).unwrap();

    let tool = ToolName::from("echo");
    let answer = session
        .call_tool(&tool, &json!({"message": "hi"}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(answer.text, "echo: collided");
    assert!(!answer.is_error);

    session.finish(Duration::from_secs(5)).unwrap();
}

#[test]
fn a_ping_from_the_server_is_answered_and_recording_continues() {
    let temp = tempdir().unwrap();
    let command = custom_fixture_server(temp.path(), FIXTURE_PING_SERVER);
    let mut session = RealMcpServerSession::spawn(&command).unwrap();
    session.initialize(Duration::from_secs(5)).unwrap();

    let tool = ToolName::from("echo");
    let answer = session
        .call_tool(&tool, &json!({"message": "hi"}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(answer.text, "echo: after ping");
    assert!(!answer.is_error);

    session.finish(Duration::from_secs(5)).unwrap();
}

#[test]
fn a_non_json_banner_line_before_the_response_does_not_fail_the_recording() {
    let temp = tempdir().unwrap();
    let command = custom_fixture_server(temp.path(), FIXTURE_BANNER_SERVER);
    let mut session = RealMcpServerSession::spawn(&command).unwrap();
    session.initialize(Duration::from_secs(5)).unwrap();

    let tool = ToolName::from("echo");
    let answer = session
        .call_tool(&tool, &json!({"message": "hi"}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(answer.text, "echo: after banner");
    assert!(!answer.is_error);

    session.finish(Duration::from_secs(5)).unwrap();
}

#[test]
fn finishing_a_session_keeps_draining_the_servers_final_notifications() {
    let child_marker = "TRG_TEST_MCP_FINAL_NOTIFICATIONS_CHILD";
    if std::env::var_os(child_marker).is_none() {
        let test_name = "agentskills::mcp_recording::tests::fixture_server_tests::finishing_a_session_keeps_draining_the_servers_final_notifications";
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(child_marker, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("the isolated final-notification regression exceeded its deadline");
            }
            thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        return;
    }

    let temp = tempdir().unwrap();
    let completed = temp.path().join("notifications-completed");
    let fixture = custom_fixture_server(
        temp.path(),
        r#"#!/bin/sh
set -eu
cat >/dev/null
awk 'BEGIN { for (i = 0; i < 65536; i++) print "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/message\",\"params\":{}}" }'
: > "$1"
"#,
    );
    let command = RealServerCommand::new(fixture.program().to_string(), vec![completed.display().to_string()]);
    let session = RealMcpServerSession::spawn(&command).unwrap();
    session.finish(Duration::from_secs(5)).unwrap();
    assert!(
        completed.exists(),
        "graceful shutdown killed a server blocked on its final stdout notifications"
    );
}
