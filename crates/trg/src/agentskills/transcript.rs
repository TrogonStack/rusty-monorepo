use std::fmt;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::prompt::{SKILL_LINK_OLD, SKILL_LINK_WITH};
use super::redact::RedactedTranscript;

pub const NORMALIZED_TRANSCRIPT_SCHEMA_VERSION: &str = "trg.skills-eval.transcript.v1";
pub const NORMALIZED_TRANSCRIPT_FILE: &str = "events.json";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ToolName(String);

impl ToolName {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return None;
        }
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn eq_ignore_case(&self, other: &str) -> bool {
        self.0.eq_ignore_ascii_case(other)
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TranscriptEvent {
    AssistantText {
        text: String,
    },
    ToolCall {
        tool: ToolName,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
    },
    Terminal {
        ok: bool,
    },
}

/// The directory a run is allowed to touch.
///
/// trg invokes third-party runner CLIs as they ship, so it cannot confine their
/// reads. The boundary exists to report what a run touched outside its
/// workspace, never to prevent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBoundary {
    roots: Vec<PathBuf>,
}

impl WorkspaceBoundary {
    pub fn at(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let mut roots = vec![root.to_path_buf()];
        match std::fs::canonicalize(root) {
            Ok(canonical) if canonical != root => roots.push(canonical),
            _ => {}
        }
        Self { roots }
    }

    /// No boundary is known, so nothing can be reported as outside it.
    pub fn unknown() -> Self {
        Self { roots: Vec::new() }
    }

    pub fn contains(&self, candidate: &str) -> bool {
        if self.roots.is_empty() {
            return true;
        }
        let Some(candidate) = host_path(candidate, home_dir()) else {
            return false;
        };
        self.roots
            .iter()
            .any(|root| lexical_resolve(root, &candidate).starts_with(root))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| !home.as_os_str().is_empty())
}

/// The path a named argument refers to on the host, or `None` when it names a
/// home directory that cannot be located.
///
/// A leading `~` names the host home directory, so joining it onto the workspace
/// root would read `~/.cursor/plugins/x/SKILL.md` as a file inside the workspace
/// and hide the very escape this boundary exists to report. A home that cannot be
/// located is not inside the workspace either, so it stays outside the boundary
/// rather than resolving to it.
fn host_path(candidate: &str, home: Option<PathBuf>) -> Option<PathBuf> {
    let Some(rest) = candidate.strip_prefix('~') else {
        return Some(PathBuf::from(candidate));
    };
    match rest.strip_prefix('/') {
        Some(tail) => Some(home?.join(tail)),
        None if rest.is_empty() => home,
        // `~other/...` names another user's home, which is never the workspace.
        None => None,
    }
}

/// A path a run named that resolves outside the workspace it was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceEscape {
    pub tool: ToolName,
    pub path: String,
}

/// Resolves `..` and `.` without touching the filesystem, since an escaping path
/// may not exist by the time the transcript is read.
fn lexical_resolve(root: &Path, candidate: &Path) -> PathBuf {
    let mut resolved = if candidate.is_absolute() {
        PathBuf::new()
    } else {
        root.to_path_buf()
    };
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// Whether a harness exposes its tool calls in a form we can parse today.
///
/// `Unavailable` is deliberately distinct from "no tools were used": a grader that
/// depends on tool observation must report itself unsupported rather than silently
/// pass or fail when the harness cannot be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolVisibility {
    Observed,
    Unavailable,
}

impl ToolVisibility {
    pub fn is_observed(self) -> bool {
        matches!(self, Self::Observed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NormalizedTranscript {
    pub schema_version: String,
    pub runner: String,
    pub tool_visibility: ToolVisibility,
    pub events: Vec<TranscriptEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_escapes: Vec<WorkspaceEscape>,
}

impl NormalizedTranscript {
    pub fn new(runner: impl Into<String>, tool_visibility: ToolVisibility, events: Vec<TranscriptEvent>) -> Self {
        Self {
            schema_version: NORMALIZED_TRANSCRIPT_SCHEMA_VERSION.to_string(),
            runner: runner.into(),
            tool_visibility,
            events,
            workspace_escapes: Vec::new(),
        }
    }

    pub fn escaped_workspace(&self) -> bool {
        !self.workspace_escapes.is_empty()
    }

    pub fn unavailable(runner: impl Into<String>) -> Self {
        Self::new(runner, ToolVisibility::Unavailable, Vec::new())
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolName> {
        self.events.iter().filter_map(|event| match event {
            TranscriptEvent::ToolCall { tool, .. } => Some(tool),
            _ => None,
        })
    }

    pub fn tool_call_count(&self, name: &str) -> usize {
        self.tool_calls().filter(|tool| tool.eq_ignore_case(name)).count()
    }

    pub fn tool_sequence(&self) -> Vec<&ToolName> {
        self.tool_calls().collect()
    }

    /// How the run engaged the staged skill, if it can be determined at all.
    ///
    /// Two signals are harness-agnostic. A harness may expose a first-class skill
    /// tool, and any harness that reports tool inputs reveals engagement when a call
    /// references the path we staged the skill at.
    pub fn skill_engagement(&self) -> SkillEngagement {
        if !self.tool_visibility.is_observed() {
            return SkillEngagement::NotObservable;
        }
        for event in &self.events {
            let TranscriptEvent::ToolCall { tool, paths } = event else {
                continue;
            };
            if tool.eq_ignore_case("Skill") {
                return SkillEngagement::NativeSkillTool;
            }
            if paths.iter().any(|path| references_staged_skill(path)) {
                return SkillEngagement::StagedPathReference;
            }
        }
        SkillEngagement::NotEngaged
    }
}

fn references_staged_skill(path: &str) -> bool {
    path.contains(SKILL_LINK_WITH) || path.contains(SKILL_LINK_OLD)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillEngagement {
    NativeSkillTool,
    StagedPathReference,
    NotEngaged,
    NotObservable,
}

impl SkillEngagement {
    pub fn describe(self) -> &'static str {
        match self {
            Self::NativeSkillTool => "the runner invoked its native skill tool",
            Self::StagedPathReference => "the transcript references the staged skill directory",
            Self::NotEngaged => "no skill invocation or staged skill path appears in the transcript",
            Self::NotObservable => "this runner does not report skill engagement",
        }
    }

    pub fn engaged(self) -> Option<bool> {
        match self {
            Self::NativeSkillTool | Self::StagedPathReference => Some(true),
            Self::NotEngaged => Some(false),
            Self::NotObservable => None,
        }
    }
}

/// The native event vocabulary of a runner's streaming output.
///
/// Each variant names only events this repository has verified against the
/// runner's own output. Tool names stay in each harness's own vocabulary, since
/// renaming a shell invocation into an Anthropic tool name would assert an
/// equivalence the harness never claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    AnthropicStreamJson,
    CursorStreamJson,
    CodexThreadJsonl,
}

impl TranscriptFormat {
    pub fn tool_visibility(self) -> ToolVisibility {
        match self {
            Self::AnthropicStreamJson | Self::CursorStreamJson | Self::CodexThreadJsonl => ToolVisibility::Observed,
        }
    }

    /// Normalizing takes the redacted transcript rather than the runner's raw
    /// bytes, so `events.json` cannot carry a secret that `transcript.jsonl`
    /// already had stripped.
    pub fn normalize(
        self,
        runner: &str,
        stdout: &RedactedTranscript,
        boundary: &WorkspaceBoundary,
    ) -> NormalizedTranscript {
        match self {
            Self::AnthropicStreamJson => normalize_stream_json(runner, stdout, boundary),
            Self::CursorStreamJson => normalize_cursor_stream_json(runner, stdout, boundary),
            Self::CodexThreadJsonl => normalize_codex_thread_jsonl(runner, stdout, boundary),
        }
    }
}

fn terminal_ok_flag(value: &serde_json::Value) -> bool {
    !value.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Accumulates normalized events and, in the same pass, the paths that left the
/// workspace, so the escape report is derived from the same reading of the stream
/// that the events are.
struct TranscriptBuilder<'a> {
    boundary: &'a WorkspaceBoundary,
    events: Vec<TranscriptEvent>,
    workspace_escapes: Vec<WorkspaceEscape>,
}

impl<'a> TranscriptBuilder<'a> {
    fn new(boundary: &'a WorkspaceBoundary) -> Self {
        Self {
            boundary,
            events: Vec::new(),
            workspace_escapes: Vec::new(),
        }
    }

    fn assistant_text(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.events
            .push(TranscriptEvent::AssistantText { text: text.to_string() });
    }

    fn tool_call(&mut self, tool: ToolName, arguments: ToolArguments) {
        let named = arguments
            .paths
            .iter()
            .cloned()
            .chain(arguments.commands.iter().flat_map(|command| command_operands(command)));
        for path in named {
            if !self.boundary.contains(&path) {
                self.workspace_escapes.push(WorkspaceEscape {
                    tool: tool.clone(),
                    path,
                });
            }
        }
        self.events.push(TranscriptEvent::ToolCall {
            tool,
            paths: arguments.into_event_paths(),
        });
    }

    fn terminal(&mut self, ok: bool) {
        self.events.push(TranscriptEvent::Terminal { ok });
    }

    fn finish(self, runner: &str, tool_visibility: ToolVisibility) -> NormalizedTranscript {
        let mut transcript = NormalizedTranscript::new(runner, tool_visibility, self.events);
        transcript.workspace_escapes = self.workspace_escapes;
        transcript
    }
}

/// The arguments of one tool call, split by what each one can be read as.
///
/// `paths` name a file outright. `commands` name a program and its operands, so a
/// path can be recovered from them without being asserted. Everything else, such
/// as a search pattern, may mention a path without naming one and stays in
/// `texts`, where skill-engagement detection can still read it.
#[derive(Debug, Default)]
struct ToolArguments {
    paths: Vec<String>,
    commands: Vec<String>,
    texts: Vec<String>,
}

impl ToolArguments {
    fn from_object(
        input: Option<&serde_json::Value>,
        path_keys: &[&str],
        command_keys: &[&str],
        text_keys: &[&str],
    ) -> Self {
        let Some(object) = input.and_then(|value| value.as_object()) else {
            return Self::default();
        };
        let collect = |keys: &[&str]| -> Vec<String> {
            keys.iter()
                .filter_map(|key| object.get(*key))
                .filter_map(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .collect()
        };
        Self {
            paths: collect(path_keys),
            commands: collect(command_keys),
            texts: collect(text_keys),
        }
    }

    fn from_paths(paths: Vec<String>) -> Self {
        Self {
            paths,
            ..Self::default()
        }
    }

    fn from_command(command: &str) -> Self {
        Self {
            commands: vec![command.to_string()],
            ..Self::default()
        }
    }

    fn into_event_paths(self) -> Vec<String> {
        let mut all = self.paths;
        all.extend(self.commands);
        all.extend(self.texts);
        all
    }
}

/// The operands of a command that name a path on the host.
///
/// This is not a shell parse. The program is skipped, since `/bin/zsh -lc '...'`
/// names an interpreter rather than a file the run reached for, and the payload of
/// an interpreter flag is scanned as the nested command it is. Only a token
/// beginning with `/`, `~`, or `..` can be read as naming a path without guessing,
/// which is enough for the escapes that matter here: a run that reaches a skill or
/// a host file through a shell instead of a file tool.
fn command_operands(command: &str) -> Vec<String> {
    const INTERPRETER_FLAGS: &[&str] = &["-c", "-lc", "-ic", "-lic", "-li", "-l"];

    let mut operands = Vec::new();
    let mut skip_program = true;
    for token in command.split_whitespace() {
        let token = token.trim_matches(|c| c == '\'' || c == '"' || c == '`');
        if token.is_empty() {
            continue;
        }
        if std::mem::take(&mut skip_program) {
            continue;
        }
        if INTERPRETER_FLAGS.contains(&token) {
            skip_program = true;
            continue;
        }
        if token.starts_with('/') || token.starts_with('~') || token.starts_with("..") {
            operands.push(token.to_string());
        }
    }
    operands
}

pub fn ndjson_values(stdout: &RedactedTranscript) -> Vec<serde_json::Value> {
    stdout
        .as_str()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn content_blocks(value: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    value
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
}

/// Normalizes the Anthropic `stream-json` event family.
///
/// Only the shapes this repository already relies on are recognized. Anything
/// unrecognized is skipped rather than guessed at.
pub fn normalize_stream_json(
    runner: &str,
    stdout: &RedactedTranscript,
    boundary: &WorkspaceBoundary,
) -> NormalizedTranscript {
    const PATH_KEYS: &[&str] = &["file_path", "path", "notebook_path"];
    const COMMAND_KEYS: &[&str] = &["command"];
    const TEXT_KEYS: &[&str] = &["pattern", "skill"];

    let mut builder = TranscriptBuilder::new(boundary);

    for value in ndjson_values(stdout) {
        match value.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                let Some(blocks) = content_blocks(&value) else { continue };
                for block in blocks {
                    match block.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                builder.assistant_text(text);
                            }
                        }
                        Some("tool_use") => {
                            let Some(tool) = block.get("name").and_then(|v| v.as_str()).and_then(ToolName::new) else {
                                continue;
                            };
                            builder.tool_call(
                                tool,
                                ToolArguments::from_object(block.get("input"), PATH_KEYS, COMMAND_KEYS, TEXT_KEYS),
                            );
                        }
                        _ => {}
                    }
                }
            }
            Some("result") => builder.terminal(terminal_ok_flag(&value)),
            _ => {}
        }
    }

    builder.finish(runner, ToolVisibility::Observed)
}

/// Normalizes the cursor-agent `stream-json` event family.
///
/// A `tool_call` event carries exactly one `<name>ToolCall` member, and that
/// member name is the tool's own name. Only the `started` subtype is read, since
/// `completed` repeats the same call with its result attached.
pub fn normalize_cursor_stream_json(
    runner: &str,
    stdout: &RedactedTranscript,
    boundary: &WorkspaceBoundary,
) -> NormalizedTranscript {
    const CALL_SUFFIX: &str = "ToolCall";
    const PATH_KEYS: &[&str] = &["path", "targetDirectory"];
    const COMMAND_KEYS: &[&str] = &["command"];
    const TEXT_KEYS: &[&str] = &["globPattern", "pattern"];

    let mut builder = TranscriptBuilder::new(boundary);

    for value in ndjson_values(stdout) {
        match value.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                let Some(blocks) = content_blocks(&value) else { continue };
                for block in blocks {
                    if block.get("type").and_then(|v| v.as_str()) != Some("text") {
                        continue;
                    }
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        builder.assistant_text(text);
                    }
                }
            }
            Some("tool_call") => {
                if value.get("subtype").and_then(|v| v.as_str()) != Some("started") {
                    continue;
                }
                let Some(call) = value.get("tool_call").and_then(|v| v.as_object()) else {
                    continue;
                };
                let Some((member, body)) = call.iter().find(|(member, _)| member.ends_with(CALL_SUFFIX)) else {
                    continue;
                };
                let Some(tool) = ToolName::new(member.trim_end_matches(CALL_SUFFIX)) else {
                    continue;
                };
                builder.tool_call(
                    tool,
                    ToolArguments::from_object(body.get("args"), PATH_KEYS, COMMAND_KEYS, TEXT_KEYS),
                );
            }
            Some("result") => builder.terminal(terminal_ok_flag(&value)),
            _ => {}
        }
    }

    builder.finish(runner, ToolVisibility::Observed)
}

/// Normalizes the codex `--json` thread event family.
///
/// codex reaches the filesystem through a shell rather than through named file
/// tools, so its tool vocabulary is `command_execution` and `file_change`. Only
/// `file_change` names paths the boundary can be checked against; a command is
/// kept as text.
pub fn normalize_codex_thread_jsonl(
    runner: &str,
    stdout: &RedactedTranscript,
    boundary: &WorkspaceBoundary,
) -> NormalizedTranscript {
    let mut builder = TranscriptBuilder::new(boundary);

    for value in ndjson_values(stdout) {
        let event = value.get("type").and_then(|v| v.as_str());
        if event == Some("turn.completed") {
            builder.terminal(true);
            continue;
        }
        let Some(item) = value.get("item") else { continue };
        let item_type = item.get("type").and_then(|v| v.as_str());
        match (event, item_type) {
            (Some("item.completed"), Some("agent_message")) => {
                if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                    builder.assistant_text(text);
                }
            }
            (Some("item.started"), Some("command_execution")) => {
                let Some(command) = item.get("command").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(tool) = ToolName::new("command_execution") else {
                    continue;
                };
                builder.tool_call(tool, ToolArguments::from_command(command));
            }
            (Some("item.started"), Some("file_change")) => {
                let paths = item
                    .get("changes")
                    .and_then(|changes| changes.as_array())
                    .map(|changes| {
                        changes
                            .iter()
                            .filter_map(|change| change.get("path"))
                            .filter_map(|path| path.as_str())
                            .filter(|path| !path.trim().is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let Some(tool) = ToolName::new("file_change") else {
                    continue;
                };
                builder.tool_call(tool, ToolArguments::from_paths(paths));
            }
            _ => {}
        }
    }

    builder.finish(runner, ToolVisibility::Observed)
}

pub fn normalized_transcript_path(transcript_path: &std::path::Path) -> std::path::PathBuf {
    transcript_path
        .parent()
        .map(|parent| parent.join(NORMALIZED_TRANSCRIPT_FILE))
        .unwrap_or_else(|| std::path::PathBuf::from(NORMALIZED_TRANSCRIPT_FILE))
}

pub fn write_normalized_transcript(
    transcript_path: &std::path::Path,
    transcript: &NormalizedTranscript,
) -> std::io::Result<()> {
    let path = normalized_transcript_path(transcript_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(transcript)?)
}

pub fn read_normalized_transcript(transcript_path: &std::path::Path) -> std::io::Result<NormalizedTranscript> {
    let path = normalized_transcript_path(transcript_path);
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::redact::redact_transcript_bytes;

    const CURSOR_STREAM: &[u8] = br#"{"type":"system","subtype":"init","cwd":"/w"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I'll read the skill first."}]}}
{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":".skill/SKILL.md"}},"toolCallId":"call-1","startedAtMs":"1"}}
{"type":"thinking","text":"..."}
{"type":"tool_call","subtype":"started","tool_call":{"globToolCall":{"args":{"targetDirectory":"/w","globPattern":"**/*.csv"}},"toolCallId":"call-2","startedAtMs":"2"}}
{"type":"tool_call","subtype":"completed","tool_call":{"globToolCall":{"args":{"targetDirectory":"/w","globPattern":"**/*.csv"},"result":{"success":{"files":[]}}},"toolCallId":"call-2","startedAtMs":"2","completedAtMs":"3"}}
{"type":"tool_call","subtype":"started","tool_call":{"editToolCall":{"args":{"path":"outputs/summary.md","streamContent":"Report line\n"}},"toolCallId":"call-3","startedAtMs":"4"}}
{"type":"result","is_error":false,"result":"done"}
"#;

    const CODEX_THREAD: &[u8] = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I'll read the staged skill first."}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"sed -n '1,240p' .skill/SKILL.md\"","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"sed -n '1,240p' .skill/SKILL.md\"","exit_code":0,"status":"completed"}}
{"type":"item.started","item":{"id":"item_8","type":"file_change","changes":[{"path":"outputs/summary.md","kind":"add"}],"status":"in_progress"}}
{"type":"turn.completed","usage":{"input_tokens":229974,"output_tokens":1877}}
"#;

    const CLAUDE_STREAM: &[u8] = br#"{"type":"system","subtype":"init"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Let me check the skill."},{"type":"tool_use","name":"Read","input":{"file_path":".skill/SKILL.md"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/report.md"}}]}}
{"type":"result","is_error":false,"result":"done"}
"#;

    #[test]
    fn tool_name_rejects_blank() {
        assert!(ToolName::new("  ").is_none());
        assert_eq!(ToolName::new("Read").unwrap().as_str(), "Read");
    }

    #[test]
    fn normalizes_claude_stream_into_ordered_events() {
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(CLAUDE_STREAM),
            &WorkspaceBoundary::unknown(),
        );

        assert_eq!(transcript.tool_visibility, ToolVisibility::Observed);
        assert_eq!(
            transcript
                .tool_sequence()
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>(),
            vec!["Read", "Write"]
        );
        assert_eq!(transcript.tool_call_count("read"), 1);
        assert_eq!(transcript.tool_call_count("Bash"), 0);
        assert!(matches!(
            transcript.events.last(),
            Some(TranscriptEvent::Terminal { ok: true })
        ));
    }

    #[test]
    fn staged_skill_path_counts_as_engagement() {
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(CLAUDE_STREAM),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
        assert_eq!(transcript.skill_engagement().engaged(), Some(true));
    }

    #[test]
    fn native_skill_tool_counts_as_engagement() {
        let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"demo-skill"}}]}}
"#;
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(transcript.skill_engagement(), SkillEngagement::NativeSkillTool);
    }

    #[test]
    fn tools_without_skill_reference_are_not_engagement() {
        let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/report.md"}}]}}
"#;
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
        assert_eq!(transcript.skill_engagement().engaged(), Some(false));
    }

    #[test]
    fn unobservable_harness_reports_none_rather_than_not_engaged() {
        let transcript = NormalizedTranscript::unavailable("codex");
        assert_eq!(transcript.skill_engagement(), SkillEngagement::NotObservable);
        assert_eq!(transcript.skill_engagement().engaged(), None);
    }

    #[test]
    fn malformed_and_unknown_lines_are_skipped() {
        let stdout = br#"not json at all
{"type":"mystery_future_event","payload":1}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":".skill/SKILL.md"}}]}}
"#;
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(transcript.events.len(), 1);
        assert_eq!(transcript.tool_call_count("Read"), 1);
    }

    #[test]
    fn error_terminal_is_recorded_as_not_ok() {
        let stdout = br#"{"type":"result","is_error":true,"result":"boom"}
"#;
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::unknown(),
        );
        assert!(matches!(
            transcript.events.first(),
            Some(TranscriptEvent::Terminal { ok: false })
        ));
    }

    #[test]
    fn codex_thread_events_are_normalized_in_the_shell_vocabulary_codex_uses() {
        let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
            "codex",
            &redact_transcript_bytes(CODEX_THREAD),
            &WorkspaceBoundary::unknown(),
        );

        assert_eq!(transcript.tool_visibility, ToolVisibility::Observed);
        assert_eq!(
            transcript
                .tool_sequence()
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>(),
            vec!["command_execution", "file_change"]
        );
        assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
        assert!(matches!(
            transcript.events.first(),
            Some(TranscriptEvent::AssistantText { .. })
        ));
        assert!(matches!(
            transcript.events.last(),
            Some(TranscriptEvent::Terminal { ok: true })
        ));
    }

    #[test]
    fn a_codex_turn_without_items_still_reports_its_terminal_event() {
        let stdout = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40}}
"#;
        let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
            "codex",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(transcript.events, vec![TranscriptEvent::Terminal { ok: true }]);
        assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
    }

    #[test]
    fn cursor_tool_calls_are_normalized_from_their_member_name() {
        let transcript = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            &redact_transcript_bytes(CURSOR_STREAM),
            &WorkspaceBoundary::unknown(),
        );

        assert_eq!(transcript.tool_visibility, ToolVisibility::Observed);
        assert_eq!(
            transcript
                .tool_sequence()
                .iter()
                .map(|t| t.as_str())
                .collect::<Vec<_>>(),
            vec!["read", "glob", "edit"]
        );
        assert_eq!(transcript.tool_call_count("Read"), 1);
        assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
        assert!(matches!(
            transcript.events.first(),
            Some(TranscriptEvent::AssistantText { .. })
        ));
    }

    #[test]
    fn a_completed_cursor_tool_call_does_not_double_count_the_started_one() {
        let stdout = br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":".skill/SKILL.md"}}}}
{"type":"tool_call","subtype":"completed","tool_call":{"readToolCall":{"args":{"path":".skill/SKILL.md"},"result":{"success":{"content":"..."}}}}}
"#;
        let transcript = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(transcript.tool_call_count("read"), 1);
    }

    #[test]
    fn a_read_outside_the_workspace_is_reported_against_the_tool_that_made_it() {
        let workspace = tempfile::tempdir().unwrap();
        let stdout =
            br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"/etc/hosts"}}}}
{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"outputs/summary.md"}}}}
{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"../../elsewhere/secret.txt"}}}}
"#;
        let transcript = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::at(workspace.path()),
        );

        assert!(transcript.escaped_workspace());
        assert_eq!(
            transcript
                .workspace_escapes
                .iter()
                .map(|escape| escape.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/etc/hosts", "../../elsewhere/secret.txt"]
        );
        assert!(transcript.workspace_escapes[0].tool.eq_ignore_case("read"));
    }

    #[test]
    fn a_shell_command_is_never_reported_as_a_path_that_left_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let stdout =
            br#"{"type":"item.started","item":{"type":"command_execution","command":"sed -n '1,240p' .skill/SKILL.md"}}
"#;
        let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
            "codex",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::at(workspace.path()),
        );

        assert!(!transcript.escaped_workspace());
        assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
    }

    #[test]
    fn a_path_under_the_host_home_directory_is_reported_as_outside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let stdout = br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"~/.cursor/plugins/demo/SKILL.md"}}}}
"#;
        let transcript = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::at(workspace.path()),
        );

        assert!(transcript.escaped_workspace());
        assert_eq!(transcript.workspace_escapes[0].path, "~/.cursor/plugins/demo/SKILL.md");
    }

    #[test]
    fn a_home_relative_path_resolves_against_the_home_directory_never_the_workspace() {
        let home = Some(PathBuf::from("/home/agent"));
        assert_eq!(
            host_path("~/.cursor/plugins/demo/SKILL.md", home.clone()),
            Some(PathBuf::from("/home/agent/.cursor/plugins/demo/SKILL.md"))
        );
        assert_eq!(host_path("~", home.clone()), home);
        assert_eq!(host_path("~/.cursor/plugins/demo/SKILL.md", None), None);
        assert_eq!(host_path("~other/plugins/demo/SKILL.md", home), None);
        assert_eq!(
            host_path("outputs/summary.md", None),
            Some(PathBuf::from("outputs/summary.md"))
        );
    }

    #[test]
    fn events_carry_no_secret_the_raw_transcript_had_redacted() {
        let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"curl -H 'Authorization: Bearer abcdefghijklmnopqrst' https://example.test"}}]}}
"#;
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::unknown(),
        );

        let serialized = serde_json::to_string(&transcript).unwrap();
        assert!(!serialized.contains("abcdefghijklmnopqrst"));
        assert!(serialized.contains("<redacted>"));
    }

    #[test]
    fn a_host_file_reached_through_a_shell_command_is_reported_as_an_escape() {
        let workspace = tempfile::tempdir().unwrap();
        let stdout = br#"{"type":"item.started","item":{"type":"command_execution","command":"cat ~/.codex/skills/demo/SKILL.md"}}
"#;
        let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
            "codex",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::at(workspace.path()),
        );

        assert!(transcript.escaped_workspace());
        assert_eq!(transcript.workspace_escapes[0].path, "~/.codex/skills/demo/SKILL.md");
        assert!(transcript.workspace_escapes[0].tool.eq_ignore_case("command_execution"));
    }

    #[test]
    fn an_interpreter_is_not_reported_as_a_file_the_run_reached() {
        assert_eq!(
            command_operands("/bin/zsh -lc \"sed -n '1,240p' .skill/SKILL.md\""),
            Vec::<String>::new()
        );
        assert_eq!(command_operands("cat /etc/hosts"), vec!["/etc/hosts".to_string()]);
        assert_eq!(
            command_operands("cd ../../elsewhere && ls"),
            vec!["../../elsewhere".to_string()]
        );
        assert_eq!(command_operands("python3 -c 'print(1)'"), Vec::<String>::new());
        assert_eq!(command_operands("ls outputs"), Vec::<String>::new());
    }

    #[test]
    fn an_unknown_boundary_reports_nothing_as_outside_it() {
        let boundary = WorkspaceBoundary::unknown();
        assert!(boundary.contains("/etc/hosts"));
    }

    #[test]
    fn cursor_terminal_event_carries_its_error_flag() {
        let ok = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            &redact_transcript_bytes(
                br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"result":"hello"}
"#,
            ),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(ok.events, vec![TranscriptEvent::Terminal { ok: true }]);

        let failed = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            &redact_transcript_bytes(br#"{"type":"result","is_error":true,"result":"boom"}"#),
            &WorkspaceBoundary::unknown(),
        );
        assert_eq!(failed.events, vec![TranscriptEvent::Terminal { ok: false }]);
    }

    #[test]
    fn normalized_transcript_is_persisted_beside_the_raw_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(CLAUDE_STREAM),
            &WorkspaceBoundary::unknown(),
        );

        write_normalized_transcript(&transcript_path, &transcript).unwrap();

        assert_eq!(
            normalized_transcript_path(&transcript_path),
            dir.path().join(NORMALIZED_TRANSCRIPT_FILE)
        );
        assert_eq!(read_normalized_transcript(&transcript_path).unwrap(), transcript);
    }

    #[test]
    fn normalized_transcript_round_trips_as_json() {
        let transcript = normalize_stream_json(
            "claude",
            &redact_transcript_bytes(CLAUDE_STREAM),
            &WorkspaceBoundary::unknown(),
        );
        let json = serde_json::to_string(&transcript).unwrap();
        let parsed: NormalizedTranscript = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, transcript);
        assert_eq!(parsed.schema_version, NORMALIZED_TRANSCRIPT_SCHEMA_VERSION);
    }
}
