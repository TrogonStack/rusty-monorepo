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

mod constants;
use constants::TIMEOUT_KILLED;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::grading::telemetry::record_error;
use super::grading::telemetry::CliSpan;
use super::mocks::{ExpectConstraint, ExpectPath, JsonTypeName, ServerName, ToolName};
use super::real_mcp_server::RealServerCommand;
use super::runner::group::{self, ProcessGroupGuard};
use crate::telemetry::ContentCapture;
use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use telemetry::{McpRequest, McpRequestSpan, PROTOCOL_VERSION};
use tracing::field::Empty;

mod telemetry;

#[derive(Serialize)]
struct RpcRequest<P> {
    jsonrpc: &'static str,
    id: i64,
    method: &'static str,
    params: P,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeParams {
    protocol_version: &'static str,
    capabilities: BTreeMap<String, Value>,
    client_info: ClientInfo,
}

#[derive(Serialize)]
struct ClientInfo {
    name: &'static str,
    version: &'static str,
}

#[derive(Serialize)]
struct InitializedNotification {
    jsonrpc: &'static str,
    method: &'static str,
}

#[derive(Serialize)]
struct ToolCallParams<'a> {
    name: &'a str,
    arguments: &'a Value,
}

#[derive(Serialize)]
struct RpcSuccess {
    jsonrpc: &'static str,
    id: Value,
    result: BTreeMap<String, Value>,
}

#[derive(Serialize)]
struct RpcError {
    jsonrpc: &'static str,
    id: Value,
    error: RpcErrorDetails,
}

#[derive(Serialize)]
struct RpcErrorDetails {
    code: i64,
    message: &'static str,
}

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
    #[error(
        "{tool}: the recorded answer contains `{sequence}`, which {expands_via} on replay; rerun once the \
         server stops returning that sequence, or write this mock by hand instead of recording it"
    )]
    UnrepresentablePlaceholder {
        tool: ToolName,
        sequence: &'static str,
        expands_via: &'static str,
    },
}

/// Which literal placeholder prefixes a recorded answer cannot safely contain depends on
/// which surface `resolve_call` puts the text on. `{{input.` is unsafe either way:
/// `substitute_input_placeholders` runs over whichever text answered the call, body or
/// `error:`. `{{file:` is unsafe only in the body: `parse_mock_file` runs
/// `resolve_file_references` over `MockDeclaration.body`, but reads `error:` straight out of
/// the frontmatter and never passes it through that function, so a recorded error is free to
/// contain `{{file:` and still replay verbatim. The mock format has no escape syntax for
/// either, so an answer that contains one on the surface that expands it cannot be written
/// as a `fixed` mock at all.
fn reject_unrepresentable_placeholder(tool: &ToolName, answer: &RecordedToolAnswer) -> Result<(), RecordingError> {
    if !answer.is_error && answer.text.contains("{{file:") {
        return Err(RecordingError::UnrepresentablePlaceholder {
            tool: tool.clone(),
            sequence: "{{file:",
            expands_via: "`resolve_file_references` expands it in the mock body",
        });
    }
    if answer.text.contains("{{input.") {
        return Err(RecordingError::UnrepresentablePlaceholder {
            tool: tool.clone(),
            sequence: "{{input.",
            expands_via: "`substitute_input_placeholders` expands it",
        });
    }
    Ok(())
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
    process: CliSpan,
    content: ContentCapture,
}

impl Drop for RealMcpServerSession {
    fn drop(&mut self) {
        self.group.terminate();
        let _ = self.child.wait();
    }
}

impl RealMcpServerSession {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            unbounded_channel,
            reason = "Stdout must drain independently of synchronous stdin writes and graceful shutdown; blocking this reader can deadlock full-duplex server pipes."
        )
    )]
    pub fn spawn(command: &RealServerCommand) -> Result<Self, RecordingError> {
        let command_label = command.to_string();
        let mut cmd = Command::new(command.program());
        cmd.args(command.args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        group::lead_own_group(&mut cmd);
        let process = CliSpan::start(command.program());
        process.inject(&mut cmd);
        let mut child = cmd.spawn().map_err(|source| {
            process.spawn_failed();
            RecordingError::Spawn {
                command: command_label.clone(),
                source,
            }
        })?;
        process.spawned(child.id());
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
            process,
            content: ContentCapture::from_env(),
        })
    }

    /// The `initialize` / `notifications/initialized` handshake every MCP session opens
    /// with, mirroring the shape `mock_server.rs` answers on the other end of this same
    /// exchange.
    pub fn initialize(&mut self, timeout: Duration) -> Result<(), RecordingError> {
        let id = self.reserve_id();
        let span = McpRequestSpan::start(self.process.span(), &McpRequest::Initialize, id, self.content);
        let result = span.span().clone().in_scope(|| self.handshake(id, timeout));
        span.finish_with(&result);
        result
    }

    fn handshake(&mut self, id: i64, timeout: Duration) -> Result<(), RecordingError> {
        let request = RpcRequest {
            jsonrpc: "2.0",
            id,
            method: "initialize",
            params: InitializeParams {
                protocol_version: PROTOCOL_VERSION,
                capabilities: BTreeMap::new(),
                client_info: ClientInfo {
                    name: "trg-record-mcp",
                    version: env!("CARGO_PKG_VERSION"),
                },
            },
        };
        self.send(&request)?;
        self.await_response(id, "initialize", timeout)?;

        let notification = InitializedNotification {
            jsonrpc: "2.0",
            method: "notifications/initialized",
        };
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
        let span = McpRequestSpan::start(
            self.process.span(),
            &McpRequest::ToolCall { tool, input },
            id,
            self.content,
        );
        let request = RpcRequest {
            jsonrpc: "2.0",
            id,
            method: "tools/call",
            params: ToolCallParams {
                name: tool.as_str(),
                arguments: input,
            },
        };
        let result = span.span().clone().in_scope(|| {
            self.send(&request)?;
            self.await_response(id, "tools/call", timeout)
        });
        match result {
            Ok(result) => {
                let answer = RecordedToolAnswer::from_result(&result);
                span.answered(&answer);
                Ok(answer)
            }
            Err(error) => {
                span.failed(&error);
                Err(error)
            }
        }
    }

    /// Close stdin and wait for the server to exit on its own, killing it if it overstays
    /// `timeout`. Mirrors the poll-and-kill idiom `runner::capture_subprocess` uses for its
    /// one-shot subprocesses, adapted here for a session that has already had its back and
    /// forth rather than a single run to completion.
    pub fn finish(mut self, timeout: Duration) -> Result<(), RecordingError> {
        let span = tracing::info_span!(parent: self.process.span(), "finish", "otel.status_code" = Empty, { ERROR_TYPE } = Empty);
        let _entered = span.enter();
        drop(self.stdin.take());
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.process.exited(&status);
                    self.group.stop_leftovers();
                    return Ok(());
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        self.process.failed(TIMEOUT_KILLED);
                        self.group.terminate();
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(source) => {
                    let error = RecordingError::Wait {
                        command: self.command_label.clone(),
                        source,
                    };
                    record_error(&span, &error.error_type());
                    self.process.failed(&error.error_type());
                    return Err(error);
                }
            }
        }
    }

    fn reserve_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send(&mut self, value: &impl Serialize) -> Result<(), RecordingError> {
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
        if server_method == "ping" {
            self.send(&RpcSuccess {
                jsonrpc: "2.0",
                id: request_id,
                result: BTreeMap::new(),
            })
        } else {
            self.send(&RpcError {
                jsonrpc: "2.0",
                id: request_id,
                error: RpcErrorDetails {
                    code: -32601,
                    message: "method not found",
                },
            })
        }
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
/// Object values get an `object` constraint of their own and are also flattened into dotted
/// paths so each nested field gets its own constraint too, the same way a hand-written
/// `expect: repo: object` plus `expect: repo.owner: string` would; arrays are left as leaf
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
            Value::Object(nested) => {
                insert_type(out, path.clone(), JsonTypeName::Object);
                collect_expect(nested, &path, out);
            }
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
    reject_unrepresentable_placeholder(tool, answer)?;
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
mod tests;
