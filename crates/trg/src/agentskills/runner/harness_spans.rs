//! Spans rebuilt from a harness's stamped stdout, for a harness that exported none.
//!
//! Every harness narrates its model turns and tool calls on stdout. Read with the
//! time each line arrived, that narration is enough to give the `invoke_agent` span
//! the `chat` and `execute_tool` children the harness itself would have emitted,
//! timed when the harness reported each step rather than when `trg` got around to
//! reading it. Only `trg`'s own stamps are used: a harness's own clock is never
//! trusted to agree with this machine's.

mod constants;
use constants::{CLAUDE_USAGE, CODEX_TOOL_ITEMS, CODEX_USAGE};
use std::collections::BTreeMap;
use std::time::SystemTime;

use opentelemetry::trace::{Span as _, SpanKind, Status, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use opentelemetry_semantic_conventions::attribute::ERROR_TYPE;
use serde_json::Value;

use super::stream::{StampedLine, StdoutTimeline};
use super::Runner;
use crate::agentskills::transcript_metrics::{ObservedToolCall, TranscriptMetrics, TurnCount};
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
    /// One of the run's caps on tool calls or turns killed the harness.
    CapReached(SystemTime),
}

impl StreamEnd {
    fn at(self) -> SystemTime {
        match self {
            Self::Exited(at) | Self::TimedOut(at) | Self::CapReached(at) => at,
        }
    }

    fn unfinished(self) -> StepFailure {
        match self {
            Self::Exited(_) => StepFailure::Unfinished,
            Self::TimedOut(_) => StepFailure::TimedOut,
            Self::CapReached(_) => StepFailure::BudgetExhausted,
        }
    }
}

impl<'a> HarnessStream<'a> {
    fn events(&self) -> impl Iterator<Item = (Value, SystemTime)> + 'a {
        let stdout = self.stdout;
        self.timeline
            .lines(stdout)
            .filter_map(|StampedLine { text, at }| parse_event(text).map(|value| (value, at)))
    }
}

fn parse_event(line: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(line).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    serde_json::from_str::<Value>(text).ok()
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

    /// Every input token the call sent, cached or not; `None` when none were reported.
    fn sent(self) -> Option<u64> {
        [self.input, self.cache_read, self.cache_write]
            .into_iter()
            .flatten()
            .reduce(u64::saturating_add)
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
    /// A cap on the run's tool calls or turns killed the harness before the step
    /// reported finishing.
    BudgetExhausted,
}

impl StepFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolError => "tool_error",
            Self::TurnFailed => "turn_failed",
            Self::Unfinished => "unfinished",
            Self::TimedOut => "timeout",
            Self::BudgetExhausted => "budget_exhausted",
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
    rebuild(runner, stream, request_model, ToolContent(content.includes_span()))
}

/// Turn and tool counts for a run, read from its captured stdout. A turn's input
/// counts only for a harness that reports usage per model call: codex reports one total
/// per turn spanning every model call in it, and cursor-agent none at all.
pub fn measure(runner: Runner, stdout: &[u8]) -> TranscriptMetrics {
    let reconstruction = reconstruct_for_measuring(runner, stdout);
    let peak_turn_input_tokens = match runner {
        Runner::ClaudeCode => reconstruction
            .steps
            .iter()
            .filter_map(|step| match &step.kind {
                StepKind::Chat { usage, .. } => usage.sent(),
                StepKind::Tool { .. } => None,
            })
            .max(),
        Runner::Codex | Runner::CursorAgent => None,
    };
    let calls = reconstruction.steps.iter().filter_map(|step| match &step.kind {
        StepKind::Tool {
            name,
            arguments,
            result,
            ..
        } => Some(ObservedToolCall {
            tool: name,
            arguments: arguments.as_deref(),
            result: result.as_deref(),
        }),
        StepKind::Chat { .. } => None,
    });
    let turns = TurnCount::new(u64::try_from(reconstruction.inference_calls()).unwrap_or(u64::MAX));
    TranscriptMetrics::measure(turns, peak_turn_input_tokens, calls)
}

/// Every step with its arguments and results kept, for measuring a run rather than
/// exporting it, so nothing it holds ever reaches a span.
fn reconstruct_for_measuring(runner: Runner, stdout: &[u8]) -> Reconstruction {
    let mut live = LiveReconstruction::begin(runner, SystemTime::UNIX_EPOCH, None, ToolContent(true));
    let selected_items = stdout.split(|byte| *byte == b'\n');
    for line in selected_items {
        live.observe_line(line, SystemTime::UNIX_EPOCH);
    }
    live.finish(StreamEnd::Exited(SystemTime::UNIX_EPOCH))
}

fn rebuild(runner: Runner, stream: HarnessStream, request_model: Option<&str>, content: ToolContent) -> Reconstruction {
    let mut live = LiveReconstruction::begin(runner, stream.started_at, request_model, content);
    for (event, at) in stream.events() {
        live.observe(&event, at);
    }
    live.finish(stream.ended)
}

/// A reconstruction fed one line at a time, so a run can be watched while it executes
/// and stopped at the step that crossed one of its caps.
pub struct LiveReconstruction {
    runner: Runner,
    builder: StepBuilder,
    content: ToolContent,
    codex_turn: Option<CodexTurn>,
    cursor_model: Option<String>,
}

impl LiveReconstruction {
    fn begin(runner: Runner, started_at: SystemTime, request_model: Option<&str>, content: ToolContent) -> Self {
        Self {
            runner,
            builder: StepBuilder::new(started_at, request_model),
            content,
            codex_turn: None,
            cursor_model: None,
        }
    }

    /// Counts steps without keeping what any of them carried.
    pub fn counting(runner: Runner, started_at: SystemTime) -> Self {
        Self::begin(runner, started_at, None, ToolContent(false))
    }

    pub fn observe_line(&mut self, line: &[u8], at: SystemTime) {
        if let Some(event) = parse_event(line) {
            self.observe(&event, at);
        }
    }

    fn observe(&mut self, event: &Value, at: SystemTime) {
        match self.runner {
            Runner::ClaudeCode => claude_code(&mut self.builder, event, at, self.content),
            Runner::Codex => codex(&mut self.builder, &mut self.codex_turn, event, at, self.content),
            Runner::CursorAgent => cursor_agent(&mut self.builder, &mut self.cursor_model, event, at, self.content),
        }
    }

    /// Tool calls seen so far, including any still running.
    pub fn tool_calls(&self) -> usize {
        self.builder.steps.iter().filter(|step| step.is_tool()).count() + self.builder.tools.len()
    }

    /// Model calls seen so far, including one still answering.
    pub fn model_calls(&self) -> usize {
        self.builder.steps.iter().filter(|step| !step.is_tool()).count() + usize::from(self.builder.turn.is_some())
    }

    fn finish(mut self, ended: StreamEnd) -> Reconstruction {
        if self.runner == Runner::CursorAgent && self.builder.summary.response_model.is_none() {
            self.builder.summary.response_model = self.cursor_model.take();
        }
        self.builder.finish(ended)
    }
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

/// A claude-code turn is every `assistant` event sharing one `message.id`; it starts
/// when the event before it arrived, which is when the request that produced it went out.
fn claude_code(builder: &mut StepBuilder, event: &Value, at: SystemTime, content: ToolContent) {
    match event_type(event) {
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
            for block in content_blocks(event) {
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
            for block in content_blocks(event) {
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
            builder.read_result(event);
        }
        _ => builder.close_turn(None),
    }
    builder.arrived(at);
}

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
fn codex(builder: &mut StepBuilder, turn: &mut Option<CodexTurn>, event: &Value, at: SystemTime, content: ToolContent) {
    match event_type(event) {
        Some("turn.started") => *turn = Some(CodexTurn::start(builder, at)),
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
                return;
            };
            let (Some(call_id), Some(name)) = (string_at(item, "id"), codex_tool_name(item)) else {
                builder.arrived(at);
                return;
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
fn cursor_agent(
    builder: &mut StepBuilder,
    model: &mut Option<String>,
    event: &Value,
    at: SystemTime,
    content: ToolContent,
) {
    match event_type(event) {
        Some("system") => {
            if let Some(init_model) = string_at(event, "model") {
                *model = Some(init_model);
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
                turn.response_model.clone_from(model);
            }
        }
        Some("tool_call") => {
            builder.close_turn(None);
            if let (Some(call_id), Some((name, call))) = (cursor_call_id(event), cursor_tool(event)) {
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
            builder.read_result(event);
        }
        Some("thinking") => {}
        _ => builder.close_turn(None),
    }
    builder.arrived(at);
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
#[path = "harness_spans_tests.rs"]
mod tests;
