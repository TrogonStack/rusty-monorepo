//! MCP client spans for a recording session: one per JSON-RPC request `trg`
//! sends the real server, under the CLI span of the server process.

use std::borrow::Cow;
use std::time::Instant;

use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, JSONRPC_REQUEST_ID, NETWORK_TRANSPORT, RPC_RESPONSE_STATUS_CODE,
};
use serde_json::Value;
use tracing::field::Empty;

use super::{RecordedToolAnswer, RecordingError};
use crate::agentskills::grading::telemetry::record_error;
use crate::agentskills::mocks::ToolName;
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, mcp_method_name, GEN_AI_OPERATION_NAME, GEN_AI_TOOL_CALL_ARGUMENTS, GEN_AI_TOOL_CALL_RESULT,
    GEN_AI_TOOL_NAME, MCP_METHOD_NAME, MCP_PROTOCOL_VERSION,
};
use crate::telemetry::semconv::generated::metrics::MCP_CLIENT_OPERATION_DURATION;
use crate::telemetry::ContentCapture;

/// The protocol revision the recording client offers in `initialize`.
pub(super) const PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC over a child's stdin and stdout.
const PIPE: &str = "pipe";

/// `error.type` for a `tools/call` the server answered with `isError: true`.
const TOOL_ERROR: &str = "tool_error";

const DURATION_BUCKETS: [f64; 14] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

impl RecordingError {
    /// The low-cardinality `error.type` a failed request is reported under. A
    /// JSON-RPC error reports its numeric code, as the MCP conventions ask.
    pub(crate) fn error_type(&self) -> Cow<'static, str> {
        match self {
            Self::Spawn { .. } => Cow::Borrowed("spawn_failed"),
            Self::ServerExited { .. } => Cow::Borrowed("server_exited"),
            Self::Timeout { .. } => Cow::Borrowed("timeout"),
            Self::Write { .. } => Cow::Borrowed("write_failed"),
            Self::Wait { .. } => Cow::Borrowed("wait_failed"),
            Self::ServerError { code, .. } => Cow::Owned(code.to_string()),
            Self::Io { .. } => Cow::Borrowed("io"),
            Self::UnrepresentablePlaceholder { .. } => Cow::Borrowed("unrepresentable_placeholder"),
        }
    }

    fn rpc_status_code(&self) -> Option<String> {
        match self {
            Self::ServerError { code, .. } => Some(code.to_string()),
            _ => None,
        }
    }
}

/// What a request span is about: the handshake, or one tool call.
pub(super) enum McpRequest<'a> {
    Initialize,
    ToolCall { tool: &'a ToolName, input: &'a Value },
}

impl McpRequest<'_> {
    fn method(&self) -> &'static str {
        match self {
            Self::Initialize => mcp_method_name::INITIALIZE,
            Self::ToolCall { .. } => mcp_method_name::TOOLS_CALL,
        }
    }

    fn tool(&self) -> Option<&str> {
        match self {
            Self::Initialize => None,
            Self::ToolCall { tool, .. } => Some(tool.as_str()),
        }
    }
}

pub(super) struct McpRequestSpan {
    span: tracing::Span,
    started: Instant,
    method: &'static str,
    tool: Option<String>,
    content: ContentCapture,
}

impl McpRequestSpan {
    pub(super) fn start(parent: &tracing::Span, request: &McpRequest<'_>, id: i64, content: ContentCapture) -> Self {
        let method = request.method();
        let tool = request.tool();
        let name = match tool {
            Some(tool) => format!("{method} {tool}"),
            None => method.to_string(),
        };
        let span = tracing::info_span!(
            parent: parent,
            "mcp request",
            "otel.name" = name.as_str(),
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { MCP_METHOD_NAME } = method,
            { MCP_PROTOCOL_VERSION } = PROTOCOL_VERSION,
            { JSONRPC_REQUEST_ID } = id.to_string().as_str(),
            { NETWORK_TRANSPORT } = PIPE,
            { GEN_AI_OPERATION_NAME } = tool.map(|_| gen_ai_operation_name::EXECUTE_TOOL),
            { GEN_AI_TOOL_NAME } = tool,
            { GEN_AI_TOOL_CALL_ARGUMENTS } = Empty,
            { GEN_AI_TOOL_CALL_RESULT } = Empty,
            { RPC_RESPONSE_STATUS_CODE } = Empty,
            { ERROR_TYPE } = Empty,
        );
        if let (McpRequest::ToolCall { input, .. }, true) = (request, content.includes_span()) {
            span.record(GEN_AI_TOOL_CALL_ARGUMENTS, input.to_string().as_str());
        }
        Self {
            span,
            started: Instant::now(),
            method,
            tool: tool.map(str::to_string),
            content,
        }
    }

    pub(super) fn span(&self) -> &tracing::Span {
        &self.span
    }

    pub(super) fn answered(self, answer: &RecordedToolAnswer) {
        if self.content.includes_span() {
            self.span.record(GEN_AI_TOOL_CALL_RESULT, answer.text.as_str());
        }
        if answer.is_error {
            self.finish(Some(TOOL_ERROR.into()), None);
        } else {
            self.finish(None, None);
        }
    }

    pub(super) fn finish_with<T>(self, result: &Result<T, RecordingError>) {
        match result {
            Ok(_) => self.finish(None, None),
            Err(error) => self.failed(error),
        }
    }

    pub(super) fn failed(self, error: &RecordingError) {
        self.finish(Some(error.error_type()), error.rpc_status_code());
    }

    fn finish(self, error_type: Option<Cow<'static, str>>, status_code: Option<String>) {
        if let Some(code) = &status_code {
            self.span.record(RPC_RESPONSE_STATUS_CODE, code.as_str());
        }
        if let Some(error_type) = &error_type {
            record_error(&self.span, error_type);
        }
        let mut attributes = vec![
            KeyValue::new(MCP_METHOD_NAME, self.method),
            KeyValue::new(MCP_PROTOCOL_VERSION, PROTOCOL_VERSION),
            KeyValue::new(NETWORK_TRANSPORT, PIPE),
        ];
        if let Some(tool) = self.tool {
            attributes.push(KeyValue::new(
                GEN_AI_OPERATION_NAME,
                gen_ai_operation_name::EXECUTE_TOOL,
            ));
            attributes.push(KeyValue::new(GEN_AI_TOOL_NAME, tool));
        }
        if let Some(code) = status_code {
            attributes.push(KeyValue::new(RPC_RESPONSE_STATUS_CODE, code));
        }
        if let Some(error_type) = error_type {
            attributes.push(KeyValue::new(ERROR_TYPE, error_type.into_owned()));
        }
        opentelemetry::global::meter("trg")
            .f64_histogram(MCP_CLIENT_OPERATION_DURATION)
            .with_unit("s")
            .with_boundaries(DURATION_BUCKETS.to_vec())
            .build()
            .record(self.started.elapsed().as_secs_f64(), &attributes);
    }
}
