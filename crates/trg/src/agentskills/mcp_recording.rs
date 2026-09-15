//! Turning "I do not know what this server answers" into a checked-in `fixed` mock.
//!
//! This starts the declared real server under `real_mcp_server`'s trust gate, speaks the
//! same JSON-RPC-over-stdio handshake `commands/ai/skills/eval/mock_server.rs` answers on
//! the other end, and writes each tool's answer out as an ordinary mock file: the same
//! `type: fixed` shape, with the same `gray_matter` YAML frontmatter, that `mocks::
//! parse_mock_file` already reads. Nothing here adds a field the parser does not know, and
//! nothing here is a new `MockType`; a recording is indistinguishable, once written, from a
//! mock an author typed by hand.
//!
//! The transport is hand-rolled rather than built on `rmcp`, for the same reason
//! `mock_server.rs` hand-rolls its side of the same handshake: `rmcp` is in this workspace
//! only with its client-role, HTTP/stdio-bridge features enabled, not a child-process
//! transport, and bending it into one would cost more than three JSON-RPC methods are worth.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::mocks::{ExpectConstraint, ExpectPath, JsonTypeName, ServerName, ToolName};
use super::real_mcp_server::RealServerCommand;
use super::runner::group::{self, ProcessGroupGuard};

#[derive(Debug, thiserror::Error)]
pub enum RecordingError {
    #[error("could not start '{command}': {source}")]
    Spawn { command: String, source: io::Error },
    #[error("'{command}' exited before answering {method}")]
    ServerExited { command: String, method: String },
    #[error("'{command}' did not answer {method} within {timeout_secs}s")]
    Timeout {
        command: String,
        method: String,
        timeout_secs: u64,
    },
    #[error("writing to '{command}' failed: {source}")]
    Write { command: String, source: io::Error },
    #[error("waiting for '{command}' to exit failed: {source}")]
    Wait { command: String, source: io::Error },
    #[error("'{command}' answered {method} with a JSON-RPC error {code}: {message}")]
    ServerError {
        command: String,
        method: String,
        code: i64,
        message: String,
    },
    #[error("could not write mock file at {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// A live JSON-RPC-over-stdio session with a real MCP server, opened only after
/// `real_mcp_server::admit_real_server` has let the command through.
///
/// This holds a process that was admitted specifically because it runs as the operator,
/// outside anything a run confines. Many real MCP servers are started through a wrapper
/// (`npx`, `uvx`, a shell script) that forks the actual server as a grandchild, so the
/// session leads its own process group and signals the whole group rather than just the
/// wrapper's pid; `Drop` is the backstop that tears the group down on every path that ends
/// the session without a clean `finish`, so a failed or timed-out recording cannot leave
/// that process, or anything it forked, running behind `trg`.
pub struct RealMcpServerSession {
    child: Child,
    group: ProcessGroupGuard,
    stdin: Option<ChildStdin>,
    responses: mpsc::Receiver<String>,
    command_label: String,
    next_id: i64,
}

impl Drop for RealMcpServerSession {
    fn drop(&mut self) {
        self.group.terminate();
        let _ = self.child.wait();
    }
}

impl RealMcpServerSession {
    pub fn spawn(command: &RealServerCommand) -> Result<Self, RecordingError> {
        let command_label = command.to_string();
        let mut cmd = Command::new(command.program());
        cmd.args(command.args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        group::lead_own_group(&mut cmd);
        let mut child = cmd.spawn().map_err(|source| RecordingError::Spawn {
            command: command_label.clone(),
            source,
        })?;
        let group = ProcessGroupGuard::led_by(&child);

        let stdin = child.stdin.take().expect("spawned with a piped stdin");
        let stdout = child.stdout.take().expect("spawned with a piped stdout");
        let stderr = child.stderr.take().expect("spawned with a piped stderr");

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr_label = command_label.clone();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("record-mcp: {stderr_label}: {line}");
            }
        });

        Ok(Self {
            child,
            group,
            stdin: Some(stdin),
            responses: rx,
            command_label,
            next_id: 1,
        })
    }

    /// The `initialize` / `notifications/initialized` handshake every MCP session opens
    /// with, mirroring the shape `mock_server.rs` answers on the other end of this same
    /// exchange.
    pub fn initialize(&mut self, timeout: Duration) -> Result<(), RecordingError> {
        let id = self.reserve_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "trg-record-mcp", "version": env!("CARGO_PKG_VERSION")},
            },
        });
        self.send(&request)?;
        self.await_response(id, "initialize", timeout)?;

        let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        self.send(&notification)?;
        Ok(())
    }

    pub fn call_tool(
        &mut self,
        tool: &ToolName,
        input: &Value,
        timeout: Duration,
    ) -> Result<RecordedToolAnswer, RecordingError> {
        let id = self.reserve_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool.as_str(), "arguments": input},
        });
        self.send(&request)?;
        let result = self.await_response(id, "tools/call", timeout)?;
        Ok(RecordedToolAnswer::from_result(&result))
    }

    /// Close stdin and wait for the server to exit on its own, killing it if it overstays
    /// `timeout`. Mirrors the poll-and-kill idiom `runner::capture_subprocess` uses for its
    /// one-shot subprocesses, adapted here for a session that has already had its back and
    /// forth rather than a single run to completion.
    pub fn finish(mut self, timeout: Duration) -> Result<(), RecordingError> {
        drop(self.stdin.take());
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.group.stop_leftovers();
                    return Ok(());
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        self.group.terminate();
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(source) => {
                    return Err(RecordingError::Wait {
                        command: self.command_label.clone(),
                        source,
                    })
                }
            }
        }
    }

    fn reserve_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send(&mut self, value: &Value) -> Result<(), RecordingError> {
        let mut line = serde_json::to_string(value).expect("a json-rpc envelope always serializes");
        line.push('\n');
        // `stdin` is only ever taken by `finish`, which consumes the session; nothing after
        // that can reach `send` again.
        let stdin = self.stdin.as_mut().expect("session stdin taken before finish");
        stdin
            .write_all(line.as_bytes())
            .map_err(|source| RecordingError::Write {
                command: self.command_label.clone(),
                source,
            })
    }

    /// Answer a request the server sent us mid-session, so it is never left waiting on one:
    /// `ping` is the one method every MCP client is expected to answer regardless of the
    /// capabilities it declared, and anything else is refused with the standard JSON-RPC
    /// "method not found" so the server's own wait ends rather than hangs.
    fn answer_server_request(&mut self, server_method: &str, request_id: Value) -> Result<(), RecordingError> {
        let response = if server_method == "ping" {
            json!({"jsonrpc": "2.0", "id": request_id, "result": {}})
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": "method not found"},
            })
        };
        self.send(&response)
    }

    fn await_response(&mut self, id: i64, method: &str, timeout: Duration) -> Result<Value, RecordingError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(RecordingError::Timeout {
                    command: self.command_label.clone(),
                    method: method.to_string(),
                    timeout_secs: timeout.as_secs(),
                });
            }
            let line = match self.responses.recv_timeout(remaining) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(RecordingError::Timeout {
                        command: self.command_label.clone(),
                        method: method.to_string(),
                        timeout_secs: timeout.as_secs(),
                    })
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(RecordingError::ServerExited {
                        command: self.command_label.clone(),
                        method: method.to_string(),
                    })
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(_) => continue,
            };
            // The client and server id spaces are independent: a server-to-client request
            // can carry the same id we happen to be waiting on, so a message is a response
            // to it only by having no `method`, never by id alone.
            if let Some(server_method) = value.get("method").and_then(Value::as_str) {
                if let Some(request_id) = value.get("id") {
                    self.answer_server_request(server_method, request_id.clone())?;
                }
                continue;
            }
            if value.get("id").and_then(Value::as_i64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(RecordingError::ServerError {
                    command: self.command_label.clone(),
                    method: method.to_string(),
                    code: error.get("code").and_then(Value::as_i64).unwrap_or_default(),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                });
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

/// One `tools/call` result, reduced to what a `fixed` mock declaration can hold: the text a
/// skill would read out of `content[*].text`, and whether the server flagged it as an error.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedToolAnswer {
    pub text: String,
    pub is_error: bool,
}

impl RecordedToolAnswer {
    fn from_result(result: &Value) -> Self {
        let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
        let text_items: Vec<&str> = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect();
        let text = if text_items.is_empty() {
            serde_json::to_string(result).unwrap_or_default()
        } else {
            text_items.join("\n")
        };
        Self { text, is_error }
    }
}

/// Every `expect` constraint `derive_expect` can write, from a call's own input: the shape
/// of each leaf, not its value. A recorded call is a sample of one, so pinning the exact
/// value it happened to carry would fail the very next call that varies it; pinning the
/// JSON type it must still hold is the constraint a single recording can honestly support.
///
/// Object values are flattened into dotted paths so a nested field gets its own constraint,
/// the same way a hand-written `expect: repo.owner: string` would; arrays are left as leaf
/// `array` constraints rather than descended into, since an element's own path has no stable
/// name to hang a constraint on. A `null` leaf is skipped rather than constrained: nothing in
/// `ExpectConstraint` represents "was null", and emitting a type constraint for it would fail
/// the very next call whose value at that path legitimately is `null`.
///
/// A key containing a `.`, at any nesting depth, is skipped rather than flattened: `ExpectPath`
/// has no escaping, so the dotted path a literal `.` in a key would produce is indistinguishable
/// from the nested-object path the same characters would build, and `lookup_path` would resolve
/// it as the latter. Skipping the field leaves it unconstrained rather than writing a constraint
/// that can never match its own recording.
pub fn derive_expect(input: &Value) -> BTreeMap<ExpectPath, ExpectConstraint> {
    let mut expect = BTreeMap::new();
    if let Value::Object(map) = input {
        collect_expect(map, "", &mut expect);
    }
    expect
}

fn collect_expect(
    map: &serde_json::Map<String, Value>,
    prefix: &str,
    out: &mut BTreeMap<ExpectPath, ExpectConstraint>,
) {
    for (key, value) in map {
        if key.contains('.') {
            continue;
        }
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            Value::Null => {}
            Value::Object(nested) => collect_expect(nested, &path, out),
            Value::String(_) => insert_type(out, path, JsonTypeName::String),
            Value::Number(_) => insert_type(out, path, JsonTypeName::Number),
            Value::Bool(_) => insert_type(out, path, JsonTypeName::Boolean),
            Value::Array(_) => insert_type(out, path, JsonTypeName::Array),
        }
    }
}

fn insert_type(out: &mut BTreeMap<ExpectPath, ExpectConstraint>, path: String, type_name: JsonTypeName) {
    out.insert(
        ExpectPath::from(path.as_str()),
        ExpectConstraint::TypeName { type_name },
    );
}

fn type_name_label(type_name: JsonTypeName) -> &'static str {
    match type_name {
        JsonTypeName::String => "string",
        JsonTypeName::Number => "number",
        JsonTypeName::Boolean => "boolean",
        JsonTypeName::Object => "object",
        JsonTypeName::Array => "array",
    }
}

/// Write one recorded call as a `<mocks-dir>/<server>/<tool>.md` mock file, in the same
/// frontmatter-plus-body shape `parse_mock_file` reads for a hand-written one.
///
/// The frontmatter is built directly rather than through a YAML serializer (none is in this
/// workspace; `gray_matter` only parses YAML, it does not write it): every `expect` value it
/// carries is one of the five fixed `JsonTypeName` labels, which need no quoting; every
/// `expect` key and the `error` string are field names and text taken verbatim from the
/// server's own answer, so each is rendered as a double-quoted scalar using `serde_json`'s
/// escaping, which YAML's double-quoted style reads the same way JSON does. A key is never
/// left bare: a field named, say, `0755` would otherwise round-trip through the YAML parser
/// as the integer `755`, silently renaming it.
pub fn write_recorded_mock(
    mocks_dir: &Path,
    server: &ServerName,
    tool: &ToolName,
    input: &Value,
    answer: &RecordedToolAnswer,
) -> Result<PathBuf, RecordingError> {
    let expect = derive_expect(input);
    let rendered = render_fixed_mock(&expect, answer);

    let dir = mocks_dir.join(server.as_str());
    fs::create_dir_all(&dir).map_err(|source| RecordingError::Io {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join(format!("{}.md", tool.as_str()));
    fs::write(&path, rendered).map_err(|source| RecordingError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

fn render_fixed_mock(expect: &BTreeMap<ExpectPath, ExpectConstraint>, answer: &RecordedToolAnswer) -> String {
    let mut out = String::from("---\ntype: fixed\n");
    if !expect.is_empty() {
        out.push_str("expect:\n");
        for (path, constraint) in expect {
            let label = match constraint {
                ExpectConstraint::TypeName { type_name } => type_name_label(*type_name),
                _ => "string",
            };
            out.push_str("  ");
            out.push_str(&yaml_quoted(path.as_str()));
            out.push_str(": ");
            out.push_str(label);
            out.push('\n');
        }
    }
    if answer.is_error {
        out.push_str("error: ");
        out.push_str(&yaml_quoted(&answer.text));
        out.push('\n');
    }
    out.push_str("---\n");
    if !answer.is_error {
        out.push_str(&answer.text);
    }
    out
}

fn yaml_quoted(raw: &str) -> String {
    serde_json::to_string(raw).expect("a string always serializes to a JSON string")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_number_boolean_and_array_leaf_each_get_their_own_type_constraint() {
        let input = json!({
            "title": "hello",
            "count": 3,
            "urgent": true,
            "labels": ["a", "b"],
            "ignored": null,
        });

        let expect = derive_expect(&input);

        assert_eq!(
            expect.get(&ExpectPath::from("title")),
            Some(&ExpectConstraint::TypeName {
                type_name: JsonTypeName::String
            })
        );
        assert_eq!(
            expect.get(&ExpectPath::from("count")),
            Some(&ExpectConstraint::TypeName {
                type_name: JsonTypeName::Number
            })
        );
        assert_eq!(
            expect.get(&ExpectPath::from("urgent")),
            Some(&ExpectConstraint::TypeName {
                type_name: JsonTypeName::Boolean
            })
        );
        assert_eq!(
            expect.get(&ExpectPath::from("labels")),
            Some(&ExpectConstraint::TypeName {
                type_name: JsonTypeName::Array
            })
        );
        assert_eq!(expect.get(&ExpectPath::from("ignored")), None);
        assert_eq!(expect.len(), 4);
    }

    #[test]
    fn a_nested_object_is_flattened_into_a_dotted_path() {
        let input = json!({"repo": {"owner": "acme"}});

        let expect = derive_expect(&input);

        assert_eq!(
            expect.get(&ExpectPath::from("repo.owner")),
            Some(&ExpectConstraint::TypeName {
                type_name: JsonTypeName::String
            })
        );
        assert_eq!(expect.len(), 1);
    }

    #[test]
    fn a_key_that_is_not_a_bare_scalar_is_rendered_double_quoted() {
        let mut expect = BTreeMap::new();
        expect.insert(
            ExpectPath::from("weird key: with colon"),
            ExpectConstraint::TypeName {
                type_name: JsonTypeName::String,
            },
        );
        let answer = RecordedToolAnswer {
            text: "ok".to_string(),
            is_error: false,
        };

        let rendered = render_fixed_mock(&expect, &answer);

        assert!(
            rendered.contains("\"weird key: with colon\": string"),
            "rendered:\n{rendered}"
        );
    }

    #[test]
    fn an_error_answer_is_written_as_the_error_field_with_an_empty_body() {
        let answer = RecordedToolAnswer {
            text: "rate limited".to_string(),
            is_error: true,
        };

        let rendered = render_fixed_mock(&BTreeMap::new(), &answer);

        assert!(rendered.contains("error: \"rate limited\""), "rendered:\n{rendered}");
        assert!(rendered.ends_with("---\n"), "an error mock has no body: {rendered:?}");
    }

    #[test]
    fn a_result_with_no_text_content_falls_back_to_the_whole_result_as_json() {
        let result = json!({"structuredContent": {"ok": true}});

        let answer = RecordedToolAnswer::from_result(&result);

        assert_eq!(answer.text, serde_json::to_string(&result).unwrap());
        assert!(!answer.is_error);
    }

    #[test]
    fn a_result_with_text_content_uses_the_text_verbatim() {
        let result = json!({"content": [{"type": "text", "text": "issue created"}], "isError": false});

        let answer = RecordedToolAnswer::from_result(&result);

        assert_eq!(answer.text, "issue created");
    }
}

/// A fake MCP server, spoken to over real stdio, so recording is exercised against an
/// actual child process without depending on the network or on any server the machine
/// happens to have installed.
#[cfg(test)]
mod fixture_server_tests {
    use super::*;
    use crate::agentskills::evals::EvalDirName;
    use crate::agentskills::mocks::{resolve_call, resolve_mock_set, MockCallAnswer};
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
echo $! > {}
wait
",
                pidfile.display()
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
}
