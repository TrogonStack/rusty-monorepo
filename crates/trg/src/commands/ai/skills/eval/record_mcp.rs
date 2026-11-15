mod constants;
use constants::REFUSED;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use clap::Args;
use serde::Serialize;
use serde_json::Value;

use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use tracing::field::Empty;

use crate::agentskills::grading::telemetry::record_error;
use crate::agentskills::mcp_recording::{write_recorded_mock, RealMcpServerSession};
use crate::agentskills::mocks::{ServerName, ToolName};
use crate::agentskills::real_mcp_server::{admit_real_server, RealServerCommand, RealServerTrust};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use crate::telemetry::semconv::generated::attributes::GEN_AI_TOOL_NAME;

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
        if !input.is_object() {
            return Err(format!(
                "`{raw}`: call input must be a JSON object, since it becomes `tools/call`'s `arguments`"
            ));
        }
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

/// Rejects an empty `--server` or `--command`, a `--server` or `--call` tool name that is
/// not a single path component, or a repeated `--call` tool name, before anything is
/// spawned. `--server`, `--command` and a call's tool name are plain `String`/`ToolName`
/// values rather than a path-shaped value object, so nothing else in the type system stops
/// an empty, absolute, or `..`-carrying one from reaching `ServerName::from` /
/// `ToolName::from` (both infallible) and then `mocks_dir.join(server).join(format!("{tool}
/// .md"))`: `Path::join` with an absolute argument replaces the whole path instead of
/// extending it, and `..` walks back out of it, so either can write a mock file outside the
/// documented `<mocks-dir>/<server>/<tool>.md` layout. A repeated tool name would instead
/// have the second call's mock file silently overwrite the first's, while the summary still
/// reports both as recorded.
fn validate_flags(server: &str, command: &str, calls: &[RecordedCallSpec]) -> Result<(), String> {
    if server.is_empty() {
        return Err("--server must not be empty".to_string());
    }
    if command.is_empty() {
        return Err("--command must not be empty".to_string());
    }
    validate_path_component("--server", server)?;

    let mut seen_tools = std::collections::BTreeSet::new();
    for call in calls {
        let tool = call.tool.as_str();
        validate_path_component("--call tool name", tool)?;
        if !seen_tools.insert(tool) {
            return Err(format!(
                "--call tool `{tool}` is repeated: each tool can only be recorded once per invocation, since its \
                 mock file would otherwise be overwritten"
            ));
        }
    }
    Ok(())
}

fn validate_path_component(flag: &str, value: &str) -> Result<(), String> {
    if Path::new(value).is_absolute() || value.contains('/') || value == "." || value == ".." {
        return Err(format!(
            "{flag} `{value}` must be a single path component, not a path: it becomes part of \
             <mocks-dir>/<server>/<tool>.md"
        ));
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
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        if let Err(e) = validate_flags(&self.server, &self.command, &self.calls) {
            eprintln!("record-mcp: {e}");
            return ExitCode::InfrastructureFailure;
        }

        let command = RealServerCommand::new(self.command.clone(), self.args.clone());
        let trust = RealServerTrust::granted(self.allow_real_mcp_server);
        let admit = tracing::info_span!("admit real server", "otel.status_code" = Empty, { ERROR_TYPE } = Empty);
        if let Err(e) = admit.in_scope(|| admit_real_server(&command, trust)) {
            record_error(&admit, REFUSED);
            eprintln!("{e}");
            return ExitCode::GateFailed;
        }
        drop(admit);

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
            let write = tracing::info_span!(
                "write mock",
                "otel.status_code" = Empty,
                { GEN_AI_TOOL_NAME } = call.tool.as_str(),
                { ERROR_TYPE } = Empty,
            );
            let written =
                write.in_scope(|| write_recorded_mock(&self.mocks_dir, &server, &call.tool, &call.input, &answer));
            let path = match written {
                Ok(path) => path,
                Err(e) => {
                    record_error(&write, &e.error_type());
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
#[path = "record_mcp_tests.rs"]
mod tests;
