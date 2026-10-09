//! Spans rebuilt from a harness's stamped stdout, for a harness that exported none.
//!
//! Every harness narrates its model turns and tool calls on stdout. Read with the
//! time each line arrived, that narration is enough to give the `invoke_agent` span
//! the `chat` and `execute_tool` children the harness itself would have emitted,
//! timed when the harness reported each step rather than when `trg` got around to
//! reading it. Only `trg`'s own stamps are used: a harness's own clock is never
//! trusted to agree with this machine's.

use std::collections::BTreeMap;
use std::time::SystemTime;

use opentelemetry::trace::{Span as _, SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use serde_json::Value;

use super::stream::{StampedLine, StdoutTimeline};
use super::Runner;
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, GEN_AI_OPERATION_NAME, GEN_AI_PROVIDER_NAME, GEN_AI_REQUEST_MODEL, GEN_AI_RESPONSE_ID,
    GEN_AI_RESPONSE_MODEL, GEN_AI_TOOL_CALL_ARGUMENTS, GEN_AI_TOOL_CALL_ID, GEN_AI_TOOL_CALL_RESULT, GEN_AI_TOOL_NAME,
    GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS, GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS, GEN_AI_USAGE_INPUT_TOKENS,
    GEN_AI_USAGE_OUTPUT_TOKENS,
};
use crate::telemetry::ContentCapture;

/// A harness's stdout as captured, with when it started, when each line arrived, and
/// how it stopped.
#[derive(Debug, Clone, Copy)]
pub struct HarnessStream<'a> {
    pub stdout: &'a [u8],
    pub timeline: &'a StdoutTimeline,
    pub started_at: SystemTime,
    pub ended: StreamEnd,
}

/// How a harness's stdout stopped, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    /// The harness exited on its own.
    Exited(SystemTime),
    /// The run's time limit killed the harness.
    TimedOut(SystemTime),
}

impl StreamEnd {
    fn at(self) -> SystemTime {
        match self {
            Self::Exited(at) | Self::TimedOut(at) => at,
        }
    }

    fn unfinished(self) -> StepFailure {
        match self {
            Self::Exited(_) => StepFailure::Unfinished,
            Self::TimedOut(_) => StepFailure::TimedOut,
        }
    }
}

impl<'a> HarnessStream<'a> {
    fn events(&self) -> impl Iterator<Item = (Value, SystemTime)> + 'a {
        let stdout = self.stdout;
        self.timeline.lines(stdout).filter_map(|StampedLine { text, at }| {
            let text = std::str::from_utf8(text).ok()?.trim();
            if text.is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(text).ok().map(|value| (value, at))
        })
    }
}

/// Token counts one model turn reported, each `None` when the harness left it out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

impl TurnUsage {
    fn read(block: Option<&Value>, names: [&str; 4]) -> Self {
        let count = |name: &str| block.and_then(|block| block.get(name)).and_then(Value::as_u64);
        Self {
            input: count(names[0]),
            output: count(names[1]),
            cache_read: count(names[2]),
            cache_write: count(names[3]),
        }
    }

    fn attributes(self) -> impl Iterator<Item = KeyValue> {
        [
            (GEN_AI_USAGE_INPUT_TOKENS, self.input),
            (GEN_AI_USAGE_OUTPUT_TOKENS, self.output),
            (GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS, self.cache_read),
            (GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS, self.cache_write),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| KeyValue::new(key, i64::try_from(value).unwrap_or(i64::MAX))))
    }
}

/// Why a rebuilt span is marked failed, as its low-cardinality `error.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepFailure {
    /// The harness reported the tool call itself failed.
    ToolError,
    /// The turn the harness reported as failed.
    TurnFailed,
    /// The harness exited before the step reported finishing.
    Unfinished,
    /// The run's time limit killed the harness before the step reported finishing.
    TimedOut,
}

impl StepFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolError => "tool_error",
            Self::TurnFailed => "turn_failed",
            Self::Unfinished => "unfinished",
            Self::TimedOut => "timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepKind {
    Chat {
        /// The model `trg` asked the harness for, absent when the harness chose.
        request_model: Option<String>,
        /// The model the harness's stream says answered.
        response_model: Option<String>,
        response_id: Option<String>,
        usage: TurnUsage,
    },
    Tool {
        name: String,
        call_id: String,
        arguments: Option<String>,
        result: Option<String>,
    },
}

/// One `chat` or `execute_tool` span, rebuilt.
#[derive(Debug, Clone, PartialEq)]
pub struct HarnessStep {
    pub kind: StepKind,
    pub start: SystemTime,
    pub end: SystemTime,
    pub failure: Option<StepFailure>,
}

impl HarnessStep {
    pub fn name(&self) -> String {
        match &self.kind {
            StepKind::Chat {
                request_model,
                response_model,
                ..
            } => match request_model.as_ref().or(response_model.as_ref()) {
                Some(model) => format!("{} {model}", gen_ai_operation_name::CHAT),
                None => gen_ai_operation_name::CHAT.to_string(),
            },
            StepKind::Tool { name, .. } => format!("{} {name}", gen_ai_operation_name::EXECUTE_TOOL),
        }
    }

    pub fn is_tool(&self) -> bool {
        matches!(self.kind, StepKind::Tool { .. })
    }

    pub fn duration_secs(&self) -> f64 {
        self.end
            .duration_since(self.start)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or_default()
    }
}

/// What the harness's terminal event said about the whole invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessSummary {
    pub duration_ms: Option<u64>,
    pub api_duration_ms: Option<u64>,
    pub response_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reconstruction {
    pub steps: Vec<HarnessStep>,
    pub summary: HarnessSummary,
}

impl Reconstruction {
    pub fn tool_calls(&self) -> usize {
        self.steps.iter().filter(|step| step.is_tool()).count()
    }

    pub fn inference_calls(&self) -> usize {
        self.steps.len() - self.tool_calls()
    }
}

/// Whether tool arguments and results may be copied onto the rebuilt spans.
#[derive(Debug, Clone, Copy)]
struct ToolContent(bool);

impl ToolContent {
    fn render(self, value: Option<&Value>) -> Option<String> {
        if !self.0 {
            return None;
        }
        match value? {
            Value::Null => None,
            Value::String(text) => Some(text.clone()),
            other => Some(other.to_string()),
        }
    }
}

pub fn reconstruct(
    runner: Runner,
    stream: HarnessStream,
    request_model: Option<&str>,
    content: ContentCapture,
) -> Reconstruction {
    let content = ToolContent(content.includes_span());
    let mut builder = StepBuilder::new(stream.started_at, request_model);
    match runner {
        Runner::ClaudeCode => claude_code(&mut builder, stream, content),
        Runner::Codex => codex(&mut builder, stream, content),
        Runner::CursorAgent => cursor_agent(&mut builder, stream, content),
    }
    builder.finish(stream.ended)
}

struct OpenTool {
    name: String,
    call_id: String,
    arguments: Option<String>,
    start: SystemTime,
}

struct OpenTurn {
    key: Option<String>,
    response_model: Option<String>,
    response_id: Option<String>,
    usage: TurnUsage,
    start: SystemTime,
    end: SystemTime,
}

struct StepBuilder {
    steps: Vec<HarnessStep>,
    tools: BTreeMap<String, OpenTool>,
    turn: Option<OpenTurn>,
    summary: HarnessSummary,
    last_at: SystemTime,
    request_model: Option<String>,
}

impl StepBuilder {
    fn new(started_at: SystemTime, request_model: Option<&str>) -> Self {
        Self {
            steps: Vec::new(),
            tools: BTreeMap::new(),
            turn: None,
            summary: HarnessSummary::default(),
            last_at: started_at,
            request_model: request_model.map(str::to_string),
        }
    }

    fn arrived(&mut self, at: SystemTime) {
        self.last_at = self.last_at.max(at);
    }

    fn open_turn(&mut self, key: Option<String>, start: SystemTime) {
        self.close_turn(None);
        self.turn = Some(OpenTurn {
            key,
            response_model: None,
            response_id: None,
            usage: TurnUsage::default(),
            start,
            end: start,
        });
    }

    fn close_turn(&mut self, failure: Option<StepFailure>) {
        if let Some(turn) = self.turn.take() {
            if turn.response_model.is_some() {
                self.summary.response_model.clone_from(&turn.response_model);
            }
            self.steps.push(HarnessStep {
                kind: StepKind::Chat {
                    request_model: self.request_model.clone(),
                    response_model: turn.response_model,
                    response_id: turn.response_id,
                    usage: turn.usage,
                },
                start: turn.start,
                end: turn.end,
                failure,
            });
        }
    }

    fn open_tool(&mut self, call_id: String, name: String, arguments: Option<String>, at: SystemTime) {
        self.tools.entry(call_id.clone()).or_insert(OpenTool {
            name,
            call_id,
            arguments,
            start: at,
        });
    }

    /// Ends the call `call_id`, or records a zero-length one for a call the stream only
    /// reported finishing.
    fn close_tool(
        &mut self,
        call_id: &str,
        fallback_name: impl FnOnce() -> String,
        result: Option<String>,
        failure: Option<StepFailure>,
        at: SystemTime,
    ) {
        let tool = self.tools.remove(call_id).unwrap_or_else(|| OpenTool {
            name: fallback_name(),
            call_id: call_id.to_string(),
            arguments: None,
            start: at,
        });
        self.steps.push(HarnessStep {
            kind: StepKind::Tool {
                name: tool.name,
                call_id: tool.call_id,
                arguments: tool.arguments,
                result,
            },
            start: tool.start,
            end: at.max(tool.start),
            failure,
        });
    }

    fn finish(mut self, ended: StreamEnd) -> Reconstruction {
        let end = ended.at();
        if let Some(turn) = &mut self.turn {
            turn.end = end.max(turn.start);
        }
        self.close_turn(Some(ended.unfinished()));
        for (_, tool) in std::mem::take(&mut self.tools) {
            self.steps.push(HarnessStep {
                kind: StepKind::Tool {
                    name: tool.name,
                    call_id: tool.call_id,
                    arguments: tool.arguments,
                    result: None,
                },
                start: tool.start,
                end: end.max(tool.start),
                failure: Some(ended.unfinished()),
            });
        }
        self.steps.sort_by_key(|step| step.start);
        Reconstruction {
            steps: self.steps,
            summary: self.summary,
        }
    }

    fn read_result(&mut self, event: &Value) {
        self.summary.duration_ms = event.get("duration_ms").and_then(Value::as_u64);
        self.summary.api_duration_ms = event.get("duration_api_ms").and_then(Value::as_u64);
    }
}

fn event_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn content_blocks(event: &Value) -> impl Iterator<Item = &Value> {
    event
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

const CLAUDE_USAGE: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

/// A claude-code turn is every `assistant` event sharing one `message.id`; it starts
/// when the event before it arrived, which is when the request that produced it went out.
fn claude_code(builder: &mut StepBuilder, stream: HarnessStream, content: ToolContent) {
    for (event, at) in stream.events() {
        match event_type(&event) {
            Some("assistant") => {
                let message = event.get("message");
                let id = message.and_then(|message| string_at(message, "id"));
                let continues = builder
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.key.is_some() && turn.key == id);
                if !continues {
                    let start = builder.last_at;
                    builder.open_turn(id.clone(), start);
                }
                if let Some(turn) = &mut builder.turn {
                    turn.end = at;
                    if let Some(message) = message {
                        turn.response_model = string_at(message, "model").or(turn.response_model.take());
                        turn.response_id = id.or(turn.response_id.take());
                        if message.get("usage").is_some() {
                            turn.usage = TurnUsage::read(message.get("usage"), CLAUDE_USAGE);
                        }
                    }
                }
                for block in content_blocks(&event) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                        continue;
                    }
                    let Some(call_id) = string_at(block, "id") else {
                        continue;
                    };
                    let name = string_at(block, "name").unwrap_or_else(|| "unknown".to_string());
                    builder.open_tool(call_id, name, content.render(block.get("input")), at);
                }
            }
            Some("user") => {
                builder.close_turn(None);
                for block in content_blocks(&event) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let Some(call_id) = block.get("tool_use_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let failure = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        .then_some(StepFailure::ToolError);
                    builder.close_tool(
                        call_id,
                        || "unknown".to_string(),
                        content.render(block.get("content")),
                        failure,
                        at,
                    );
                }
            }
            Some("result") => {
                builder.close_turn(None);
                builder.read_result(&event);
            }
            _ => builder.close_turn(None),
        }
        builder.arrived(at);
    }
}

const CODEX_USAGE: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
];

const CODEX_TOOL_ITEMS: [&str; 4] = ["command_execution", "file_change", "mcp_tool_call", "web_search"];

fn codex_tool_name(item: &Value) -> Option<String> {
    let kind = item.get("type").and_then(Value::as_str)?;
    if !CODEX_TOOL_ITEMS.contains(&kind) {
        return None;
    }
    if kind == "mcp_tool_call" {
        if let Some(tool) = string_at(item, "tool") {
            return Some(tool);
        }
    }
    Some(kind.to_string())
}

fn codex_arguments(item: &Value) -> Option<&Value> {
    ["arguments", "command", "changes", "query"]
        .into_iter()
        .find_map(|key| item.get(key))
}

fn codex_result(item: &Value) -> Option<&Value> {
    ["result", "aggregated_output", "error"]
        .into_iter()
        .find_map(|key| item.get(key))
}

fn codex_failure(item: &Value) -> Option<StepFailure> {
    let status_failed = matches!(item.get("status").and_then(Value::as_str), Some("failed" | "declined"));
    let nonzero_exit = item
        .get("exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|code| code != 0);
    (status_failed || nonzero_exit).then_some(StepFailure::ToolError)
}

/// A codex turn is one or more model calls with tool calls between them, and codex
/// reports only the tool calls. Every stretch of the turn with no tool running is taken
/// as a model call: the one that chose the next tool, or the one that ended the turn.
/// Codex names no model on its stream, so its model calls carry only the one the run
/// asked for.
fn codex(builder: &mut StepBuilder, stream: HarnessStream, content: ToolContent) {
    let mut turn: Option<CodexTurn> = None;
    for (event, at) in stream.events() {
        match event_type(&event) {
            Some("turn.started") => turn = Some(CodexTurn::start(builder, at)),
            Some("turn.completed") => {
                let usage = TurnUsage::read(event.get("usage"), CODEX_USAGE);
                if let Some(turn) = turn.take() {
                    turn.end(builder, at, None, |last| *last = usage);
                }
            }
            Some("turn.failed") => {
                if let Some(turn) = turn.take() {
                    turn.end(builder, at, Some(StepFailure::TurnFailed), |_| {});
                }
            }
            Some(kind @ ("item.started" | "item.completed")) => {
                let Some(item) = event.get("item") else {
                    builder.arrived(at);
                    continue;
                };
                let (Some(call_id), Some(name)) = (string_at(item, "id"), codex_tool_name(item)) else {
                    builder.arrived(at);
                    continue;
                };
                let arguments = content.render(codex_arguments(item));
                if let Some(open) = builder.tools.get_mut(&call_id) {
                    open.arguments = open.arguments.take().or(arguments);
                } else {
                    if builder.tools.is_empty() {
                        if let Some(chat) = &mut builder.turn {
                            chat.end = at;
                        }
                        builder.close_turn(None);
                    }
                    builder.open_tool(call_id.clone(), name.clone(), arguments, at);
                }
                if kind == "item.completed" {
                    builder.close_tool(
                        &call_id,
                        || name,
                        content.render(codex_result(item)),
                        codex_failure(item),
                        at,
                    );
                    if builder.tools.is_empty() && turn.is_some() {
                        builder.open_turn(None, at);
                    }
                }
            }
            _ => {}
        }
        if let Some(chat) = &mut builder.turn {
            chat.end = chat.end.max(at);
        }
        builder.arrived(at);
    }
}

/// Where a codex turn's model calls begin among the rebuilt steps.
struct CodexTurn {
    first_step: usize,
}

impl CodexTurn {
    fn start(builder: &mut StepBuilder, at: SystemTime) -> Self {
        builder.close_turn(None);
        let first_step = builder.steps.len();
        builder.open_turn(None, at);
        Self { first_step }
    }

    /// Ends the turn at `at`, handing its last model call to `last_chat`, the only call
    /// the turn's usage and failure can be pinned on.
    fn end(
        self,
        builder: &mut StepBuilder,
        at: SystemTime,
        failure: Option<StepFailure>,
        last_chat: impl FnOnce(&mut TurnUsage),
    ) {
        if let Some(chat) = &mut builder.turn {
            chat.end = at;
            last_chat(&mut chat.usage);
            builder.close_turn(failure);
            return;
        }
        let closed = builder.steps.get_mut(self.first_step..).unwrap_or_default();
        if let Some(step) = closed.iter_mut().rev().find(|step| !step.is_tool()) {
            if let StepKind::Chat { usage, .. } = &mut step.kind {
                last_chat(usage);
            }
            step.failure = step.failure.or(failure);
        }
    }
}

/// The tool a cursor-agent `tool_call` event names, from its single `<name>ToolCall` member.
fn cursor_tool(event: &Value) -> Option<(String, &Value)> {
    let calls = event.get("tool_call")?.as_object()?;
    calls.iter().find_map(|(key, call)| {
        key.strip_suffix("ToolCall")
            .filter(|name| !name.is_empty())
            .map(|name| (name.to_string(), call))
    })
}

fn cursor_call_id(event: &Value) -> Option<String> {
    string_at(event, "call_id").or_else(|| event.get("tool_call").and_then(|call| string_at(call, "toolCallId")))
}

/// A cursor-agent turn is a run of consecutive `assistant` events. cursor-agent reports
/// no usage per turn, only for the whole invocation.
fn cursor_agent(builder: &mut StepBuilder, stream: HarnessStream, content: ToolContent) {
    let mut model: Option<String> = None;
    for (event, at) in stream.events() {
        match event_type(&event) {
            Some("system") => {
                if let Some(init_model) = string_at(&event, "model") {
                    model = Some(init_model);
                }
                builder.close_turn(None);
            }
            Some("assistant") => {
                if builder.turn.is_none() {
                    let start = builder.last_at;
                    builder.open_turn(None, start);
                }
                if let Some(turn) = &mut builder.turn {
                    turn.end = at;
                    turn.response_model.clone_from(&model);
                }
            }
            Some("tool_call") => {
                builder.close_turn(None);
                if let (Some(call_id), Some((name, call))) = (cursor_call_id(&event), cursor_tool(&event)) {
                    match event.get("subtype").and_then(Value::as_str) {
                        Some("started") => builder.open_tool(call_id, name, content.render(call.get("args")), at),
                        Some("completed") => {
                            let result = call.get("result");
                            let failure = result
                                .and_then(|result| result.get("error"))
                                .map(|_| StepFailure::ToolError);
                            if !builder.tools.contains_key(&call_id) {
                                builder.open_tool(call_id.clone(), name.clone(), content.render(call.get("args")), at);
                            }
                            builder.close_tool(&call_id, || name, content.render(result), failure, at);
                        }
                        _ => {}
                    }
                }
            }
            Some("result") => {
                builder.close_turn(None);
                builder.read_result(&event);
            }
            Some("thinking") => {}
            _ => builder.close_turn(None),
        }
        builder.arrived(at);
    }
    if builder.summary.response_model.is_none() {
        builder.summary.response_model = model;
    }
}

/// Emits `steps` as children of `parent`, each with the start and end it was rebuilt with.
pub fn emit<T: Tracer>(tracer: &T, parent: &Context, provider: &'static str, steps: &[HarnessStep]) {
    for step in steps {
        let mut attributes = vec![KeyValue::new(GEN_AI_PROVIDER_NAME, provider)];
        let kind = match &step.kind {
            StepKind::Chat {
                request_model,
                response_model,
                response_id,
                usage,
            } => {
                attributes.push(KeyValue::new(GEN_AI_OPERATION_NAME, gen_ai_operation_name::CHAT));
                if let Some(model) = request_model {
                    attributes.push(KeyValue::new(GEN_AI_REQUEST_MODEL, model.clone()));
                }
                if let Some(model) = response_model {
                    attributes.push(KeyValue::new(GEN_AI_RESPONSE_MODEL, model.clone()));
                }
                if let Some(id) = response_id {
                    attributes.push(KeyValue::new(GEN_AI_RESPONSE_ID, id.clone()));
                }
                attributes.extend(usage.attributes());
                SpanKind::Client
            }
            StepKind::Tool {
                name,
                call_id,
                arguments,
                result,
            } => {
                attributes.push(KeyValue::new(
                    GEN_AI_OPERATION_NAME,
                    gen_ai_operation_name::EXECUTE_TOOL,
                ));
                attributes.push(KeyValue::new(GEN_AI_TOOL_NAME, name.clone()));
                attributes.push(KeyValue::new(GEN_AI_TOOL_CALL_ID, call_id.clone()));
                if let Some(arguments) = arguments {
                    attributes.push(KeyValue::new(GEN_AI_TOOL_CALL_ARGUMENTS, arguments.clone()));
                }
                if let Some(result) = result {
                    attributes.push(KeyValue::new(GEN_AI_TOOL_CALL_RESULT, result.clone()));
                }
                SpanKind::Internal
            }
        };
        if let Some(failure) = step.failure {
            attributes.push(KeyValue::new(ERROR_TYPE, failure.as_str()));
        }
        let builder = tracer
            .span_builder(step.name())
            .with_kind(kind)
            .with_start_time(step.start)
            .with_attributes(attributes);
        let mut span = tracer.build_with_context(builder, parent);
        if let Some(failure) = step.failure {
            span.set_status(Status::error(failure.as_str()));
        }
        span.end_with_timestamp(step.end);
    }
}

/// Whether `parent` belongs to a trace that is being recorded.
pub fn records(parent: &Context) -> bool {
    parent.span().span_context().is_valid()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
    }

    fn line_count(stdout: &[u8]) -> u64 {
        stdout.split_inclusive(|byte| *byte == b'\n').count() as u64
    }

    /// A stream whose lines arrive one second apart, the first at second 1.
    fn stamped(stdout: &[u8]) -> (StdoutTimeline, SystemTime) {
        let times: Vec<SystemTime> = (1..=line_count(stdout)).map(at).collect();
        (StdoutTimeline::stamped(stdout, &times), at(0))
    }

    /// Rebuilds a stream whose harness exited as its last line arrived.
    fn rebuild(runner: Runner, stdout: &[u8], model: Option<&str>, content: ContentCapture) -> Reconstruction {
        let ended = StreamEnd::Exited(at(line_count(stdout)));
        rebuild_ending(runner, stdout, model, content, ended)
    }

    fn rebuild_ending(
        runner: Runner,
        stdout: &[u8],
        model: Option<&str>,
        content: ContentCapture,
        ended: StreamEnd,
    ) -> Reconstruction {
        let (timeline, started_at) = stamped(stdout);
        reconstruct(
            runner,
            HarnessStream {
                stdout,
                timeline: &timeline,
                started_at,
                ended,
            },
            model,
            content,
        )
    }

    fn chat_usages(reconstruction: &Reconstruction) -> Vec<TurnUsage> {
        reconstruction
            .steps
            .iter()
            .filter_map(|step| match step.kind {
                StepKind::Chat { usage, .. } => Some(usage),
                StepKind::Tool { .. } => None,
            })
            .collect()
    }

    fn tool<'a>(reconstruction: &'a Reconstruction, name: &str) -> &'a HarnessStep {
        reconstruction
            .steps
            .iter()
            .find(|step| matches!(&step.kind, StepKind::Tool { name: tool, .. } if tool == name))
            .unwrap_or_else(|| panic!("no tool step {name}"))
    }

    fn chats(reconstruction: &Reconstruction) -> Vec<&HarnessStep> {
        reconstruction.steps.iter().filter(|step| !step.is_tool()).collect()
    }

    const CLAUDE: &[u8] = br#"{"type":"system","subtype":"init","model":"claude-sonnet-4-5"}
{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"Reading."}],"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}
{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"SKILL.md"}}],"usage":{"input_tokens":10,"output_tokens":7,"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"skill body"}]}}
{"type":"assistant","message":{"id":"msg_2","model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"false"}}],"usage":{"input_tokens":20,"output_tokens":3}}}
{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_2","content":"exit 1","is_error":true}]}}
{"type":"assistant","message":{"id":"msg_3","model":"claude-sonnet-4-5","content":[{"type":"text","text":"Done."}],"usage":{"input_tokens":30,"output_tokens":4}}}
{"type":"result","is_error":false,"duration_ms":6500,"duration_api_ms":4200,"total_cost_usd":0.01}
"#;

    #[test]
    fn claude_code_tools_span_from_use_to_result() {
        let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::NoContent);

        let read = tool(&rebuilt, "Read");
        assert_eq!((read.start, read.end), (at(3), at(4)));
        assert_eq!(read.failure, None);
        assert!(
            matches!(&read.kind, StepKind::Tool { call_id, arguments: None, result: None, .. } if call_id == "toolu_1")
        );

        let bash = tool(&rebuilt, "Bash");
        assert_eq!((bash.start, bash.end), (at(5), at(6)));
        assert_eq!(bash.failure, Some(StepFailure::ToolError));
    }

    #[test]
    fn claude_code_turns_group_by_message_and_carry_their_own_usage() {
        let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::NoContent);
        let turns = chats(&rebuilt);
        assert_eq!(turns.len(), 3);

        assert_eq!((turns[0].start, turns[0].end), (at(1), at(3)));
        assert_eq!(turns[0].name(), "chat claude-sonnet-4-5");
        assert_eq!(
            turns[0].kind,
            StepKind::Chat {
                request_model: None,
                response_model: Some("claude-sonnet-4-5".to_string()),
                response_id: Some("msg_1".to_string()),
                usage: TurnUsage {
                    input: Some(10),
                    output: Some(7),
                    cache_read: Some(100),
                    cache_write: Some(5)
                },
            }
        );
        assert_eq!((turns[1].start, turns[1].end), (at(4), at(5)));
        assert_eq!((turns[2].start, turns[2].end), (at(6), at(7)));

        assert_eq!(rebuilt.tool_calls(), 2);
        assert_eq!(rebuilt.inference_calls(), 3);
        assert_eq!(
            rebuilt.summary,
            HarnessSummary {
                duration_ms: Some(6500),
                api_duration_ms: Some(4200),
                response_model: Some("claude-sonnet-4-5".to_string()),
            }
        );
    }

    #[test]
    fn tool_content_is_copied_only_under_span_content_capture() {
        let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::SpanOnly);
        assert!(matches!(
            &tool(&rebuilt, "Read").kind,
            StepKind::Tool { arguments: Some(arguments), result: Some(result), .. }
                if arguments == r#"{"file_path":"SKILL.md"}"# && result == "skill body"
        ));

        let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::EventOnly);
        assert!(matches!(
            &tool(&rebuilt, "Read").kind,
            StepKind::Tool {
                arguments: None,
                result: None,
                ..
            }
        ));
    }

    const CUT_OFF: &[u8] = br#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{}}]}}
{"type":"system","subtype":"status"}
"#;

    #[test]
    fn a_tool_cut_off_by_the_harness_exiting_ends_when_it_exited_and_is_unfinished() {
        let ended = StreamEnd::Exited(at(5));
        let rebuilt = rebuild_ending(Runner::ClaudeCode, CUT_OFF, None, ContentCapture::NoContent, ended);
        let bash = tool(&rebuilt, "Bash");
        assert_eq!((bash.start, bash.end), (at(1), at(5)));
        assert_eq!(bash.failure, Some(StepFailure::Unfinished));
    }

    #[test]
    fn a_tool_cut_off_by_the_time_limit_ends_when_the_harness_was_killed_and_is_a_timeout() {
        let ended = StreamEnd::TimedOut(at(30));
        let rebuilt = rebuild_ending(Runner::ClaudeCode, CUT_OFF, None, ContentCapture::NoContent, ended);
        let bash = tool(&rebuilt, "Bash");
        assert_eq!((bash.start, bash.end), (at(1), at(30)));
        assert_eq!(bash.failure, Some(StepFailure::TimedOut));

        let (spans, _) = exported(&rebuilt.steps);
        let span = spans.iter().find(|span| span.name == "execute_tool Bash").unwrap();
        assert_eq!(span.end_time, at(30));
        let error_type = span
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == ERROR_TYPE)
            .map(|kv| kv.value.clone());
        assert_eq!(error_type, Some(opentelemetry::Value::from("timeout")));
    }

    #[test]
    fn claude_code_chat_usage_is_each_messages_last_snapshot_and_never_reconciled_with_the_result_total() {
        let stdout = br#"{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"a"}],"usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","message":{"id":"msg_1","model":"claude-sonnet-4-5","content":[{"type":"text","text":"b"}],"usage":{"input_tokens":10,"output_tokens":6}}}
{"type":"assistant","message":{"id":"msg_2","model":"claude-sonnet-4-5","content":[{"type":"text","text":"c"}],"usage":{"input_tokens":20,"output_tokens":4}}}
{"type":"result","is_error":false,"duration_ms":10,"usage":{"input_tokens":500,"output_tokens":90}}
"#;
        let rebuilt = rebuild(Runner::ClaudeCode, stdout, None, ContentCapture::NoContent);
        let reported = |input, output| TurnUsage {
            input: Some(input),
            output: Some(output),
            ..TurnUsage::default()
        };
        assert_eq!(chat_usages(&rebuilt), vec![reported(10, 6), reported(20, 4)]);
    }

    const CODEX: &[u8] = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Reading."}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"cat SKILL.md","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"cat SKILL.md","aggregated_output":"body","exit_code":0,"status":"completed"}}
{"type":"item.started","item":{"id":"item_2","type":"mcp_tool_call","server":"mock","tool":"lookup","arguments":{"q":"x"},"status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_2","type":"mcp_tool_call","server":"mock","tool":"lookup","status":"failed","error":{"message":"boom"}}}
{"type":"item.completed","item":{"id":"item_3","type":"file_change","changes":[{"path":"out.md","kind":"add"}],"status":"completed"}}
{"type":"turn.completed","usage":{"input_tokens":200,"cached_input_tokens":150,"output_tokens":40}}
"#;

    #[test]
    fn codex_tools_span_from_item_start_to_completion() {
        let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);

        let command = tool(&rebuilt, "command_execution");
        assert_eq!((command.start, command.end), (at(4), at(5)));
        assert_eq!(command.failure, None);

        let lookup = tool(&rebuilt, "lookup");
        assert_eq!((lookup.start, lookup.end), (at(6), at(7)));
        assert_eq!(lookup.failure, Some(StepFailure::ToolError));

        let change = tool(&rebuilt, "file_change");
        assert_eq!((change.start, change.end), (at(8), at(8)));
        assert_eq!(rebuilt.tool_calls(), 3);
    }

    #[test]
    fn codex_model_calls_fill_the_gaps_between_tools_without_overlapping_them() {
        let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);
        let spans: Vec<(SystemTime, SystemTime, bool)> = rebuilt
            .steps
            .iter()
            .map(|step| (step.start, step.end, step.is_tool()))
            .collect();
        assert_eq!(
            spans,
            vec![
                (at(2), at(4), false),
                (at(4), at(5), true),
                (at(5), at(6), false),
                (at(6), at(7), true),
                (at(7), at(8), false),
                (at(8), at(8), true),
                (at(8), at(9), false),
            ]
        );
        assert_eq!(rebuilt.inference_calls(), 4);
        assert!(chats(&rebuilt).iter().all(|chat| chat.name() == "chat gpt-5"));

        let unnamed = rebuild(Runner::Codex, CODEX, None, ContentCapture::NoContent);
        assert_eq!(chats(&unnamed)[0].name(), "chat");
    }

    #[test]
    fn codex_turn_usage_lands_once_on_the_turns_last_model_call() {
        let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);
        let turn = TurnUsage {
            input: Some(200),
            output: Some(40),
            cache_read: Some(150),
            cache_write: None,
        };
        let unattributed = TurnUsage::default();
        assert_eq!(
            chat_usages(&rebuilt),
            vec![unattributed, unattributed, unattributed, turn]
        );
    }

    #[test]
    fn a_codex_turn_without_tools_is_a_single_model_call() {
        let stdout = br#"{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"Done."}}
{"type":"turn.completed","usage":{"input_tokens":20,"output_tokens":4}}
"#;
        let rebuilt = rebuild(Runner::Codex, stdout, Some("gpt-5"), ContentCapture::NoContent);
        let turns = chats(&rebuilt);
        assert_eq!(turns.len(), 1);
        assert_eq!((turns[0].start, turns[0].end), (at(1), at(3)));
    }

    const CODEX_CUT_OFF_AFTER_TOOL: &[u8] = br#"{"type":"turn.started"}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"ls","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"ls","aggregated_output":"","exit_code":0,"status":"completed"}}
{"type":"system","subtype":"status"}
"#;

    #[test]
    fn a_codex_chat_cut_off_by_the_time_limit_ends_when_the_harness_was_killed_and_is_a_timeout() {
        let ended = StreamEnd::TimedOut(at(30));
        let rebuilt = rebuild_ending(
            Runner::Codex,
            CODEX_CUT_OFF_AFTER_TOOL,
            Some("gpt-5"),
            ContentCapture::NoContent,
            ended,
        );
        let last_chat = chats(&rebuilt).into_iter().last().unwrap();
        assert_eq!(last_chat.end, at(30));
        assert_eq!(last_chat.failure, Some(StepFailure::TimedOut));

        let (spans, _) = exported(&rebuilt.steps);
        let span = spans.iter().rfind(|span| span.name == "chat gpt-5").unwrap();
        assert_eq!(span.end_time, at(30));
        let error_type = span
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == ERROR_TYPE)
            .map(|kv| kv.value.clone());
        assert_eq!(error_type, Some(opentelemetry::Value::from("timeout")));
    }

    #[test]
    fn a_failed_codex_turn_marks_only_its_last_model_call() {
        let stdout = br#"{"type":"turn.started"}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"ls","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"ls","aggregated_output":"","exit_code":0,"status":"completed"}}
{"type":"turn.failed"}
"#;
        let rebuilt = rebuild(Runner::Codex, stdout, None, ContentCapture::NoContent);
        let failures: Vec<Option<StepFailure>> = chats(&rebuilt).iter().map(|chat| chat.failure).collect();
        assert_eq!(failures, vec![None, Some(StepFailure::TurnFailed)]);
    }

    const CURSOR: &[u8] = br#"{"type":"system","subtype":"init","model":"gpt-5","cwd":"/w"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Reading."}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":" more"}]}}
{"type":"tool_call","subtype":"started","call_id":"call-1","tool_call":{"readToolCall":{"args":{"path":"SKILL.md"}}}}
{"type":"thinking","text":"..."}
{"type":"tool_call","subtype":"completed","call_id":"call-1","tool_call":{"readToolCall":{"args":{"path":"SKILL.md"},"result":{"success":{"content":"body"}}}}}
{"type":"tool_call","subtype":"started","tool_call":{"editToolCall":{"args":{"path":"out.md"}},"toolCallId":"call-2"}}
{"type":"tool_call","subtype":"completed","tool_call":{"editToolCall":{"args":{"path":"out.md"},"result":{"error":{"message":"denied"}}},"toolCallId":"call-2"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}
{"type":"result","is_error":false,"duration_ms":9000,"duration_api_ms":7000,"result":"done"}
"#;

    #[test]
    fn cursor_agent_tools_span_from_started_to_completed() {
        let rebuilt = rebuild(Runner::CursorAgent, CURSOR, None, ContentCapture::NoContent);

        let read = tool(&rebuilt, "read");
        assert_eq!((read.start, read.end), (at(4), at(6)));
        assert_eq!(read.failure, None);

        let edit = tool(&rebuilt, "edit");
        assert_eq!((edit.start, edit.end), (at(7), at(8)));
        assert_eq!(edit.failure, Some(StepFailure::ToolError));
    }

    #[test]
    fn cursor_agent_turns_are_runs_of_assistant_events_named_by_the_init_model() {
        let rebuilt = rebuild(Runner::CursorAgent, CURSOR, None, ContentCapture::NoContent);
        let turns = chats(&rebuilt);
        assert_eq!(turns.len(), 2);
        assert_eq!((turns[0].start, turns[0].end), (at(1), at(3)));
        assert_eq!((turns[1].start, turns[1].end), (at(8), at(9)));
        assert_eq!(turns[0].name(), "chat gpt-5");
        assert_eq!(
            rebuilt.summary,
            HarnessSummary {
                duration_ms: Some(9000),
                api_duration_ms: Some(7000),
                response_model: Some("gpt-5".to_string()),
            }
        );
    }

    fn exported(steps: &[HarnessStep]) -> (Vec<SpanData>, SpanData) {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("trg");
        let parent = tracer.start("invoke_agent Claude Code");
        let parent_cx = Context::current_with_span(parent);
        assert!(records(&parent_cx));
        emit(&tracer, &parent_cx, "anthropic", steps);
        parent_cx.span().end();
        let _ = provider.force_flush();
        let mut spans = exporter.get_finished_spans().unwrap();
        let index = spans
            .iter()
            .position(|span| span.name == "invoke_agent Claude Code")
            .unwrap();
        let parent = spans.remove(index);
        (spans, parent)
    }

    #[test]
    fn emitted_spans_are_children_of_invoke_agent_with_the_rebuilt_times() {
        let rebuilt = rebuild(Runner::ClaudeCode, CLAUDE, None, ContentCapture::NoContent);
        let (spans, parent) = exported(&rebuilt.steps);
        assert_eq!(spans.len(), rebuilt.steps.len());
        for span in &spans {
            assert_eq!(span.parent_span_id, parent.span_context.span_id(), "{}", span.name);
            assert_eq!(span.span_context.trace_id(), parent.span_context.trace_id());
        }

        let read = spans.iter().find(|span| span.name == "execute_tool Read").unwrap();
        assert_eq!((read.start_time, read.end_time), (at(3), at(4)));
        assert_eq!(read.span_kind, SpanKind::Internal);
        let attribute = |span: &SpanData, key: &str| {
            span.attributes
                .iter()
                .find(|kv| kv.key.as_str() == key)
                .map(|kv| kv.value.to_string())
        };
        assert_eq!(attribute(read, GEN_AI_OPERATION_NAME).as_deref(), Some("execute_tool"));
        assert_eq!(attribute(read, GEN_AI_TOOL_CALL_ID).as_deref(), Some("toolu_1"));
        assert_eq!(attribute(read, GEN_AI_TOOL_CALL_ARGUMENTS), None);

        let bash = spans.iter().find(|span| span.name == "execute_tool Bash").unwrap();
        assert_eq!(attribute(bash, ERROR_TYPE).as_deref(), Some("tool_error"));
        assert!(matches!(bash.status, Status::Error { .. }));

        let chat = spans.iter().find(|span| span.name == "chat claude-sonnet-4-5").unwrap();
        assert_eq!(chat.span_kind, SpanKind::Client);
        assert_eq!(
            value(chat, GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS),
            Some(opentelemetry::Value::I64(100))
        );
        assert_eq!(attribute(chat, GEN_AI_PROVIDER_NAME).as_deref(), Some("anthropic"));
    }

    fn value(span: &SpanData, key: &str) -> Option<opentelemetry::Value> {
        span.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    }

    fn chat_models(
        runner: Runner,
        stdout: &[u8],
        requested: Option<&str>,
    ) -> Vec<(String, Option<String>, Option<String>)> {
        let rebuilt = rebuild(runner, stdout, requested, ContentCapture::NoContent);
        let (spans, _) = exported(&rebuilt.steps);
        let text = |span: &SpanData, key: &str| value(span, key).map(|value| value.to_string());
        spans
            .iter()
            .filter(|span| value(span, GEN_AI_OPERATION_NAME) == Some(opentelemetry::Value::from("chat")))
            .map(|span| {
                (
                    span.name.to_string(),
                    text(span, GEN_AI_REQUEST_MODEL),
                    text(span, GEN_AI_RESPONSE_MODEL),
                )
            })
            .collect()
    }

    #[test]
    fn a_chat_span_names_a_request_model_only_when_trg_requested_one() {
        let unrequested = chat_models(Runner::ClaudeCode, CLAUDE, None);
        assert!(!unrequested.is_empty());
        for (name, request, response) in unrequested {
            assert_eq!(name, "chat claude-sonnet-4-5");
            assert_eq!(request, None);
            assert_eq!(response.as_deref(), Some("claude-sonnet-4-5"));
        }

        for (name, request, response) in chat_models(Runner::ClaudeCode, CLAUDE, Some("sonnet")) {
            assert_eq!(name, "chat sonnet");
            assert_eq!(request.as_deref(), Some("sonnet"));
            assert_eq!(response.as_deref(), Some("claude-sonnet-4-5"));
        }

        for (name, request, response) in chat_models(Runner::CursorAgent, CURSOR, None) {
            assert_eq!(name, "chat gpt-5");
            assert_eq!(request, None);
            assert_eq!(response.as_deref(), Some("gpt-5"));
        }
    }

    #[test]
    fn a_codex_chat_span_carries_the_requested_model_and_no_response_model_its_stream_never_named() {
        for (name, request, response) in chat_models(Runner::Codex, CODEX, Some("gpt-5")) {
            assert_eq!(name, "chat gpt-5");
            assert_eq!(request.as_deref(), Some("gpt-5"));
            assert_eq!(response, None);
        }
        let rebuilt = rebuild(Runner::Codex, CODEX, Some("gpt-5"), ContentCapture::NoContent);
        assert_eq!(rebuilt.summary.response_model, None);
    }

    #[test]
    fn an_unrecorded_parent_is_recognised() {
        assert!(!records(&Context::new()));
    }
}
