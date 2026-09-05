//! What `trg mcp proxy` puts on stdout when it cannot start.
//!
//! Stdout is the MCP host's only channel: an editor spawns the proxy with both
//! its pipes bound and shows nothing of stderr, so a failure that only prints
//! there reaches the person as an MCP server that exited. These pin the
//! failure onto the protocol instead.
//!
//! Both directions go through rmcp's own message types rather than hand-written
//! JSON. A test that spells the wire format itself can assert a shape neither
//! side speaks, and the proxy is a pipe between two implementations of exactly
//! these types.
//!
//! The server points at a closed loopback port, which fails discovery without
//! reaching the network and without depending on any credential store.

use std::fs;

use assert_cmd::Command;
use rmcp::model::{
    ClientCapabilities, ClientJsonRpcMessage, ClientNotification, ClientRequest, Implementation, InitializeRequest,
    InitializeRequestParams, InitializedNotification, JsonRpcMessage, ProtocolVersion, RequestId, ServerJsonRpcMessage,
};

fn initialize(id: RequestId) -> ClientJsonRpcMessage {
    let request = InitializeRequest::new(
        InitializeRequestParams::new(ClientCapabilities::default(), Implementation::new("probe", "0"))
            .with_protocol_version(ProtocolVersion::LATEST),
    );
    JsonRpcMessage::request(ClientRequest::InitializeRequest(request), id)
}

fn initialized() -> ClientJsonRpcMessage {
    JsonRpcMessage::notification(ClientNotification::InitializedNotification(
        InitializedNotification::default(),
    ))
}

fn config_home_pointing_at_a_closed_port() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("trg")).unwrap();
    fs::write(
        dir.path().join("trg/config.toml"),
        "[mcp.servers.nowhere]\nurl = \"http://127.0.0.1:1/mcp\"\n",
    )
    .unwrap();
    dir
}

fn proxy(config_home: &std::path::Path, sent: &[ClientJsonRpcMessage]) -> std::process::Output {
    let stdin = sent
        .iter()
        .map(|msg| serde_json::to_string(msg).expect("an rmcp message serializes") + "\n")
        .collect::<String>();

    let mut cmd = Command::cargo_bin("trg").unwrap();
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.args(["mcp", "proxy", "--server", "nowhere"]);
    cmd.write_stdin(stdin);
    cmd.output().unwrap()
}

fn received(stdout: &[u8]) -> Vec<ServerJsonRpcMessage> {
    let text = String::from_utf8(stdout.to_vec()).expect("stdout is utf-8");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("stdout was not an MCP message: {e}\n{line}")))
        .collect()
}

#[test]
fn a_proxy_that_cannot_start_answers_the_request_rather_than_only_exiting() {
    let home = config_home_pointing_at_a_closed_port();
    let id = RequestId::Number(1);
    let out = proxy(home.path(), &[initialize(id.clone())]);

    let messages = received(&out.stdout);
    let [JsonRpcMessage::Error(refusal)] = messages.as_slice() else {
        panic!("expected one error and nothing else, got {messages:#?}");
    };

    assert_eq!(refusal.id, Some(id), "the answer has to name the request it answers");
    assert!(
        !refusal.error.message.is_empty(),
        "the refusal has to carry a reason: {refusal:#?}"
    );

    // Answering the host is not the same as having worked.
    assert_eq!(out.status.code(), Some(1));
}

/// A notification carries no id, so there is nothing to answer and a reply
/// would be a response to a request the host never made.
#[test]
fn a_notification_is_not_answered() {
    let home = config_home_pointing_at_a_closed_port();
    let out = proxy(home.path(), &[initialized()]);

    let messages = received(&out.stdout);
    assert!(messages.is_empty(), "{messages:#?}");
    assert_eq!(out.status.code(), Some(1));
}

/// One failure, one answer each: a host that pipelines must not be left with a
/// request the proxy quietly dropped.
#[test]
fn every_request_is_answered_and_not_only_the_first() {
    let home = config_home_pointing_at_a_closed_port();
    let first = RequestId::Number(1);
    let second = RequestId::String("second".into());
    let out = proxy(
        home.path(),
        &[initialize(first.clone()), initialized(), initialize(second.clone())],
    );

    let ids: Vec<Option<RequestId>> = received(&out.stdout)
        .into_iter()
        .map(|msg| match msg {
            JsonRpcMessage::Error(refusal) => refusal.id,
            other => panic!("expected an error, got {other:#?}"),
        })
        .collect();

    assert_eq!(ids, vec![Some(first), Some(second)]);
}
