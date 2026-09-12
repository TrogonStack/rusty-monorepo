use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::prompt::{SKILL_LINK_OLD, SKILL_LINK_WITH};

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
}

impl NormalizedTranscript {
    pub fn new(runner: impl Into<String>, tool_visibility: ToolVisibility, events: Vec<TranscriptEvent>) -> Self {
        Self {
            schema_version: NORMALIZED_TRANSCRIPT_SCHEMA_VERSION.to_string(),
            runner: runner.into(),
            tool_visibility,
            events,
        }
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
/// runner's own output. Tool-call events are normalized for the Anthropic
/// vocabulary alone; the other two report `ToolVisibility::Unavailable` so a
/// tool-dependent grader is told it cannot be answered here rather than being
/// given a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFormat {
    AnthropicStreamJson,
    CursorStreamJson,
    CodexThreadJsonl,
}

impl TranscriptFormat {
    pub fn tool_visibility(self) -> ToolVisibility {
        match self {
            Self::AnthropicStreamJson => ToolVisibility::Observed,
            Self::CursorStreamJson | Self::CodexThreadJsonl => ToolVisibility::Unavailable,
        }
    }

    pub fn normalize(self, runner: &str, stdout: &[u8]) -> NormalizedTranscript {
        match self {
            Self::AnthropicStreamJson => normalize_stream_json(runner, stdout),
            Self::CursorStreamJson => normalize_terminal_only(runner, self, stdout, "result", is_error_flag),
            Self::CodexThreadJsonl => normalize_terminal_only(runner, self, stdout, "turn.completed", |_| true),
        }
    }
}

fn is_error_flag(value: &serde_json::Value) -> bool {
    !value.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn normalize_terminal_only(
    runner: &str,
    format: TranscriptFormat,
    stdout: &[u8],
    terminal_type: &str,
    terminal_ok: fn(&serde_json::Value) -> bool,
) -> NormalizedTranscript {
    let events = ndjson_values(stdout)
        .iter()
        .filter(|value| value.get("type").and_then(|v| v.as_str()) == Some(terminal_type))
        .map(|value| TranscriptEvent::Terminal { ok: terminal_ok(value) })
        .collect();
    NormalizedTranscript::new(runner, format.tool_visibility(), events)
}

pub fn ndjson_values(stdout: &[u8]) -> Vec<serde_json::Value> {
    let Ok(text) = std::str::from_utf8(stdout) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Normalizes the Anthropic `stream-json` event family.
///
/// Only the shapes this repository already relies on are recognized. Anything
/// unrecognized is skipped rather than guessed at.
pub fn normalize_stream_json(runner: &str, stdout: &[u8]) -> NormalizedTranscript {
    let mut events = Vec::new();

    for value in ndjson_values(stdout) {
        match value.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                let blocks = value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(|content| content.as_array());
                let Some(blocks) = blocks else { continue };
                for block in blocks {
                    match block.get("type").and_then(|v| v.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                                if !text.trim().is_empty() {
                                    events.push(TranscriptEvent::AssistantText { text: text.to_string() });
                                }
                            }
                        }
                        Some("tool_use") => {
                            let Some(tool) = block.get("name").and_then(|v| v.as_str()).and_then(ToolName::new) else {
                                continue;
                            };
                            let paths = collect_path_like(block.get("input"));
                            events.push(TranscriptEvent::ToolCall { tool, paths });
                        }
                        _ => {}
                    }
                }
            }
            Some("result") => {
                let ok = !value.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                events.push(TranscriptEvent::Terminal { ok });
            }
            _ => {}
        }
    }

    NormalizedTranscript::new(runner, ToolVisibility::Observed, events)
}

fn collect_path_like(input: Option<&serde_json::Value>) -> Vec<String> {
    const PATH_KEYS: &[&str] = &["file_path", "path", "notebook_path", "command", "pattern", "skill"];

    let Some(object) = input.and_then(|value| value.as_object()) else {
        return Vec::new();
    };
    PATH_KEYS
        .iter()
        .filter_map(|key| object.get(*key))
        .filter_map(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .collect()
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
        let transcript = normalize_stream_json("claude", CLAUDE_STREAM);

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
        let transcript = normalize_stream_json("claude", CLAUDE_STREAM);
        assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
        assert_eq!(transcript.skill_engagement().engaged(), Some(true));
    }

    #[test]
    fn native_skill_tool_counts_as_engagement() {
        let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"demo-skill"}}]}}
"#;
        let transcript = normalize_stream_json("claude", stdout);
        assert_eq!(transcript.skill_engagement(), SkillEngagement::NativeSkillTool);
    }

    #[test]
    fn tools_without_skill_reference_are_not_engagement() {
        let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/report.md"}}]}}
"#;
        let transcript = normalize_stream_json("claude", stdout);
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
        let transcript = normalize_stream_json("claude", stdout);
        assert_eq!(transcript.events.len(), 1);
        assert_eq!(transcript.tool_call_count("Read"), 1);
    }

    #[test]
    fn error_terminal_is_recorded_as_not_ok() {
        let stdout = br#"{"type":"result","is_error":true,"result":"boom"}
"#;
        let transcript = normalize_stream_json("claude", stdout);
        assert!(matches!(
            transcript.events.first(),
            Some(TranscriptEvent::Terminal { ok: false })
        ));
    }

    #[test]
    fn codex_terminal_event_is_normalized_without_claiming_tool_visibility() {
        let stdout = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40}}
"#;
        let transcript = TranscriptFormat::CodexThreadJsonl.normalize("codex", stdout);
        assert_eq!(transcript.tool_visibility, ToolVisibility::Unavailable);
        assert_eq!(transcript.events, vec![TranscriptEvent::Terminal { ok: true }]);
        assert_eq!(transcript.skill_engagement(), SkillEngagement::NotObservable);
    }

    #[test]
    fn cursor_terminal_event_carries_its_error_flag() {
        let ok = TranscriptFormat::CursorStreamJson.normalize(
            "cursor-agent",
            br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"result":"hello"}
"#,
        );
        assert_eq!(ok.events, vec![TranscriptEvent::Terminal { ok: true }]);

        let failed = TranscriptFormat::CursorStreamJson
            .normalize("cursor-agent", br#"{"type":"result","is_error":true,"result":"boom"}"#);
        assert_eq!(failed.events, vec![TranscriptEvent::Terminal { ok: false }]);
        assert_eq!(failed.tool_visibility, ToolVisibility::Unavailable);
    }

    #[test]
    fn normalized_transcript_is_persisted_beside_the_raw_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let transcript_path = dir.path().join("transcript.jsonl");
        let transcript = normalize_stream_json("claude", CLAUDE_STREAM);

        write_normalized_transcript(&transcript_path, &transcript).unwrap();

        assert_eq!(
            normalized_transcript_path(&transcript_path),
            dir.path().join(NORMALIZED_TRANSCRIPT_FILE)
        );
        assert_eq!(read_normalized_transcript(&transcript_path).unwrap(), transcript);
    }

    #[test]
    fn normalized_transcript_round_trips_as_json() {
        let transcript = normalize_stream_json("claude", CLAUDE_STREAM);
        let json = serde_json::to_string(&transcript).unwrap();
        let parsed: NormalizedTranscript = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, transcript);
        assert_eq!(parsed.schema_version, NORMALIZED_TRANSCRIPT_SCHEMA_VERSION);
    }
}
