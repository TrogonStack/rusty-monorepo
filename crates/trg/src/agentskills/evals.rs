use serde::ser::SerializeStruct;
mod constants;
use super::case_env::CaseEnv;
use super::companion_skills::CompanionSkill;
use super::graders::CaseGrader;
use super::grading::{self, GradingFile};
use super::model_name::{ModelName, ModelSource};
use super::outputs::guess_mime_type;
use super::prompt::StagedSkillDir;
use super::runner::TimingFile;
use super::sampling::AttemptCount;
use super::system_prompt_appendix::SystemPromptAppendix;
use super::tool_grant::ToolGrant;
use super::validation::{ValidationError, ValidationErrors};
use super::workspace_scaffold::WorkspaceScaffold;
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use constants::FIXTURE_BINARY_SAMPLE_BYTES;
pub use constants::{DEFAULT_MAX_FIXTURE_BYTES, EVAL_SUITE_DIR_NAME, EVAL_SUITE_MANIFEST_NAME};
use regex::Regex;
use schemars::JsonSchema;
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// A single path segment naming the directory, under a skill root, that holds a
/// suite's eval manifest or case directories.
///
/// A skill directory is author-supplied, and every reader that wants a suite joins this
/// onto one it did not choose. `PathBuf::join` silently discards the base and returns
/// the joined value outright when handed an absolute path, so an unvalidated name here
/// would let a skill's own manifest point `trg` at an arbitrary path on the operator's
/// machine; a name of `..` or containing a separator reaches just as far with smaller
/// words. Restricting this to one plain segment closes all three at the type rather than
/// asking every join site to remember why.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[schemars(schema_with = "eval_dir_name_schema")]
pub struct EvalDirName(String);

fn eval_dir_name_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1,
        "pattern": "^[^/\\\\]+$"
    })
}

impl EvalDirName {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        if value.is_empty() {
            return Err(
                "eval directory name must not be empty: an empty segment names the skill directory itself, not a directory under it"
                    .to_string(),
            );
        }
        if let Some(separator) = value.chars().find(|c| matches!(c, '/' | '\\')) {
            return Err(format!(
                "eval directory name '{value}' must be a single path segment: '{separator}' names a path, not the one directory this joins onto the skill root"
            ));
        }
        if value == "." || value == ".." {
            return Err(format!(
                "eval directory name '{value}' names no directory of its own: it resolves to the skill directory or its parent instead"
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for EvalDirName {
    fn default() -> Self {
        Self(EVAL_SUITE_DIR_NAME.to_string())
    }
}

impl fmt::Display for EvalDirName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<Path> for EvalDirName {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl std::str::FromStr for EvalDirName {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl<'de> Deserialize<'de> for EvalDirName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Error, Debug)]
pub enum EvalError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Validation failed: {0}")]
    Validation(ValidationErrors),

    #[error("Validation failed: feedback: {0}")]
    Feedback(#[source] Box<super::feedback::FeedbackError>),
}

impl EvalError {
    /// Which kind of red this failure is, so a caller does not have to re-derive it.
    ///
    /// A validation failure is a finding about the thing being checked, and is the answer
    /// the check was asked for. Failing to read or parse the artifacts is not an answer at
    /// all: the check never reached the thing it was supposed to measure, so pointing
    /// anyone at the skill over it would be pointing them at something nothing looked at.
    pub fn reported_as(&self) -> ExitCode {
        match self {
            Self::Validation(_) | Self::Feedback(_) => ExitCode::GateFailed,
            Self::Io(_) | Self::Json(_) => ExitCode::InfrastructureFailure,
        }
    }
}

pub type Result<T> = std::result::Result<T, EvalError>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[schemars(schema_with = "eval_case_id_schema")]
pub struct EvalCaseId(String);

fn eval_case_id_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "description": "Non-empty string or non-negative integer",
        "oneOf": [
            { "type": "string", "minLength": 1 },
            { "type": "integer", "minimum": 0 }
        ]
    })
}

impl EvalCaseId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EvalCaseId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EvalCaseId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EvalCaseIdVisitor;

        impl Visitor<'_> for EvalCaseIdVisitor {
            type Value = EvalCaseId;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a non-empty string or non-negative integer")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return Err(E::custom("eval id must not be empty"));
                }

                Ok(EvalCaseId(trimmed.to_string()))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&value)
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(EvalCaseId(value.to_string()))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                if value < 0 {
                    return Err(E::custom("eval id must be non-negative"));
                }

                Ok(EvalCaseId(value.to_string()))
            }
        }

        deserializer.deserialize_any(EvalCaseIdVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[schemars(schema_with = "non_empty_string_schema")]
pub struct NonEmptyString(String);

fn non_empty_string_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1
    })
}

impl NonEmptyString {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NonEmptyString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for NonEmptyString {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.trim().is_empty() {
            return Err(de::Error::custom("must be a non-empty string"));
        }
        Ok(NonEmptyString(value))
    }
}

/// Rejects an absolute path or one that escapes the directory it is declared
/// relative to, shared by every value object that names a path a case must
/// not be able to point outside the skill it belongs to.
pub(crate) fn validate_relative_path(noun: &str, value: &str) -> std::result::Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{noun} must not be empty"));
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(format!("{noun} '{value}' must be relative to the skill directory"));
    }
    if !path
        .components()
        .all(|component| matches!(component, Component::CurDir | Component::Normal(_)))
    {
        return Err(format!("{noun} '{value}' must stay inside the skill directory"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct RelativeSkillPath(String);

impl RelativeSkillPath {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl AsRef<Path> for RelativeSkillPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for RelativeSkillPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RelativeSkillPath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        validate_relative_path("path", &value).map_err(de::Error::custom)?;
        Ok(RelativeSkillPath(value))
    }
}

/// A path relative to the skill directory that may use glob wildcards.
///
/// The supported dialect is deliberately small: `*` matches within one path
/// segment, `**` matches across segments (including zero of them), and `?`
/// matches a single character other than `/`. Character classes and brace
/// alternation (`[`, `]`, `{`, `}`) are rejected outright rather than given a
/// meaning the eval author did not ask for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct GlobPattern(String);

impl GlobPattern {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        validate_relative_path("pattern", &value)?;
        if let Some(c) = value.chars().find(|c| matches!(c, '[' | ']' | '{' | '}' | '\\')) {
            return Err(format!(
                "pattern '{value}' must not contain '{c}': only *, **, and ? are supported"
            ));
        }
        let normalized = normalize_glob(&value);
        if normalized.is_empty() {
            return Err(format!("pattern '{value}' names no path"));
        }
        Ok(GlobPattern(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True when the pattern names an exact path with no wildcard.
    pub fn is_literal(&self) -> bool {
        !self.0.contains(['*', '?'])
    }

    pub fn as_literal_path(&self) -> Option<&Path> {
        self.is_literal().then(|| Path::new(self.0.as_str()))
    }

    /// The same pattern as it reads from inside the output directory.
    ///
    /// `outputs/` is a spelling of that directory, not a path segment that is
    /// always there to walk into: grading resolves the output tree beside the
    /// workspace in one run layout and inside it in another, so a pattern naming
    /// `outputs/` matches nothing at all in the first. Dropping the prefix before
    /// matching against the output tree makes the pattern answer the same way in
    /// both, which is how a literal `outputs/...` path is already resolved.
    pub fn within_outputs(&self) -> Self {
        match self.0.strip_prefix("outputs/") {
            Some(rest) if !rest.is_empty() => Self(rest.to_string()),
            _ => self.clone(),
        }
    }

    /// Compiles the glob into a regex anchored to a full match against a
    /// `/`-separated relative path.
    pub fn compile(&self) -> Regex {
        Regex::new(&format!("^{}$", glob_to_regex_source(&self.0))).expect("glob was validated at construction")
    }
}

/// Drops the `./` segments `validate_relative_path` allows, so a pattern has one
/// spelling by the time it is compiled. The walk that a glob is matched against
/// yields paths relative to a directory, which never carry a `./` prefix, and a
/// dot compiled as the literal it is would quietly match nothing at all.
fn normalize_glob(value: &str) -> String {
    Path::new(value)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Translates the supported glob dialect (`*`, `**`, `?`) into a regex
/// fragment. `*` stays within a path segment, `**/` consumes zero or more
/// whole segments, a bare `**` matches anything (including `/`), and `?`
/// matches one character other than `/`. Everything else is escaped literally.
fn glob_to_regex_source(glob: &str) -> String {
    let mut out = String::new();
    let mut chars = glob.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    out.push_str("(?:.*/)?");
                } else {
                    out.push_str(".*");
                }
            }
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            other => out.push_str(&regex::escape(&other.to_string())),
        }
    }
    out
}

impl AsRef<Path> for GlobPattern {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl fmt::Display for GlobPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GlobPattern {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// Whether a fixture's copy in the workspace may be changed by a run.
///
/// A run that is handed a read-only fixture is being asked whether it can do its job
/// without touching a file it has no business changing. Writable stays the default so
/// every fixture declared before this existed keeps its original meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FixtureMutability {
    #[default]
    Writable,
    ReadOnly,
}

impl FixtureMutability {
    pub fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

/// A `files` entry on an `EvalCase`.
///
/// The wire form stays a bare string for the common, writable case: a suite authored
/// before read-only fixtures existed must keep parsing, and must keep round-tripping
/// as a string rather than being rewritten into the object form the first time the
/// suite is saved back out. Only a fixture that opts into `read_only` pays for the
/// object form.
#[derive(Debug, Clone, PartialEq, Eq, Hash, JsonSchema)]
#[schemars(schema_with = "eval_fixture_schema")]
pub struct EvalFixture {
    path: RelativeSkillPath,
    mutability: FixtureMutability,
}

fn eval_fixture_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "description": "A fixture path, or an object naming its path and mutability",
        "oneOf": [
            { "type": "string", "minLength": 1 },
            {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "minLength": 1 },
                    "mode": { "type": "string", "enum": ["writable", "read_only"] }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        ]
    })
}

impl EvalFixture {
    pub fn path(&self) -> &RelativeSkillPath {
        &self.path
    }

    pub fn as_str(&self) -> &str {
        self.path.as_str()
    }

    pub fn as_path(&self) -> &Path {
        self.path.as_path()
    }

    pub fn is_read_only(&self) -> bool {
        self.mutability.is_read_only()
    }
}

impl fmt::Display for EvalFixture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.path, f)
    }
}

impl<'de> Deserialize<'de> for EvalFixture {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EvalFixtureVisitor;

        impl<'de> Visitor<'de> for EvalFixtureVisitor {
            type Value = EvalFixture;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a relative path string, or an object with \"path\" and optional \"mode\"")
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                let path = RelativeSkillPath::deserialize(de::value::StrDeserializer::new(value))?;
                Ok(EvalFixture {
                    path,
                    mutability: FixtureMutability::Writable,
                })
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_str(&value)
            }

            fn visit_map<A>(self, map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                #[derive(Deserialize)]
                #[serde(rename_all = "snake_case", deny_unknown_fields)]
                struct EvalFixtureObject {
                    path: RelativeSkillPath,
                    #[serde(default)]
                    mode: FixtureMutability,
                }

                let object = EvalFixtureObject::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(EvalFixture {
                    path: object.path,
                    mutability: object.mode,
                })
            }
        }

        deserializer.deserialize_any(EvalFixtureVisitor)
    }
}

impl Serialize for EvalFixture {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.mutability {
            // A writable fixture must keep serializing as a bare string: rewriting it into
            // object form the first time a suite round-trips through this type would change
            // the file on disk for every suite that has never heard of read-only fixtures.
            FixtureMutability::Writable => serializer.serialize_str(self.path.as_str()),
            FixtureMutability::ReadOnly => {
                let mut state = serializer.serialize_struct("EvalFixture", 2)?;
                state.serialize_field("path", &self.path)?;
                state.serialize_field("mode", &self.mutability)?;
                state.end()
            }
        }
    }
}

#[cfg(test)]
impl EvalFixture {
    pub fn writable(path: RelativeSkillPath) -> Self {
        EvalFixture {
            path,
            mutability: FixtureMutability::Writable,
        }
    }

    pub fn read_only(path: RelativeSkillPath) -> Self {
        EvalFixture {
            path,
            mutability: FixtureMutability::ReadOnly,
        }
    }
}

/// A transcript a case would resume before its own prompt is sent.
///
/// No installed harness can adopt an arbitrary, case-authored transcript as history it
/// did not itself produce (see `HarnessControl::ConversationSeeding`), so this exists to
/// let a case say what it is asking for honestly rather than not at all: a run whose case
/// sets this is skipped, naming the reason, instead of silently starting a fresh
/// conversation and answering a different question than the one the case asked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ConversationHistory(RelativeSkillPath);

impl ConversationHistory {
    pub fn transcript(&self) -> &RelativeSkillPath {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvalPriority {
    Low,
    Normal,
    High,
    Critical,
}

/// Which side of the train/test boundary a case belongs to.
///
/// A suite that never withholds a case from training cannot tell a skill that got
/// better from one that learned the suite, and every overfitting check depends on a
/// split existing. A case declares its side as a typed field rather than a tag
/// convention, so nothing downstream has to trust that a tag was spelled consistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvalSplit {
    #[default]
    Train,
    Test,
}

impl EvalSplit {
    pub const ALL: [Self; 2] = [Self::Train, Self::Test];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Train => "train",
            Self::Test => "test",
        }
    }
}

/// Whether a case's prompt names the skill that is staged for it.
///
/// A prompt that names the skill asks how well a run uses a skill it has already
/// been handed. It cannot also ask whether the skill's own description wins the
/// run's routing decision, because the prompt made that decision for the run.
/// Those are two different questions, so a case has to say which one it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillDisclosure {
    /// Name the skill, the directory it is staged in, and its description.
    #[default]
    Announced,
    /// Stage the skill where a listing of the workspace shows it and say nothing
    /// about it, so both arms of a comparison get the same prompt and a run has
    /// to reach for the skill on its own.
    Unannounced,
}

impl SkillDisclosure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Announced => "announced",
            Self::Unannounced => "unannounced",
        }
    }

    pub fn announces_the_skill(self) -> bool {
        matches!(self, Self::Announced)
    }
}

fn is_announced(disclosure: &SkillDisclosure) -> bool {
    disclosure.announces_the_skill()
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalSuite {
    pub skill_name: NonEmptyString,
    /// The suite author's own claim about which directory this manifest was found in.
    ///
    /// Optional, and checked rather than trusted: a suite is located by the eval
    /// directory `trg` was already told to use (`--eval-dir`, or the default), never by
    /// this field, so it exists to catch the manifest and the caller disagreeing about
    /// where the suite lives rather than to redirect discovery. See
    /// `case_directories::resolve_eval_suite`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eval_dir: Option<EvalDirName>,
    #[serde(deserialize_with = "deserialize_evals")]
    pub evals: Vec<EvalCase>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalCase {
    pub id: EvalCaseId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<NonEmptyString>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<NonEmptyString>,
    pub prompt: NonEmptyString,
    pub expected_output: NonEmptyString,
    #[serde(default)]
    pub files: Vec<EvalFixture>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<EvalPriority>,
    #[serde(default)]
    pub split: EvalSplit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u32>,
    /// How many times this case is drawn, for a case whose stability is the question or
    /// whose cost makes the suite default too expensive.
    ///
    /// Yields to an explicit `--attempts`, so an operator can still force a cheap smoke
    /// pass or a deep one over a whole suite. Absent means the case takes the pass default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<AttemptCount>,

    /// The model this case wants, for a case whose question is about one model in
    /// particular.
    ///
    /// Overrides `--runner-model` rather than narrowing it: unlike a tool grant, a model is
    /// not an authority the operator is capping, so there is nothing for a case to exceed.
    /// Absent means the case takes whatever the operator asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelName>,
    /// Variables this case adds to its own run's environment, for a case whose inputs
    /// cannot be expressed as a file in the workspace.
    ///
    /// The sanctioned hole in the isolation `--environment` otherwise holds shut, which is
    /// why the names are confined to `EVAL_*`: a case that could name `PATH` or a
    /// credential variable would be rewriting the environment the policy promised rather
    /// than adding its own inputs to it. Absent means the run sees only what the policy
    /// admits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<CaseEnv>,
    /// Text appended to the harness's own system prompt for this case, for a case whose
    /// question is about how the agent was instructed rather than about what it was asked.
    ///
    /// A harness that offers no way to append one cannot answer the question a case using
    /// this is asking, so such a run is recorded as not started rather than run without it:
    /// a run missing the instruction it was supposed to carry is not a weaker answer, it is
    /// an answer to a different question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<SystemPromptAppendix>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_output_files: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grader_hints: Option<HashMap<String, serde_json::Value>>,
    #[serde(
        default,
        deserialize_with = "deserialize_case_graders",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub graders: Vec<CaseGrader>,
    #[serde(default, skip_serializing_if = "is_announced")]
    pub skill_disclosure: SkillDisclosure,
    /// Other skill directories staged beside the one under test, present only to be passed
    /// over. See [`CompanionSkill`].
    ///
    /// Only an unannounced case may declare them, and a case that declares them anyway is
    /// refused when the suite loads rather than at the point one would have been staged.
    /// An announced prompt names the skill and the directory it sits in, so it has already
    /// made the routing decision a distractor exists to leave open; staging one would put a
    /// skill in the workspace that the prompt ruled out before the run began. Refusing it
    /// where the suite is read means both authoring layouts are covered by one check and
    /// the case never costs a run to find out, which the run-time refusals cannot claim:
    /// those exist for facts only the chosen harness can supply.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub companion_skills: Vec<CompanionSkill>,
    /// The state this case is asking about, for a case that is not asking about an empty
    /// directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaffold: Option<WorkspaceScaffold>,
    /// A transcript this case would resume before its own prompt, for a case that is not
    /// asking about a first turn. See [`ConversationHistory`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_history: Option<ConversationHistory>,
    /// This case's own ceiling on what its run's harness may reach for.
    ///
    /// Narrows whatever the operator granted at the CLI; it can never widen it. Absent
    /// means the case adds no ceiling of its own, leaving the operator's grant whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<ToolGrant>,
}

impl EvalCase {
    /// Whether the case declares any grader at all.
    pub fn has_checks(&self) -> bool {
        !self.graders.is_empty()
    }

    /// Whether any of the case's checks is about the run reaching for the skill,
    /// which is the one question an announcing prompt answers for it.
    pub fn checks_skill_engagement(&self) -> bool {
        self.graders
            .iter()
            .any(|declared| declared.grader.presupposes_the_skill())
    }
}

pub fn parse_eval_suite(content: &str) -> Result<EvalSuite> {
    let value: serde_json::Value = serde_json::from_str(content)?;
    serde_json::from_value(value).map_err(EvalError::from)
}

pub fn effective_timeout_secs(case: &EvalCase, global_timeout_secs: Option<u64>) -> Option<u64> {
    case.timeout_secs.map(u64::from).or(global_timeout_secs)
}

/// How many draws a run of this case takes, once the case's own count and the operator's
/// `--attempts` are combined.
///
/// The operator wins when they said anything at all, which is why `operator_attempts` is
/// optional rather than defaulted at the CLI: a count defaulted there is indistinguishable
/// from one an operator typed, and a case pinning five draws would then silently override
/// an operator asking for one.
pub fn effective_attempts(case: &EvalCase, operator_attempts: Option<AttemptCount>) -> AttemptCount {
    operator_attempts
        .or(case.attempts)
        .unwrap_or_else(AttemptCount::recommended)
}

/// The model a run of this case executes under, once the case's own choice and the
/// operator's `--runner-model` are combined.
pub fn effective_model<'a>(case: &'a EvalCase, global_model: Option<&'a ModelName>) -> Option<&'a ModelName> {
    case.model.as_ref().or(global_model)
}

/// Where `effective_model` obtained its answer, for a run to record alongside the model
/// itself.
pub fn effective_model_source(case: &EvalCase, global_model: Option<&ModelName>) -> ModelSource {
    if case.model.is_some() {
        ModelSource::Case
    } else if global_model.is_some() {
        ModelSource::RunnerFlag
    } else {
        ModelSource::RunnerDefault
    }
}

fn deserialize_evals<'de, D>(deserializer: D) -> std::result::Result<Vec<EvalCase>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Vec<serde_json::Value> = Vec::deserialize(deserializer)?;
    let mut evals = Vec::with_capacity(raw.len());
    for (index, case) in raw.into_iter().enumerate() {
        evals.push(
            serde_json::from_value::<EvalCase>(case)
                .map_err(|error| de::Error::custom(format!("evals[{index}]: {error}")))?,
        );
    }
    if evals.is_empty() {
        return Err(de::Error::custom("evals must contain at least one test case"));
    }
    let mut seen = HashSet::new();
    for eval in &evals {
        if !seen.insert(eval.id.as_str()) {
            return Err(de::Error::custom(format!("evals contains duplicate id '{}'", eval.id)));
        }
        if !eval.companion_skills.is_empty() && eval.skill_disclosure.announces_the_skill() {
            return Err(de::Error::custom(format!(
                "evals id '{}' declares companion_skills ({}), but its prompt announces the skill and the directory it is staged in, so there is no routing decision left for a distractor to sit in front of; set \"skill_disclosure\": \"{}\" or drop the companions",
                eval.id,
                eval.companion_skills
                    .iter()
                    .map(|companion| format!("'{companion}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
                SkillDisclosure::Unannounced.as_str()
            )));
        }
    }
    Ok(evals)
}

fn deserialize_case_graders<'de, D>(deserializer: D) -> std::result::Result<Vec<CaseGrader>, D::Error>
where
    D: Deserializer<'de>,
{
    let graders = Vec::<CaseGrader>::deserialize(deserializer)?;
    let mut seen = HashSet::new();
    for declared in &graders {
        if let Some(name) = &declared.name {
            if !seen.insert(name.as_str()) {
                return Err(de::Error::custom(format!("graders contains duplicate name '{name}'")));
            }
        }
    }
    Ok(graders)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvalCheckOptions {
    pub require_graders: bool,
    pub max_fixture_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EvalLintOptions {
    pub allow_empty_graders: bool,
    pub max_fixture_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalLintWarning {
    pub eval_id: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EvalCheckReport {
    pub skill_name: String,
    pub eval_count: usize,
    pub file_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceCheckReport>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WorkspaceCheckOptions {
    pub require_grading: bool,
    pub fail_on_failed_assertions: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct WorkspaceCheckReport {
    pub grading_files: usize,
    pub timing_files: usize,
    /// Scored results only: results the runner could not answer are counted in
    /// `unsupported_assertions`, results that presuppose the skill in
    /// `excluded_assertions`, and neither reaches `pass_rate`.
    pub assertion_results: usize,
    pub passed_assertions: usize,
    pub failed_assertions: usize,
    #[serde(default)]
    pub unsupported_assertions: usize,
    #[serde(default)]
    pub excluded_assertions: usize,
    #[serde(default)]
    pub ungraded_assertions: usize,
    /// `null` when the workspace produced no scored assertion at all, so an
    /// unobservable runner is not reported as a total failure.
    pub pass_rate: Option<f64>,
}

pub fn load_eval_suite(fs: &impl FileSystem, skill_path: &Path, eval_dir: &EvalDirName) -> Result<EvalSuite> {
    super::case_directories::resolve_eval_suite(fs, skill_path, eval_dir).map(|compiled| compiled.suite)
}

pub fn eval_manifest_scaffold_json(skill_name: &str) -> String {
    let skill_name_json = serde_json::to_string(skill_name).expect("string serialization to JSON is infallible");
    format!(
        r#"{{
  "skill_name": {skill_name_json},
  "evals": [
    {{
      "id": "produces-a-summary",
      "prompt": "Summarize the sample task using the skill guidance and write the result to outputs/summary.md with a top-level Markdown heading.",
      "expected_output": "outputs/summary.md exists and opens with a top-level Markdown heading.",
      "files": [],
      "graders": [
        {{ "type": "file_exists", "path": "summary.md" }},
        {{
          "type": "regex",
          "pattern": "^#[ \\t]+\\S",
          "target": {{ "file": "summary.md" }}
        }},
        {{
          "type": "contains",
          "text": "TODO",
          "target": {{ "file": "summary.md" }},
          "negate": true
        }}
      ]
    }},
    {{
      "id": "triggers-the-skill",
      "prompt": "Replace this with the request a user would make in their own words for the work this skill does, mentioning neither the skill nor where it lives.",
      "expected_output": "The run finds the skill and follows it rather than improvising.",
      "files": [],
      "tags": ["triggering"],
      "skill_disclosure": "unannounced",
      "graders": [
        {{ "type": "skill_used" }}
      ]
    }}
  ]
}}"#
    )
}

pub fn scaffold_eval_suite(skill_name: &str) -> EvalSuite {
    parse_eval_suite(&eval_manifest_scaffold_json(skill_name)).expect("scaffold manifest must parse")
}

pub fn write_eval_suite(
    fs: &impl FileSystem,
    skill_path: &Path,
    eval_dir: &EvalDirName,
    suite: &EvalSuite,
) -> Result<()> {
    let suite_path = skill_path.join(eval_dir.as_str()).join(EVAL_SUITE_MANIFEST_NAME);
    let json = serde_json::to_string_pretty(suite)?;
    fs.write(&suite_path, &json)?;
    Ok(())
}

pub fn write_eval_manifest_scaffold(
    fs: &impl FileSystem,
    skill_path: &Path,
    eval_dir: &EvalDirName,
    skill_name: &str,
) -> Result<()> {
    let suite_path = skill_path.join(eval_dir.as_str()).join(EVAL_SUITE_MANIFEST_NAME);
    fs.write(&suite_path, &eval_manifest_scaffold_json(skill_name))?;
    Ok(())
}

pub fn lint_eval_suite(suite: &EvalSuite, options: EvalLintOptions) -> Vec<EvalLintWarning> {
    let mut warnings = Vec::new();

    for eval in &suite.evals {
        let eval_id = eval.id.as_str().to_string();

        if eval.prompt.as_str().len() < 20 {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: "prompt is too vague (shorter than 20 characters)".to_string(),
            });
        }

        if eval.expected_output.as_str().len() < 10 {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: "expected_output is too generic (shorter than 10 characters)".to_string(),
            });
        }

        if !eval.files.is_empty() && !eval.prompt.as_str().contains('/') {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: "fixture files are present but the prompt does not reference a file path".to_string(),
            });
        }

        let mut seen_files = HashSet::new();
        for file in &eval.files {
            if !seen_files.insert(file.as_str()) {
                warnings.push(EvalLintWarning {
                    eval_id: eval_id.clone(),
                    message: format!("duplicate fixture path '{}'", file),
                });
            }
        }

        if eval.checks_skill_engagement() && eval.skill_disclosure.announces_the_skill() {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.clone(),
                message: format!(
                    "the case checks whether the run reaches for the skill, but the prompt names the skill and its directory; set \"skill_disclosure\": \"{}\" to leave the routing decision to the run",
                    SkillDisclosure::Unannounced.as_str()
                ),
            });
        }

        if !options.allow_empty_graders && !eval.has_checks() {
            warnings.push(EvalLintWarning {
                eval_id,
                message: "no graders are declared".to_string(),
            });
        }
    }

    if !suite.evals.is_empty() && !suite.evals.iter().any(|eval| eval.split == EvalSplit::Test) {
        warnings.push(EvalLintWarning {
            eval_id: suite.skill_name.as_str().to_string(),
            message:
                "no case declares \"split\": \"test\"; every overfitting check depends on a held-out case existing"
                    .to_string(),
        });
    }

    warnings
}

pub fn lint_eval_suite_fixtures(
    fs: &impl FileSystem,
    skill_path: &Path,
    suite: &EvalSuite,
    options: EvalLintOptions,
) -> Vec<EvalLintWarning> {
    let mut warnings = lint_eval_suite(suite, options);

    for eval in &suite.evals {
        let eval_id = eval.id.as_str().to_string();
        warnings.extend(lint_fixture_files(
            fs,
            skill_path,
            &eval_id,
            &eval.files,
            options.max_fixture_bytes,
        ));
    }

    warnings
}

fn effective_max_fixture_bytes(max_fixture_bytes: Option<u64>) -> u64 {
    max_fixture_bytes.unwrap_or(DEFAULT_MAX_FIXTURE_BYTES)
}

fn lint_fixture_files(
    fs: &impl FileSystem,
    skill_path: &Path,
    eval_id: &str,
    files: &[EvalFixture],
    max_fixture_bytes: Option<u64>,
) -> Vec<EvalLintWarning> {
    let limit = effective_max_fixture_bytes(max_fixture_bytes);
    let mut warnings = Vec::new();

    for file in files {
        let full_path = skill_path.join(file.as_path());
        if !fs.is_file(&full_path) {
            continue;
        }

        let Ok(bytes) = fs.read_bytes(&full_path) else {
            continue;
        };

        if bytes.len() as u64 > limit {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.to_string(),
                message: format!(
                    "fixture '{}' is {} bytes (exceeds {} byte limit)",
                    file,
                    bytes.len(),
                    limit
                ),
            });
        }

        if is_probably_binary_fixture(&full_path, &bytes) {
            warnings.push(EvalLintWarning {
                eval_id: eval_id.to_string(),
                message: format!(
                    "fixture '{}' appears to be binary; runners and graders may not handle it well",
                    file
                ),
            });
        }
    }

    warnings
}

pub fn is_probably_binary_fixture(path: &Path, bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(FIXTURE_BINARY_SAMPLE_BYTES)];
    if sample.contains(&0) {
        return true;
    }

    if let Some(mime) = guess_mime_type(path) {
        return !(mime.starts_with("text/")
            || matches!(
                mime.as_str(),
                "application/json" | "application/xml" | "application/yaml" | "application/x-ndjson"
            ));
    }

    std::str::from_utf8(sample).is_err()
}

pub fn missing_expected_output_warnings(eval: &EvalCase, outputs_dir: &Path) -> Vec<String> {
    let Some(expected_files) = eval.expected_output_files.as_ref() else {
        return Vec::new();
    };

    expected_files
        .iter()
        .filter(|relative| !outputs_dir.join(relative).is_file())
        .map(|relative| format!("expected output file '{}' is missing under outputs/", relative))
        .collect()
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub fn print_eval_lint_warnings(warnings: &[EvalLintWarning]) {
    for warning in warnings {
        eprintln!("eval lint warning ({}): {}", warning.eval_id, warning.message);
    }
}

pub fn check_eval_suite(
    fs: &impl FileSystem,
    skill_path: &Path,
    eval_dir: &EvalDirName,
    expected_skill_name: &str,
    options: EvalCheckOptions,
) -> Result<EvalCheckReport> {
    let suite = super::case_directories::resolve_eval_suite(fs, skill_path, eval_dir)?.suite;

    let mut errors = ValidationErrors::new();
    let mut file_count = 0;

    if suite.skill_name.as_str() != expected_skill_name {
        errors.push(ValidationError::for_field(
            "skill_name",
            format!(
                "'{}' must match skill frontmatter name '{}'",
                suite.skill_name, expected_skill_name
            ),
        ));
    }

    for eval in &suite.evals {
        let label = format!("evals id '{}'", eval.id);

        if options.require_graders && !eval.has_checks() {
            errors.push(ValidationError::for_field(
                label.clone(),
                "must define at least one grader",
            ));
        }

        for file in &eval.files {
            file_count += 1;
            let full_path = skill_path.join(file.as_path());
            if !fs.exists(&full_path) {
                errors.push(ValidationError::for_field(
                    format!("{}.files", label),
                    format!("path '{}' does not exist", full_path.display()),
                ));
                continue;
            }

            if fs.is_file(&full_path) {
                let limit = effective_max_fixture_bytes(options.max_fixture_bytes);
                match fs.read_bytes(&full_path) {
                    Ok(bytes) if bytes.len() as u64 > limit => {
                        errors.push(ValidationError::for_field(
                            format!("{}.files", label),
                            format!(
                                "path '{}' is {} bytes (exceeds {} byte limit)",
                                full_path.display(),
                                bytes.len(),
                                limit
                            ),
                        ));
                    }
                    Err(source) => {
                        errors.push(ValidationError::for_field(
                            format!("{}.files", label),
                            format!("path '{}': {source}", full_path.display()),
                        ));
                    }
                    Ok(_) => {}
                }
            }
        }

        if let Err(failure) = super::companion_skills::resolve_companions(
            fs,
            skill_path,
            &eval.companion_skills,
            &[StagedSkillDir::companion(expected_skill_name)],
        ) {
            errors.push(ValidationError::for_field(
                format!("{}.companion_skills", label),
                failure.to_string(),
            ));
        }
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    Ok(EvalCheckReport {
        skill_name: suite.skill_name.as_str().to_string(),
        eval_count: suite.evals.len(),
        file_count,
        workspace: None,
    })
}

pub fn check_workspace(workspace_path: &Path, options: WorkspaceCheckOptions) -> Result<WorkspaceCheckReport> {
    if !workspace_path.exists() {
        return Err(EvalError::Validation(
            ValidationError::for_field(format!("workspace '{}'", workspace_path.display()), "does not exist").into(),
        ));
    }

    if !workspace_path.is_dir() {
        return Err(EvalError::Validation(
            ValidationError::for_field(
                format!("workspace '{}'", workspace_path.display()),
                "must be a directory",
            )
            .into(),
        ));
    }

    let search_root = ArtifactSearchRoot::for_workspace(workspace_path);
    let mut grading_files = Vec::new();
    let mut timing_files = Vec::new();
    collect_named_files(search_root.as_path(), "grading.json", &mut grading_files)?;
    collect_named_files(search_root.as_path(), "timing.json", &mut timing_files)?;

    let mut errors = ValidationErrors::new();
    let mut assertion_results = 0;
    let mut passed_assertions = 0;
    let mut failed_assertions = 0;
    let mut unsupported_assertions = 0;
    let mut excluded_assertions = 0;
    let mut ungraded_assertions = 0;

    if options.require_grading && grading_files.is_empty() {
        errors.push(ValidationError::for_field(
            format!("workspace '{}'", workspace_path.display()),
            "must contain at least one grading.json",
        ));
    }

    for grading_path in &grading_files {
        let grading = read_grading_file(grading_path)?;
        validate_grading_file(grading_path, &grading, options, &mut errors);

        let counts = grading::GradingCounts::tally(&grading.assertion_results);
        assertion_results += counts.scored();
        passed_assertions += counts.passed;
        failed_assertions += counts.failed;
        unsupported_assertions += counts.unsupported;
        excluded_assertions += counts.excluded;
        ungraded_assertions += counts.ungraded;
    }

    for timing_path in &timing_files {
        let timing = read_timing_file(timing_path)?;
        validate_timing_file(timing_path, &timing, &mut errors);
    }

    if !errors.is_empty() {
        return Err(EvalError::Validation(errors));
    }

    let pass_rate = if assertion_results == 0 {
        None
    } else {
        Some(passed_assertions as f64 / assertion_results as f64)
    };

    Ok(WorkspaceCheckReport {
        grading_files: grading_files.len(),
        timing_files: timing_files.len(),
        assertion_results,
        passed_assertions,
        failed_assertions,
        unsupported_assertions,
        excluded_assertions,
        ungraded_assertions,
        pass_rate,
    })
}

fn validate_non_empty(value: &str, field: &str, errors: &mut ValidationErrors) {
    if value.trim().is_empty() {
        errors.push(ValidationError::for_field(field, "must be a non-empty string"));
    }
}

/// Where verification looks for the companion artifacts of a run.
///
/// `eval grade` writes `grading.json` into the run directory rather than the
/// workspace, because the verdict is about the run and not about the files the
/// agent happened to leave behind. Pointing `verify` at the workspace is the
/// documented habit, so a workspace path widens to its run directory instead of
/// reporting a graded run as ungraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSearchRoot(PathBuf);

impl ArtifactSearchRoot {
    const WORKSPACE_DIR_NAME: &'static str = "workspace";

    pub fn for_workspace(path: &Path) -> Self {
        if path.file_name().and_then(|name| name.to_str()) == Some(Self::WORKSPACE_DIR_NAME) {
            if let Some(run_dir) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                return Self(run_dir.to_path_buf());
            }
        }
        Self(path.to_path_buf())
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

pub fn collect_named_files(root: &Path, file_name: &str, matches: &mut Vec<PathBuf>) -> std::io::Result<()> {
    walk_named_files(root, file_name, matches)?;
    matches.sort();
    Ok(())
}

fn walk_named_files(root: &Path, file_name: &str, matches: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            walk_named_files(&path, file_name, matches)?;
        } else if file_type.is_file() && path.file_name().and_then(|name| name.to_str()) == Some(file_name) {
            matches.push(path);
        }
    }

    Ok(())
}

fn read_grading_file(path: &Path) -> Result<GradingFile> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn read_timing_file(path: &Path) -> Result<TimingFile> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

fn validate_grading_file(
    path: &Path,
    grading: &GradingFile,
    options: WorkspaceCheckOptions,
    errors: &mut ValidationErrors,
) {
    let file_label = path.display().to_string();

    if grading.assertion_results.is_empty() {
        errors.push(ValidationError::for_field(
            format!("{} assertion_results", file_label),
            "must contain at least one result",
        ));
    }

    let counts = grading::GradingCounts::tally(&grading.assertion_results);
    let passed = counts.passed;
    let failed = counts.failed;

    for (index, result) in grading.assertion_results.iter().enumerate() {
        validate_non_empty(
            &result.assertion,
            &format!("{} assertion_results[{index}].assertion", file_label),
            errors,
        );
        validate_non_empty(
            &result.evidence,
            &format!("{} assertion_results[{index}].evidence", file_label),
            errors,
        );
        if result.passed && result.is_scored() && grading::evidence_is_trivial(&result.assertion, &result.evidence) {
            errors.push(ValidationError::for_field(
                format!("{} assertion_results[{index}].evidence", file_label),
                "passed assertions must include non-trivial evidence",
            ));
        }
    }

    if grading.summary.passed != passed {
        errors.push(ValidationError::for_field(
            format!("{} summary.passed", file_label),
            format!(
                "{} does not match {} passed assertion results",
                grading.summary.passed, passed
            ),
        ));
    }

    if grading.summary.failed != failed {
        errors.push(ValidationError::for_field(
            format!("{} summary.failed", file_label),
            format!(
                "{} does not match {} failed assertion results",
                grading.summary.failed, failed
            ),
        ));
    }

    if grading.summary.total != grading.assertion_results.len() {
        errors.push(ValidationError::for_field(
            format!("{} summary.total", file_label),
            format!(
                "{} does not match {} assertion results",
                grading.summary.total,
                grading.assertion_results.len()
            ),
        ));
    }

    if grading.summary.unsupported != counts.unsupported {
        errors.push(ValidationError::for_field(
            format!("{} summary.unsupported", file_label),
            format!(
                "{} does not match {} unsupported assertion results",
                grading.summary.unsupported, counts.unsupported
            ),
        ));
    }

    if grading.summary.excluded != counts.excluded {
        errors.push(ValidationError::for_field(
            format!("{} summary.excluded", file_label),
            format!(
                "{} does not match {} excluded assertion results",
                grading.summary.excluded, counts.excluded
            ),
        ));
    }

    if grading.summary.ungraded != counts.ungraded {
        errors.push(ValidationError::for_field(
            format!("{} summary.ungraded", file_label),
            format!(
                "{} does not match {} ungraded assertion results",
                grading.summary.ungraded, counts.ungraded
            ),
        ));
    }

    if !grading::pass_rate_matches(grading.summary.pass_rate, counts.pass_rate()) {
        errors.push(ValidationError::for_field(
            format!("{} summary.pass_rate", file_label),
            format!(
                "{} does not match computed pass rate {}",
                grading::describe_pass_rate(grading.summary.pass_rate),
                grading::describe_pass_rate(counts.pass_rate())
            ),
        ));
    }

    if options.fail_on_failed_assertions && failed > 0 {
        errors.push(ValidationError::for_field(
            file_label,
            format!("has {} failed assertion result(s)", failed),
        ));
    }
}

fn validate_timing_file(path: &Path, timing: &TimingFile, errors: &mut ValidationErrors) {
    let file_label = path.display().to_string();

    if matches!(timing.total_tokens, Some(0)) {
        errors.push(ValidationError::for_field(
            format!("{} total_tokens", file_label),
            "must be greater than zero when present",
        ));
    }

    if timing.duration_ms == 0 {
        errors.push(ValidationError::for_field(
            format!("{} duration_ms", file_label),
            "must be greater than zero",
        ));
    }
}

#[cfg(test)]
mod tests;
