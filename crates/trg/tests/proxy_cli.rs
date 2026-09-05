//! What `trg mcp proxy` puts on stdout when it cannot start.
//!
//! Stdout is the MCP host's only channel: an editor spawns the proxy with both
//! its pipes bound and shows nothing of stderr, so a failure that only prints
//! there reaches the person as an MCP server that exited. These pin the
//! failure onto the protocol instead.
//!
//! The server points at a closed loopback port, which fails discovery without
//! reaching the network and without depending on any credential store.

use std::fs;

use assert_cmd::Command;

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#;

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

fn proxy(config_home: &std::path::Path, stdin: &str) -> std::process::Output {
    let mut cmd = Command::cargo_bin("trg").unwrap();
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.args(["mcp", "proxy", "--server", "nowhere"]);
    cmd.write_stdin(stdin);
    cmd.output().unwrap()
}

#[test]
fn a_proxy_that_cannot_start_answers_the_request_rather_than_only_exiting() {
    let home = config_home_pointing_at_a_closed_port();
    let out = proxy(home.path(), &format!("{INITIALIZE}\n"));

    let stdout = String::from_utf8(out.stdout).unwrap();
    let line = stdout.lines().next().unwrap_or_else(|| panic!("nothing on stdout"));
    let parsed: serde_json::Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("stdout was not JSON-RPC: {e}\n{line}"));

    assert_eq!(parsed["jsonrpc"], "2.0", "{line}");
    assert_eq!(parsed["id"], 1, "{line}");
    assert!(
        parsed["error"]["message"].as_str().is_some_and(|m| !m.is_empty()),
        "the refusal has to carry a reason: {line}"
    );

    // Answering the host is not the same as having worked.
    assert_eq!(out.status.code(), Some(1), "{stdout}");
}

/// A notification carries no id, so there is nothing to answer and a reply
/// would be a response to a request the host never made.
#[test]
fn a_notification_is_not_answered() {
    let home = config_home_pointing_at_a_closed_port();
    let out = proxy(
        home.path(),
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
    );

    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.trim().is_empty(), "{stdout}");
    assert_eq!(out.status.code(), Some(1));
}
