use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use clap::Args;
use serde::Serialize;
use serde_json::Value;

use crate::agentskills::exit_code::ExitCode;
use crate::agentskills::mcp_recording::{write_recorded_mock, RealMcpServerSession};
use crate::agentskills::mocks::{ServerName, ToolName};
use crate::agentskills::real_mcp_server::{admit_real_server, RealServerCommand, RealServerTrust};
use crate::fs::FileSystem;
use crate::output::OutputFormat;

use super::print_json;

/// One `--call TOOL=JSON` flag: the tool to call, and the call's input as a JSON object.
#[derive(Debug, Clone)]
pub struct RecordedCallSpec {
    pub tool: ToolName,
    pub input: Value,
}

impl FromStr for RecordedCallSpec {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let (tool, json) = raw.split_once('=').ok_or_else(|| {
            format!("`{raw}` is not TOOL=JSON: expected a tool name, then `=`, then a JSON call input")
        })?;
        if tool.is_empty() {
            return Err(format!("`{raw}` has no tool name before `=`"));
        }
        let input: Value =
            serde_json::from_str(json).map_err(|e| format!("`{raw}`: call input is not valid JSON: {e}"))?;
        Ok(Self {
            tool: ToolName::from(tool),
            input,
        })
    }
}

#[derive(Args, Debug)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval record-mcp \\
      --server github --command ./mcp-servers/github --arg --stdio \\
      --call 'create_issue={\"repo\":\"acme/widgets\",\"title\":\"hi\"}' \\
      --mocks-dir ./my-skill/evals/mocks \\
      --allow-real-mcp-server

  $ trg ai skills eval record-mcp --server github --command ./mcp-servers/github \\
      --call 'list_issues={}' --call 'close_issue={\"id\":1}' \\
      --mocks-dir ./my-skill/evals/mocks --allow-real-mcp-server
")]
pub struct RecordMcpArgs {
    /// The server name a suite's mocks will be resolved under, e.g. `github`.
    #[arg(long, value_name = "NAME")]
    pub server: String,

    /// The real MCP server to start, e.g. an installed binary or a wrapper script.
    #[arg(long, value_name = "PROGRAM")]
    pub command: String,

    /// One argument to pass the command; repeat for each. Order is preserved. Accepts a
    /// value that itself looks like a flag, e.g. `--arg --stdio`.
    #[arg(long = "arg", value_name = "ARG", allow_hyphen_values = true)]
    pub args: Vec<String>,

    /// One tool call to make and record, as `TOOL=JSON`; repeat for each tool a suite needs
    /// a mock for.
    #[arg(long = "call", value_name = "TOOL=JSON", required = true)]
    pub calls: Vec<RecordedCallSpec>,

    /// Directory to write `<server>/<tool>.md` mock files into, e.g. a suite's
    /// `evals/mocks`.
    #[arg(long, value_name = "DIR")]
    pub mocks_dir: PathBuf,

    /// Start the declared command as a real MCP server and record its answers. It runs as
    /// you, outside anything a run confines, so recording refuses to start it until this is
    /// passed.
    #[arg(long)]
    pub allow_real_mcp_server: bool,

    /// How long to wait for the server to answer a single request, in seconds.
    #[arg(long, value_name = "SECS", default_value_t = 10)]
    pub timeout_secs: u64,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

/// Rejects an empty `--server` or `--command` before anything is spawned. Both are plain
/// `String` flags rather than a value object, so nothing else in the type system stops an
/// empty one from reaching `ServerName::from` (which is infallible and would silently write
/// a mock one directory above where `eval run` looks for it) or `Command::new` (which would
/// fail far less clearly than naming the flag here does).
fn validate_flags(server: &str, command: &str) -> Result<(), String> {
    if server.is_empty() {
        return Err("--server must not be empty".to_string());
    }
    if command.is_empty() {
        return Err("--command must not be empty".to_string());
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct RecordedCallSummary {
    tool: String,
    path: String,
    is_error: bool,
}

#[derive(Debug, Serialize)]
struct RecordMcpOutput {
    server: String,
    command: String,
    mocks_dir: String,
    recorded: Vec<RecordedCallSummary>,
}

impl RecordMcpArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        if let Err(e) = validate_flags(&self.server, &self.command) {
            eprintln!("record-mcp: {e}");
            return ExitCode::InfrastructureFailure;
        }

        let command = RealServerCommand::new(self.command.clone(), self.args.clone());
        let trust = RealServerTrust::granted(self.allow_real_mcp_server);
        if let Err(e) = admit_real_server(&command, trust) {
            eprintln!("{e}");
            return ExitCode::GateFailed;
        }

        let timeout = Duration::from_secs(self.timeout_secs);
        let mut session = match RealMcpServerSession::spawn(&command) {
            Ok(session) => session,
            Err(e) => {
                eprintln!("record-mcp: {e}");
                return ExitCode::InfrastructureFailure;
            }
        };

        if let Err(e) = session.initialize(timeout) {
            eprintln!("record-mcp: {e}");
            return ExitCode::InfrastructureFailure;
        }

        let server = ServerName::from(self.server.as_str());
        let mut recorded = Vec::with_capacity(self.calls.len());
        for call in &self.calls {
            let answer = match session.call_tool(&call.tool, &call.input, timeout) {
                Ok(answer) => answer,
                Err(e) => {
                    eprintln!("record-mcp: {e}");
                    return ExitCode::InfrastructureFailure;
                }
            };
            let path = match write_recorded_mock(&self.mocks_dir, &server, &call.tool, &call.input, &answer) {
                Ok(path) => path,
                Err(e) => {
                    eprintln!("record-mcp: {e}");
                    return ExitCode::InfrastructureFailure;
                }
            };
            recorded.push(RecordedCallSummary {
                tool: call.tool.as_str().to_string(),
                path: path.display().to_string(),
                is_error: answer.is_error,
            });
        }

        if let Err(e) = session.finish(timeout) {
            eprintln!("record-mcp: {e}");
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            return print_json(
                &RecordMcpOutput {
                    server: self.server,
                    command: command.to_string(),
                    mocks_dir: self.mocks_dir.display().to_string(),
                    recorded,
                },
                ExitCode::Success,
            );
        }

        println!("Recorded {} against {command}:", self.server);
        for call in &recorded {
            let marker = if call.is_error { "error" } else { "ok" };
            println!("  {} [{marker}] -> {}", call.tool, call.path);
        }
        ExitCode::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_spec_parses_the_tool_name_and_json_input() {
        let spec = RecordedCallSpec::from_str(r#"create_issue={"repo":"acme/widgets"}"#).unwrap();

        assert_eq!(spec.tool.as_str(), "create_issue");
        assert_eq!(spec.input, serde_json::json!({"repo": "acme/widgets"}));
    }

    #[test]
    fn a_call_spec_without_an_equals_sign_is_rejected_by_name() {
        let err = RecordedCallSpec::from_str("create_issue").unwrap_err();

        assert!(err.contains("TOOL=JSON"), "error should name the expected shape: {err}");
    }

    #[test]
    fn a_call_spec_with_invalid_json_is_rejected() {
        let err = RecordedCallSpec::from_str("create_issue=not json").unwrap_err();

        assert!(err.contains("not valid JSON"), "error: {err}");
    }

    #[test]
    fn an_empty_server_name_is_rejected_by_name() {
        let err = validate_flags("", "./mcp-servers/github").unwrap_err();

        assert!(err.contains("--server"), "error should name the empty flag: {err}");
    }

    #[test]
    fn an_empty_command_is_rejected_by_name() {
        let err = validate_flags("github", "").unwrap_err();

        assert!(err.contains("--command"), "error should name the empty flag: {err}");
    }
}
