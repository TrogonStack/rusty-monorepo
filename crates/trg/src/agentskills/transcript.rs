mod constants;
pub use constants::NORMALIZED_TRANSCRIPT_FILE;
use constants::{INTERPRETER_FLAGS, STAGED_SKILL_DIRS};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::prompt::{SkillName, StagedSkillDir};
use super::redact::RedactedTranscript;

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

/// Where the run that produced a transcript staged its own skill.
///
/// `STAGED_SKILL_DIRS` says where a run could stage a skill, not where this one
/// did, so reading a named path against all of them credits an arm that merely
/// looked at `skills/` with reaching for a skill it was never given, and the arm
/// that loses its reading is the control the comparison rests on. The run knows
/// what it staged, so it records it and each reader judges a path against that.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StagedSkill {
    /// A transcript from before a run recorded what it staged, which can only be
    /// read against every directory a run stages at.
    #[default]
    Unrecorded,
    /// The arm that runs without a skill, so no path it named is this run's.
    Nothing,
    At {
        directory: StagedSkillDir,
        #[serde(default, skip_serializing_if = "StagedSkillName::is_unrecorded")]
        name: StagedSkillName,
    },
}

/// What a run recorded about the name of the skill it staged.
///
/// A native skill tool call reports the skill it invoked by name, and the name is
/// the only thing in that call that tells this run's skill from one the harness
/// carries of its own. A transcript written before the name was recorded cannot
/// tell them apart, and being unable to say which skill was invoked is not
/// evidence that the wrong one was, so it keeps the reading it was written under.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StagedSkillName {
    #[default]
    Unrecorded,
    Known {
        name: SkillName,
    },
}

impl StagedSkillName {
    pub fn known(name: SkillName) -> Self {
        Self::Known { name }
    }

    fn recorded(&self) -> Option<&SkillName> {
        match self {
            Self::Unrecorded => None,
            Self::Known { name } => Some(name),
        }
    }

    fn is_unrecorded(&self) -> bool {
        matches!(self, Self::Unrecorded)
    }
}

impl StagedSkill {
    fn named_by(&self, token: &str) -> bool {
        match self {
            Self::Unrecorded => STAGED_SKILL_DIRS.iter().any(|directory| token.contains(directory)),
            Self::Nothing => false,
            Self::At { directory, .. } => token.contains(directory.as_str()),
        }
    }

    /// Is this workspace-relative path inside the skill this run staged?
    ///
    /// A transcript from before a run recorded its staging cannot say which
    /// directory it used, so it answers for all of them: crediting an agent
    /// with a file the harness may have planted is the error worth avoiding.
    pub fn planted(&self, relative: &str) -> bool {
        match self {
            Self::Unrecorded => STAGED_SKILL_DIRS
                .iter()
                .any(|directory| relative.starts_with(directory)),
            Self::Nothing => false,
            Self::At { directory, .. } => relative.starts_with(directory.as_str()),
        }
    }

    fn is_unrecorded(&self) -> bool {
        matches!(self, Self::Unrecorded)
    }

    fn stages_nothing(&self) -> bool {
        matches!(self, Self::Nothing)
    }

    fn recorded_name(&self) -> Option<&SkillName> {
        match self {
            Self::Unrecorded | Self::Nothing => None,
            Self::At { name, .. } => name.recorded(),
        }
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
    pub runner: String,
    pub tool_visibility: ToolVisibility,
    pub events: Vec<TranscriptEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_escapes: Vec<WorkspaceEscape>,
    #[serde(default, skip_serializing_if = "StagedSkill::is_unrecorded")]
    pub staged_skill: StagedSkill,
}

impl NormalizedTranscript {
    pub fn new(runner: impl Into<String>, tool_visibility: ToolVisibility, events: Vec<TranscriptEvent>) -> Self {
        Self {
            runner: runner.into(),
            tool_visibility,
            events,
            workspace_escapes: Vec::new(),
            staged_skill: StagedSkill::default(),
        }
    }

    /// What the run staged, which the transcript carries because grading reads the
    /// transcript back rather than the run that wrote it.
    pub fn staged_at(mut self, staged_skill: StagedSkill) -> Self {
        self.staged_skill = staged_skill;
        self
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

    /// Every call the run made, paired with the values that call named.
    ///
    /// The values are what a harness reported the call being given, not the JSON
    /// input it was given: the shape of that JSON belongs to one harness, so a
    /// reader written against its key names would answer nothing on another.
    pub fn tool_call_inputs(&self) -> impl Iterator<Item = (&ToolName, &[String])> {
        self.events.iter().filter_map(|event| match event {
            TranscriptEvent::ToolCall { tool, paths } => Some((tool, paths.as_slice())),
            _ => None,
        })
    }

    pub fn tool_sequence(&self) -> Vec<&ToolName> {
        self.tool_calls().collect()
    }

    /// How the run engaged the staged skill, if it can be determined at all.
    ///
    /// Two signals are harness-agnostic. A harness may expose a first-class skill
    /// tool that names the skill it invoked, and any harness that reports tool inputs
    /// reveals engagement when a call references the path we staged the skill at.
    ///
    /// Neither signal is read against what some run could have staged. A harness
    /// carries skills of its own, so the arm that stages no skill has none to engage
    /// and a run that invoked one installed on the host reached for a skill that was
    /// never under test. Reading either as engagement would cost the comparison its
    /// control arm.
    pub fn skill_engagement(&self) -> SkillEngagement {
        if !self.tool_visibility.is_observed() {
            return SkillEngagement::NotObservable;
        }
        for event in &self.events {
            let TranscriptEvent::ToolCall { tool, paths } = event else {
                continue;
            };
            if tool.eq_ignore_case("Skill") && self.invoked_the_staged_skill(paths) {
                return SkillEngagement::NativeSkillTool;
            }
            if paths.iter().any(|path| self.references_staged_skill(path)) {
                return SkillEngagement::StagedPathReference;
            }
        }
        SkillEngagement::NotEngaged
    }

    /// Whether a native skill tool call invoked the skill this run staged.
    ///
    /// A harness carries skills of its own, so the call names which one it reached
    /// for and nothing else in it does. Crediting every such call would score a run
    /// that used a skill installed on the host as having used the one it was given,
    /// and would leave a case that stages several skills with no way to ask which
    /// one the agent picked.
    ///
    /// A run that staged no skill has no name to match, and the arm that recorded
    /// no name cannot say which skill the call invoked, so each keeps the reading it
    /// already had rather than being turned into a run that engaged nothing.
    fn invoked_the_staged_skill(&self, named: &[String]) -> bool {
        let Some(name) = self.staged_skill.recorded_name() else {
            return !self.staged_skill.stages_nothing();
        };
        named
            .iter()
            .flat_map(|value| self.tokens_worth_reading(value))
            .any(|token| name.named_by(&token))
    }

    /// Whether what the run named is the skill this run staged.
    ///
    /// An unannounced case stages under a plain directory name that also appears
    /// in the harness's own skill install paths, so a path that left the
    /// workspace is somebody else's skill and never this run's, and a path inside
    /// the workspace is read against the one directory this run staged at rather
    /// than against every directory some run could stage at.
    ///
    /// A token is still only the text a tool call carried. A transcript does not
    /// say which directory a command ran in, so a relative path reached after a
    /// `cd`, or a search pattern quoting the staged directory, reads as a path to
    /// it. Resolving the text against the workspace root would not tell those
    /// apart either, since what is missing is where the run stood.
    fn references_staged_skill(&self, named: &str) -> bool {
        self.tokens_worth_reading(named)
            .iter()
            .any(|token| self.staged_skill.named_by(token))
    }

    /// The tokens of a named argument that could still be this run's own.
    ///
    /// A command-style tool records its whole command line, and one command line
    /// can name this run's skill and a host one at once, so each token is judged
    /// on its own. Cutting the escaping paths out of the text instead would let
    /// one short escape decide the reading of every other call: an escape of `/`
    /// takes the separator that `STAGED_SKILL_DIRS` is written in out of the
    /// text, and an escape of `~` leaves the rest of a host skill path behind to
    /// be counted as this run's.
    fn tokens_worth_reading(&self, named: &str) -> Vec<String> {
        if self.left_the_workspace(named) {
            return Vec::new();
        }
        named_tokens(named)
            .into_iter()
            .filter(|token| !self.left_the_workspace(token))
            .collect()
    }

    fn left_the_workspace(&self, named: &str) -> bool {
        self.workspace_escapes.iter().any(|escape| escape.path == named)
    }
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
    let mut operands = Vec::new();
    let mut skip_program = true;
    for token in named_tokens(command) {
        if std::mem::take(&mut skip_program) {
            continue;
        }
        if INTERPRETER_FLAGS.contains(&token.as_str()) {
            skip_program = true;
            continue;
        }
        if token.starts_with('/') || token.starts_with('~') || token.starts_with("..") {
            operands.push(token);
        }
    }
    operands
}

/// The tokens of a named argument, each stripped of the quoting a command line
/// carries around it.
///
/// One argument can name several paths, since a command-style tool records its
/// whole command line, and the tokens are how they are told apart. The quoting is
/// what says where a token ends: whitespace alone tears a quoted path that holds a
/// space in two, and the tail of a torn host skill path still reads as a skill
/// directory of this run's own. The payload of an interpreter flag is a command
/// line in its own right, so it is opened into its own tokens and a path reached
/// through a shell is a token like any other. Every reader tokenizes the same way,
/// so a token that named a path can be compared against a recorded escape by
/// equality.
fn named_tokens(named: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote: Option<char> = None;
    let mut nested = false;
    for character in named.chars() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => token.push(character),
            None if character == '\'' || character == '"' || character == '`' => quote = Some(character),
            None if character.is_whitespace() => push_token(&mut tokens, std::mem::take(&mut token), &mut nested),
            None => token.push(character),
        }
    }
    push_token(&mut tokens, token, &mut nested);
    tokens
}

fn push_token(tokens: &mut Vec<String>, token: String, nested: &mut bool) {
    if token.is_empty() {
        return;
    }
    if std::mem::take(nested) {
        tokens.extend(named_tokens(&token));
        return;
    }
    *nested = INTERPRETER_FLAGS.contains(&token.as_str());
    tokens.push(token);
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
#[path = "transcript_tests.rs"]
mod tests;
