use std::fmt;
use std::path::{Path, PathBuf};

use regex::Regex;
use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::call_bounds::CallBounds;
use super::evals::{EvalError, GlobPattern, NonEmptyString, RelativeSkillPath, Result};
use super::transcript::{NormalizedTranscript, ToolName};
use super::validation::ValidationError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(schema_with = "regex_pattern_schema")]
pub struct RegexPattern(String);

fn regex_pattern_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1,
        "format": "regex"
    })
}

impl RegexPattern {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, regex::Error> {
        let value = value.into();
        Regex::new(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn compile(&self) -> Regex {
        Regex::new(&self.0).expect("pattern was validated at construction")
    }

    pub fn compile_with(&self, flags: &RegexFlags) -> Regex {
        let mut builder = regex::RegexBuilder::new(&self.0);
        flags.apply(&mut builder);
        builder.build().expect("pattern was validated at construction")
    }
}

impl fmt::Display for RegexPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RegexPattern {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(|e| de::Error::custom(format!("invalid regex: {e}")))
    }
}

/// Regex flags, mapped one-to-one onto what the `regex` crate's builder
/// offers: `i` for case-insensitive, `m` for multi-line (`^`/`$` match at
/// line boundaries), and `s` for dot-matches-newline. Anything else is
/// rejected rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, JsonSchema)]
#[schemars(schema_with = "regex_flags_schema")]
pub struct RegexFlags(String);

fn regex_flags_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "pattern": "^[ims]*$"
    })
}

impl RegexFlags {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        if let Some(c) = value.chars().find(|c| !matches!(c, 'i' | 'm' | 's')) {
            return Err(format!("unsupported regex flag '{c}': only i, m, and s are supported"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn apply(&self, builder: &mut regex::RegexBuilder) {
        for flag in self.0.chars() {
            match flag {
                'i' => {
                    builder.case_insensitive(true);
                }
                'm' => {
                    builder.multi_line(true);
                }
                's' => {
                    builder.dot_matches_new_line(true);
                }
                other => unreachable!("flag '{other}' should have been rejected at construction"),
            }
        }
    }
}

impl fmt::Display for RegexFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RegexFlags {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// An exact number of regex matches a `regex` grader must find.
///
/// Zero is refused: `negate` without a `count` already asks for the pattern
/// to be absent, so `count: 0` would only be a second spelling of that same
/// check, in either polarity of `negate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(schema_with = "match_count_schema")]
pub struct MatchCount(usize);

fn match_count_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "integer",
        "minimum": 1
    })
}

impl MatchCount {
    pub fn parse(value: usize) -> std::result::Result<Self, String> {
        if value == 0 {
            return Err(
                "count 0 duplicates what negate without a count already means; drop count and use negate to ask for zero matches"
                    .to_string(),
            );
        }
        Ok(Self(value))
    }

    pub fn get(&self) -> usize {
        self.0
    }
}

impl fmt::Display for MatchCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<'de> Deserialize<'de> for MatchCount {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = usize::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MatchCase {
    #[default]
    Insensitive,
    Sensitive,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GradeTarget {
    #[default]
    FinalText,
    Transcript,
    AnyOutput,
    File(RelativeSkillPath),
    /// The set of files the run created under `outputs/`, one per line. Backed by
    /// the same index the report itself is built from, so "the agent created a
    /// file called X" is checkable without walking the directory a second time.
    CreatedFiles,
}

impl fmt::Display for GradeTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FinalText => f.write_str("final text"),
            Self::Transcript => f.write_str("transcript"),
            Self::AnyOutput => f.write_str("any output file"),
            Self::File(path) => write!(f, "file '{path}'"),
            Self::CreatedFiles => f.write_str("created files"),
        }
    }
}

/// Whether an `llm` grader's `target` came from the author or from the field's
/// default.
///
/// A bare `GradeTarget` cannot answer that question: `#[serde(default)]` fills
/// the field in either case, so by the time the grader is deserialized, an
/// omitted `target` and one written out as `final_text` look identical. That
/// distinction is exactly what decides whether the mechanical shortcut may
/// still fire ahead of the judge, so it has to survive deserialization as part
/// of the value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TargetDeclaration {
    #[default]
    Default,
    Named(GradeTarget),
}

impl TargetDeclaration {
    pub fn resolve(&self) -> GradeTarget {
        match self {
            Self::Default => GradeTarget::default(),
            Self::Named(target) => target.clone(),
        }
    }

    pub fn is_explicit(&self) -> bool {
        matches!(self, Self::Named(_))
    }
}

impl<'de> Deserialize<'de> for TargetDeclaration {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        GradeTarget::deserialize(deserializer).map(Self::Named)
    }
}

impl Serialize for TargetDeclaration {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.resolve().serialize(serializer)
    }
}

fn is_default_target(target: &TargetDeclaration) -> bool {
    !target.is_explicit()
}

/// Which arm of a with-skill versus without-skill comparison a grader is
/// allowed to score in.
///
/// A check that presupposes the skill can only ever pass where the skill is
/// present, so scoring it would credit the skill for its own premise and widen
/// the reported gap between the arms by exactly the number of such checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraderArm {
    /// Let the grader decide: one that presupposes the skill is reported but
    /// never scored, and everything else is scored.
    #[default]
    Auto,
    /// Report the result in both arms and score it in neither.
    WithOnly,
    /// Score in both arms even though the grader presupposes the skill, which
    /// is how a "the skill must not be engaged" expectation is written.
    Both,
}

impl GraderArm {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::WithOnly => "with_only",
            Self::Both => "both",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Grader {
    Regex {
        pattern: RegexPattern,
        #[serde(default)]
        target: GradeTarget,
        #[serde(default)]
        negate: bool,
        #[serde(default, skip_serializing_if = "RegexFlags::is_empty")]
        flags: RegexFlags,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<MatchCount>,
    },
    Contains {
        text: NonEmptyString,
        #[serde(default)]
        target: GradeTarget,
        #[serde(default)]
        case: MatchCase,
        #[serde(default)]
        negate: bool,
    },
    FileExists {
        path: GlobPattern,
        #[serde(default = "default_exists", skip_serializing_if = "is_default_exists")]
        exists: bool,
    },
    ToolUsed {
        tool: ToolName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_match: Option<RegexPattern>,
        #[serde(flatten)]
        calls: CallBounds,
    },
    ToolOrder {
        #[schemars(length(min = 2))]
        tools: Vec<ToolName>,
    },
    SkillUsed {
        #[serde(default, skip_serializing_if = "is_not_negated")]
        #[schemars(extend("default" = false))]
        negate: bool,
    },
    Llm {
        criterion: NonEmptyString,
        #[serde(default, skip_serializing_if = "is_default_target")]
        #[schemars(with = "GradeTarget", extend("default" = "final_text"))]
        target: TargetDeclaration,
    },
    ValidJson {
        #[serde(default)]
        target: GradeTarget,
    },
    SchemaValidation {
        schema: RelativeSkillPath,
        #[serde(default)]
        target: GradeTarget,
    },
}

impl Grader {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Regex { .. } => "regex",
            Self::Contains { .. } => "contains",
            Self::FileExists { .. } => "file_exists",
            Self::ToolUsed { .. } => "tool_used",
            Self::ToolOrder { .. } => "tool_order",
            Self::SkillUsed { .. } => "skill_used",
            Self::Llm { .. } => "llm",
            Self::ValidJson { .. } => "valid_json",
            Self::SchemaValidation { .. } => "schema_validation",
        }
    }

    /// A human-readable restatement, so a typed grader can still populate the
    /// `assertion` field that the grading artifact and its consumers expect.
    pub fn describe(&self) -> String {
        match self {
            Self::Regex {
                pattern,
                target,
                negate,
                flags,
                count,
            } => {
                let verb = if *negate { "does not match" } else { "matches" };
                match count {
                    Some(n) => format!("{target} {verb} regex /{pattern}/{flags} exactly {n} time(s)"),
                    None => format!("{target} {verb} regex /{pattern}/{flags}"),
                }
            }
            Self::Contains {
                text,
                target,
                case,
                negate,
            } => {
                let verb = if *negate { "does not contain" } else { "contains" };
                let suffix = match case {
                    MatchCase::Sensitive => " (case sensitive)",
                    MatchCase::Insensitive => "",
                };
                format!("{target} {verb} '{text}'{suffix}")
            }
            Self::FileExists { path, exists } => match exists {
                true => format!("file '{path}' exists"),
                false => format!("file '{path}' does not exist"),
            },
            Self::ToolUsed {
                tool,
                input_match,
                calls,
            } => {
                let counted = match calls.max() {
                    None if calls.min() == 1 => format!("tool '{tool}' was used"),
                    _ => format!("tool '{tool}' was used {calls}"),
                };
                match input_match {
                    Some(pattern) => format!("{counted}{}", naming(pattern)),
                    None => counted,
                }
            }
            Self::ToolOrder { tools } => {
                let names = tools.iter().map(ToolName::as_str).collect::<Vec<_>>().join(" then ");
                format!("tools were used in order: {names}")
            }
            Self::SkillUsed { negate } => match negate {
                true => "the skill was not engaged".to_string(),
                false => "the skill was engaged".to_string(),
            },
            Self::Llm { criterion, target } => match target.resolve() {
                GradeTarget::FinalText => criterion.to_string(),
                other => format!("{other}: {criterion}"),
            },
            Self::ValidJson { target } => format!("{target} is valid json"),
            Self::SchemaValidation { schema, target } => format!("{target} validates against schema '{schema}'"),
        }
    }

    pub fn needs_tool_visibility(&self) -> bool {
        matches!(
            self,
            Self::ToolUsed { .. } | Self::ToolOrder { .. } | Self::SkillUsed { .. }
        )
    }

    /// Whether the staging of the skill, rather than how well the run did the
    /// work it was asked to do, decides the verdict. Skill engagement is the
    /// harness-agnostic example, and the only one trg can recognise without
    /// being told: a tool-name check is specific to one harness's vocabulary, so
    /// it has to be marked by hand.
    ///
    /// A negated engagement check is settled by the arm just as much as a plain
    /// one. There is no skill to engage in the without-skill arm, so "not
    /// engaged" holds there for a run that did nothing at all.
    pub fn presupposes_the_skill(&self) -> bool {
        matches!(self, Self::SkillUsed { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(schema_with = "grader_name_schema")]
pub struct GraderName(String);

fn grader_name_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1
    })
}

impl GraderName {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err("grader name must not be empty".to_string());
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GraderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GraderName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// A grader as a case declares it, together with the arm scope that decides
/// whether its result counts toward the score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaseGrader {
    #[serde(flatten)]
    pub grader: Grader,
    #[serde(default, skip_serializing_if = "is_default_arm")]
    pub arm: GraderArm,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<GraderName>,
}

fn is_not_negated(negate: &bool) -> bool {
    !*negate
}

fn default_exists() -> bool {
    true
}

fn is_default_exists(exists: &bool) -> bool {
    *exists == default_exists()
}

fn is_default_arm(arm: &GraderArm) -> bool {
    *arm == GraderArm::default()
}

impl CaseGrader {
    pub fn new(grader: Grader) -> Self {
        Self {
            grader,
            arm: GraderArm::default(),
            name: None,
        }
    }

    pub fn counts_toward_score(&self) -> bool {
        self.exclusion_reason().is_none()
    }

    /// Why this grader is reported but not scored, when that is the case.
    pub fn exclusion_reason(&self) -> Option<String> {
        match self.arm {
            GraderArm::Both => None,
            GraderArm::WithOnly => Some(format!(
                "declared 'arm': '{}', so it is reported in both arms and scored in neither",
                GraderArm::WithOnly.as_str()
            )),
            GraderArm::Auto if self.grader.presupposes_the_skill() => Some(format!(
                "'{}' is settled by whether the skill was staged rather than by the run, so it is reported in both arms and scored in neither; declare 'arm': '{}' to score it anyway",
                self.grader.kind(),
                GraderArm::Both.as_str()
            )),
            GraderArm::Auto => None,
        }
    }
}

impl From<Grader> for CaseGrader {
    fn from(grader: Grader) -> Self {
        Self::new(grader)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraderOutcome {
    Passed {
        evidence: String,
    },
    Failed {
        evidence: String,
    },
    /// The property cannot be observed on this runner, so neither pass nor fail
    /// would be an honest answer.
    Unsupported {
        reason: String,
    },
    /// The grader is a judgement call and must be handed to the LLM grader.
    Deferred {
        criterion: String,
        target: TargetDeclaration,
    },
    /// The case, not the run, is malformed: a named schema is missing, unreadable, or
    /// not itself a valid JSON Schema document. Distinct from `Unsupported`, which
    /// means the property is unobservable on this particular runner; this means the
    /// assertion could never be checked on any runner until the case is fixed.
    AuthoringError {
        reason: String,
    },
}

impl GraderOutcome {
    fn from_bool(passed: bool, evidence: String) -> Self {
        if passed {
            Self::Passed { evidence }
        } else {
            Self::Failed { evidence }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GradeInput<'a> {
    pub final_text: &'a str,
    pub run_dir: &'a Path,
    pub workspace_dir: &'a Path,
    pub outputs_dir: &'a Path,
    pub raw_transcript: &'a str,
    pub transcript: Option<&'a NormalizedTranscript>,
    pub skill_dir: &'a Path,
    /// The paths this run created under `outputs/`, as already indexed for the
    /// report. Backs `GradeTarget::CreatedFiles` without a second directory walk.
    pub created_files: &'a [String],
}

/// The image formats a `file` target can hand the judge as a picture rather
/// than as text.
///
/// Detected from the extension, the same signal `MechanicalKind::ImageExists`
/// already trusts to decide whether a file is a picture at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMediaType {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageMediaType {
    fn from_extension(path: &Path) -> Option<Self> {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("png") => Some(Self::Png),
            Some("jpg") | Some("jpeg") => Some(Self::Jpeg),
            Some("gif") => Some(Self::Gif),
            Some("webp") => Some(Self::Webp),
            _ => None,
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetContent {
    Text(String),
    Image { media_type: ImageMediaType, bytes: Vec<u8> },
    Missing(String),
}

impl TargetContent {
    pub(crate) fn from_path(path: &Path) -> Self {
        if let Some(media_type) = ImageMediaType::from_extension(path) {
            return match std::fs::read(path) {
                Ok(bytes) => Self::Image { media_type, bytes },
                Err(e) => Self::Missing(format!("cannot read '{}': {e}", path.display())),
            };
        }
        match std::fs::read_to_string(path) {
            Ok(text) => Self::Text(text),
            Err(e) => Self::Missing(format!("cannot read '{}': {e}", path.display())),
        }
    }
}

impl<'a> GradeInput<'a> {
    /// Declared outputs win, then whatever the agent left in its working
    /// directory, then the run directory for files placed there out of band.
    /// An unfound path reports as the workspace candidate, since that is where
    /// the agent was asked to write.
    pub fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
        let path = path.as_ref();
        let workspace_candidate = self.workspace_dir.join(path);
        for candidate in [
            self.outputs_dir.join(path),
            workspace_candidate.clone(),
            self.run_dir.join(path),
        ] {
            if candidate.exists() {
                return candidate;
            }
        }
        workspace_candidate
    }

    pub(crate) fn target_content(&self, target: &GradeTarget) -> TargetContent {
        match target {
            GradeTarget::FinalText => TargetContent::Text(self.final_text.to_string()),
            GradeTarget::Transcript => TargetContent::Text(self.raw_transcript.to_string()),
            GradeTarget::File(path) => TargetContent::from_path(&self.resolve(path)),
            GradeTarget::AnyOutput => {
                let mut parts = vec![self.final_text.to_string()];
                collect_text(self.outputs_dir, &mut parts);
                TargetContent::Text(parts.join("\n"))
            }
            GradeTarget::CreatedFiles => TargetContent::Text(self.created_files.join("\n")),
        }
    }
}

fn collect_text(dir: &Path, parts: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_text(&path, parts);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            parts.push(text);
        }
    }
}

/// Walks each directory in priority order looking for a file whose path,
/// relative to that directory and written with `/` separators, matches the
/// glob's regex. Returns the first match.
fn find_glob_match(dirs: &[&Path], regex: &Regex) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| walk_for_glob_match(dir, dir, regex))
}

fn walk_for_glob_match(base: &Path, dir: &Path, regex: &Regex) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk_for_glob_match(base, &path, regex) {
                return Some(found);
            }
        } else if let Ok(relative) = path.strip_prefix(base) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if regex.is_match(&relative) {
                return Some(path);
            }
        }
    }
    None
}

pub fn evaluate(grader: &Grader, input: &GradeInput) -> GraderOutcome {
    match grader {
        Grader::Llm { criterion, target } => GraderOutcome::Deferred {
            criterion: criterion.to_string(),
            target: target.clone(),
        },
        Grader::FileExists { path, exists } => {
            let (found, evidence) = match path.as_literal_path() {
                Some(literal) => {
                    let resolved = input.resolve(literal);
                    match std::fs::metadata(&resolved) {
                        Ok(meta) if meta.is_file() => (
                            true,
                            format!("'{}' exists and holds {} bytes", resolved.display(), meta.len()),
                        ),
                        Ok(_) => (false, format!("'{}' exists but is not a file", resolved.display())),
                        Err(e) => (false, format!("'{}' could not be read: {e}", resolved.display())),
                    }
                }
                None => {
                    let regex = path.compile();
                    let dirs = [input.outputs_dir, input.workspace_dir, input.run_dir];
                    match find_glob_match(&dirs, &regex) {
                        Some(matched) => (true, format!("'{}' matches glob '{path}'", matched.display())),
                        None => (false, format!("no file matches glob '{path}'")),
                    }
                }
            };
            GraderOutcome::from_bool(found == *exists, evidence)
        }
        Grader::Regex {
            pattern,
            target,
            negate,
            flags,
            count,
        } => match input.target_content(target) {
            TargetContent::Missing(reason) => GraderOutcome::Failed { evidence: reason },
            TargetContent::Image { .. } => GraderOutcome::Failed {
                evidence: format!("{target} is an image; a pattern can only match text"),
            },
            TargetContent::Text(text) => {
                let regex = pattern.compile_with(flags);
                match count {
                    Some(expected) => {
                        let actual = regex.find_iter(&text).count();
                        let evidence = format!(
                            "{target} matched /{pattern}/{flags} {actual} time(s), expected exactly {expected}"
                        );
                        GraderOutcome::from_bool((actual == expected.get()) != *negate, evidence)
                    }
                    None => {
                        let found = regex.find(&text);
                        let evidence = match &found {
                            Some(m) => format!(
                                "{target} matched /{pattern}/{flags} at byte {}: '{}'",
                                m.start(),
                                m.as_str()
                            ),
                            None => format!("{target} did not match /{pattern}/{flags}"),
                        };
                        GraderOutcome::from_bool(found.is_some() != *negate, evidence)
                    }
                }
            }
        },
        Grader::Contains {
            text,
            target,
            case,
            negate,
        } => match input.target_content(target) {
            TargetContent::Missing(reason) => GraderOutcome::Failed { evidence: reason },
            TargetContent::Image { .. } => GraderOutcome::Failed {
                evidence: format!("{target} is an image; it cannot be searched for text"),
            },
            TargetContent::Text(haystack) => {
                let found = match case {
                    MatchCase::Sensitive => haystack.find(text.as_str()),
                    MatchCase::Insensitive => haystack.to_lowercase().find(&text.as_str().to_lowercase()),
                };
                let evidence = match found {
                    Some(offset) => format!("{target} ({} bytes) contains '{text}' at byte {offset}", haystack.len()),
                    None => format!("{target} ({} bytes) does not contain '{text}'", haystack.len()),
                };
                GraderOutcome::from_bool(found.is_some() != *negate, evidence)
            }
        },
        Grader::ToolUsed {
            tool,
            input_match,
            calls,
        } => with_transcript(input, |transcript| {
            let observed = matching_call_count(transcript, tool, input_match.as_ref());
            let qualifier = input_match.as_ref().map(naming).unwrap_or_default();
            GraderOutcome::from_bool(
                calls.admits(observed),
                format!("'{tool}' was called {observed} time(s){qualifier}, expected {calls}"),
            )
        }),
        Grader::ToolOrder { tools } => with_transcript(input, |transcript| {
            let observed = transcript.tool_sequence();
            let matched = is_subsequence(tools, &observed);
            let rendered = observed.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ");
            let verb = if matched { "contains" } else { "does not contain" };
            GraderOutcome::from_bool(
                matched,
                format!("observed tool order [{rendered}] {verb} the expected order"),
            )
        }),
        Grader::SkillUsed { negate } => with_transcript(input, |transcript| {
            let engagement = transcript.skill_engagement();
            match engagement.engaged() {
                Some(engaged) => GraderOutcome::from_bool(engaged != *negate, engagement.describe().to_string()),
                None => GraderOutcome::Unsupported {
                    reason: unobservable_reason(transcript),
                },
            }
        }),
        Grader::ValidJson { target } => {
            let (passed, evidence) = check_json_validity(&input.target_content(target));
            GraderOutcome::from_bool(passed, evidence)
        }
        Grader::SchemaValidation { schema, target } => {
            let schema_path = input.skill_dir.join(schema.as_path());
            match check_schema_validation(&schema_path, &input.target_content(target)) {
                Ok((passed, evidence)) => GraderOutcome::from_bool(passed, evidence),
                Err(e) => GraderOutcome::AuthoringError { reason: e.to_string() },
            }
        }
    }
}

/// Whether a target parses as JSON, shared by the typed `valid_json` grader and the
/// prose sniffer so the two rules can never drift apart.
pub(crate) fn check_json_validity(content: &TargetContent) -> (bool, String) {
    match content {
        TargetContent::Missing(reason) => (false, reason.clone()),
        TargetContent::Image { .. } => (
            false,
            "target is an image; json validity is checked against text".to_string(),
        ),
        TargetContent::Text(text) => match serde_json::from_str::<serde_json::Value>(text) {
            Ok(_) => (true, "parses as valid json".to_string()),
            Err(e) => (
                false,
                format!("invalid json at line {} column {}: {e}", e.line(), e.column()),
            ),
        },
    }
}

/// Whether a target validates against a named JSON Schema document, shared by the
/// typed `schema_validation` grader and the prose sniffer. A schema that cannot be
/// read or is not itself a valid schema is an authoring error, not a failed
/// assertion, so it surfaces as `Err` rather than as `Ok((false, _))`.
///
/// The schema is opened before the target is looked at, and the order is the whole
/// guarantee. Judging the target first means a suite naming a schema that does not
/// exist reports the skill failing for as long as the run happens to produce nothing
/// parseable, and only admits to the typo once the skill starts passing.
pub(crate) fn check_schema_validation(schema_path: &Path, content: &TargetContent) -> Result<(bool, String)> {
    #[cfg(any(feature = "schema-validation", test))]
    {
        let schema_text = std::fs::read_to_string(schema_path).map_err(|e| {
            EvalError::Validation(
                ValidationError::for_field(
                    format!("schema '{}'", schema_path.display()),
                    format!("cannot read: {e}"),
                )
                .into(),
            )
        })?;
        let schema_value: serde_json::Value = serde_json::from_str(&schema_text).map_err(|e| {
            EvalError::Validation(
                ValidationError::for_field(
                    format!("schema '{}'", schema_path.display()),
                    format!("is not valid json: {e}"),
                )
                .into(),
            )
        })?;
        let validator = jsonschema::validator_for(&schema_value).map_err(|e| {
            EvalError::Validation(
                ValidationError::for_field(
                    format!("schema '{}'", schema_path.display()),
                    format!("is not a valid json schema document: {e}"),
                )
                .into(),
            )
        })?;

        let text = match content {
            TargetContent::Missing(reason) => return Ok((false, reason.clone())),
            TargetContent::Image { .. } => {
                return Ok((
                    false,
                    "target is an image; schema validation is checked against text".to_string(),
                ))
            }
            TargetContent::Text(text) => text,
        };
        let target: serde_json::Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(e) => {
                return Ok((
                    false,
                    format!("invalid json at line {} column {}: {e}", e.line(), e.column()),
                ))
            }
        };

        let errors: Vec<String> = validator.iter_errors(&target).map(|e| e.to_string()).collect();
        if errors.is_empty() {
            Ok((true, format!("validates against schema '{}'", schema_path.display())))
        } else {
            Ok((
                false,
                format!(
                    "does not validate against schema '{}': {}",
                    schema_path.display(),
                    errors.join("; ")
                ),
            ))
        }
    }

    #[cfg(not(any(feature = "schema-validation", test)))]
    {
        let _ = content;
        Err(EvalError::Validation(
            ValidationError::for_field(
                format!("schema '{}'", schema_path.display()),
                "schema validation requires this build's schema-validation feature",
            )
            .into(),
        ))
    }
}

/// How many of a run's calls to a tool named something the case asked for.
///
/// Without a pattern the count is every call to the tool, which says no more than
/// "some tool ran". The pattern is what makes "`npm test` ran" expressible, and it
/// is tested against the values the call named, one at a time: the paths it was
/// given, the command lines it ran, and the texts it searched for.
///
/// Matching those values rather than the harness's own JSON input is what keeps a
/// case portable. The key a command arrives under, and whether a skill is even
/// reached through a tool call, differ per harness, so a pattern written against
/// one harness's request body would quietly match nothing under another.
fn matching_call_count(
    transcript: &NormalizedTranscript,
    tool: &ToolName,
    input_match: Option<&RegexPattern>,
) -> usize {
    let Some(pattern) = input_match else {
        return transcript.tool_call_count(tool.as_str());
    };
    let matcher = pattern.compile();
    transcript
        .tool_call_inputs()
        .filter(|(called, _)| called.eq_ignore_case(tool.as_str()))
        .filter(|(_, named)| named.iter().any(|value| matcher.is_match(value)))
        .count()
}

fn naming(pattern: &RegexPattern) -> String {
    format!(" with an argument matching /{pattern}/")
}

fn with_transcript(input: &GradeInput, grade: impl FnOnce(&NormalizedTranscript) -> GraderOutcome) -> GraderOutcome {
    let Some(transcript) = input.transcript else {
        return GraderOutcome::Unsupported {
            reason: "no normalized transcript was recorded for this run".to_string(),
        };
    };
    if !transcript.tool_visibility.is_observed() {
        return GraderOutcome::Unsupported {
            reason: unobservable_reason(transcript),
        };
    }
    grade(transcript)
}

fn unobservable_reason(transcript: &NormalizedTranscript) -> String {
    format!(
        "runner '{}' does not expose tool calls in a form trg can read, so this property cannot be graded here",
        transcript.runner
    )
}

fn is_subsequence(expected: &[ToolName], observed: &[&ToolName]) -> bool {
    let mut cursor = observed.iter();
    expected
        .iter()
        .all(|wanted| cursor.any(|seen| seen.eq_ignore_case(wanted.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::redact::redact_transcript_bytes;
    use crate::agentskills::transcript::{normalize_stream_json, TranscriptFormat, WorkspaceBoundary};

    const CLAUDE_STREAM: &[u8] = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":".skill/SKILL.md"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/report.md"}}]}}
{"type":"result","is_error":false,"result":"done"}
"#;

    struct Fixture {
        _dir: tempfile::TempDir,
        run_dir: PathBuf,
        workspace_dir: PathBuf,
        outputs_dir: PathBuf,
        skill_dir: PathBuf,
        transcript: NormalizedTranscript,
    }

    impl Fixture {
        fn new() -> Self {
            Self::of(CLAUDE_STREAM)
        }

        fn of(stream: &[u8]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let run_dir = dir.path().join("run");
            let workspace_dir = run_dir.join("workspace");
            let outputs_dir = workspace_dir.join("outputs");
            let skill_dir = dir.path().join("skill");
            std::fs::create_dir_all(&outputs_dir).unwrap();
            std::fs::create_dir_all(&skill_dir).unwrap();
            std::fs::write(outputs_dir.join("report.md"), "# Title\nRevenue grew 12% in Q3.\n").unwrap();
            Self {
                _dir: dir,
                run_dir,
                workspace_dir,
                outputs_dir,
                skill_dir,
                transcript: normalize_stream_json(
                    "claude",
                    &redact_transcript_bytes(stream),
                    &WorkspaceBoundary::unknown(),
                ),
            }
        }

        fn write_to_workspace(&self, name: &str, contents: &str) {
            std::fs::write(self.workspace_dir.join(name), contents).unwrap();
        }

        fn write_to_skill(&self, name: &str, contents: &str) {
            std::fs::write(self.skill_dir.join(name), contents).unwrap();
        }

        fn input(&self) -> GradeInput<'_> {
            GradeInput {
                final_text: "Wrote the report to outputs/report.md",
                run_dir: &self.run_dir,
                workspace_dir: &self.workspace_dir,
                outputs_dir: &self.outputs_dir,
                raw_transcript: "",
                transcript: Some(&self.transcript),
                skill_dir: &self.skill_dir,
                created_files: &[],
            }
        }
    }

    fn path(value: &str) -> RelativeSkillPath {
        serde_json::from_value(serde_json::json!(value)).unwrap()
    }

    fn glob(value: &str) -> GlobPattern {
        serde_json::from_value(serde_json::json!(value)).unwrap()
    }

    fn text(value: &str) -> NonEmptyString {
        serde_json::from_value(serde_json::json!(value)).unwrap()
    }

    fn tool(value: &str) -> ToolName {
        ToolName::new(value).unwrap()
    }

    #[test]
    fn regex_pattern_is_rejected_at_parse_time() {
        let err = serde_json::from_value::<Grader>(serde_json::json!({
            "type": "regex",
            "pattern": "([unclosed"
        }))
        .unwrap_err();
        assert!(err.to_string().contains("invalid regex"), "{err}");
    }

    #[test]
    fn regex_grades_a_named_output_file() {
        let fixture = Fixture::new();
        let grader = Grader::Regex {
            pattern: RegexPattern::parse(r"\d+%").unwrap(),
            target: GradeTarget::File(path("report.md")),
            negate: false,
            flags: RegexFlags::default(),
            count: None,
        };
        let outcome = evaluate(&grader, &fixture.input());
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");
    }

    #[test]
    fn a_case_insensitive_flag_matches_regardless_of_case() {
        let fixture = Fixture::new();
        let grader = Grader::Regex {
            pattern: RegexPattern::parse("TITLE").unwrap(),
            target: GradeTarget::File(path("report.md")),
            negate: false,
            flags: RegexFlags::parse("i").unwrap(),
            count: None,
        };
        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn without_the_flag_the_same_pattern_fails_on_case() {
        let fixture = Fixture::new();
        let grader = Grader::Regex {
            pattern: RegexPattern::parse("TITLE").unwrap(),
            target: GradeTarget::File(path("report.md")),
            negate: false,
            flags: RegexFlags::default(),
            count: None,
        };
        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Failed { .. }
        ));
    }

    #[test]
    fn an_unsupported_flag_is_rejected_at_parse_time() {
        let err = RegexFlags::parse("x").unwrap_err();
        assert!(err.contains("unsupported regex flag"), "{err}");
    }

    #[test]
    fn count_requires_an_exact_number_of_matches() {
        let fixture = Fixture::new();
        let grader = Grader::Regex {
            pattern: RegexPattern::parse("e").unwrap(),
            target: GradeTarget::File(path("report.md")),
            negate: false,
            flags: RegexFlags::default(),
            count: Some(MatchCount::parse(1).unwrap()),
        };
        // "# Title\nRevenue grew 12% in Q3.\n" has more than one 'e'.
        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Failed { .. }
        ));
    }

    #[test]
    fn absence_of_the_pattern_is_expressed_with_negate_not_count_zero() {
        let fixture = Fixture::new();
        let grader = Grader::Regex {
            pattern: RegexPattern::parse("zzz").unwrap(),
            target: GradeTarget::File(path("report.md")),
            negate: true,
            flags: RegexFlags::default(),
            count: None,
        };
        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn count_zero_is_rejected_at_parse_time() {
        let err = MatchCount::parse(0).unwrap_err();
        assert!(err.contains("negate"), "{err}");
    }

    #[test]
    fn negate_with_count_asks_for_anything_other_than_that_count() {
        let fixture = Fixture::new();
        let grader = Grader::Regex {
            pattern: RegexPattern::parse("e").unwrap(),
            target: GradeTarget::File(path("report.md")),
            negate: true,
            flags: RegexFlags::default(),
            count: Some(MatchCount::parse(1).unwrap()),
        };
        // "# Title\nRevenue grew 12% in Q3.\n" has more than one 'e', so negate
        // flips the "not exactly 1" mismatch into a pass.
        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn negate_inverts_the_verdict_and_the_evidence_stays_factual() {
        let fixture = Fixture::new();
        let grader = Grader::Contains {
            text: text("TODO"),
            target: GradeTarget::File(path("report.md")),
            case: MatchCase::Sensitive,
            negate: true,
        };
        match evaluate(&grader, &fixture.input()) {
            GraderOutcome::Passed { evidence } => assert!(evidence.contains("does not contain"), "{evidence}"),
            other => panic!("expected pass, got {other:?}"),
        }
    }

    #[test]
    fn missing_target_file_fails_rather_than_erroring() {
        let fixture = Fixture::new();
        let grader = Grader::Contains {
            text: text("anything"),
            target: GradeTarget::File(path("absent.md")),
            case: MatchCase::Insensitive,
            negate: false,
        };
        match evaluate(&grader, &fixture.input()) {
            GraderOutcome::Failed { evidence } => assert!(evidence.contains("cannot read"), "{evidence}"),
            other => panic!("expected fail, got {other:?}"),
        }
    }

    #[test]
    fn file_exists_resolves_relative_to_outputs_first() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("report.md"),
                exists: true,
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("nope.md"),
                exists: true,
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn file_exists_finds_what_the_agent_wrote_in_its_working_directory() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("summary.md", "May revenue was up.\n");

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("summary.md"),
                exists: true,
            },
            &fixture.input(),
        );
        match outcome {
            GraderOutcome::Passed { evidence } => assert!(evidence.contains("workspace"), "{evidence}"),
            other => panic!("expected pass, got {other:?}"),
        }
    }

    #[test]
    fn an_unfound_path_is_reported_against_the_workspace_not_the_run_directory() {
        let fixture = Fixture::new();
        match evaluate(
            &Grader::FileExists {
                path: glob("nope.md"),
                exists: true,
            },
            &fixture.input(),
        ) {
            GraderOutcome::Failed { evidence } => assert!(evidence.contains("workspace/nope.md"), "{evidence}"),
            other => panic!("expected fail, got {other:?}"),
        }
    }

    #[test]
    fn exists_false_passes_when_the_file_is_absent() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("nope.md"),
                exists: false,
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");
    }

    #[test]
    fn exists_false_fails_when_the_file_is_present() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("report.md"),
                exists: false,
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn a_glob_matches_a_file_under_outputs() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("*.md"),
                exists: true,
            },
            &fixture.input(),
        );
        match outcome {
            GraderOutcome::Passed { evidence } => assert!(evidence.contains("report.md"), "{evidence}"),
            other => panic!("expected pass, got {other:?}"),
        }
    }

    #[test]
    fn a_glob_that_matches_nothing_fails() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("*.json"),
                exists: true,
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn a_double_star_glob_matches_across_directories() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("summary.md", "notes\n");
        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("**/*.md"),
                exists: true,
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");
    }

    #[test]
    fn any_output_target_spans_final_text_and_files() {
        let fixture = Fixture::new();
        let grader = Grader::Contains {
            text: text("Revenue grew"),
            target: GradeTarget::AnyOutput,
            case: MatchCase::Sensitive,
            negate: false,
        };
        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn tool_used_counts_calls_against_the_minimum() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::ToolUsed {
                tool: tool("read"),
                input_match: None,
                calls: CallBounds::at_least_once(),
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(
            &Grader::ToolUsed {
                tool: tool("Read"),
                input_match: None,
                calls: CallBounds::parse(2, None).unwrap(),
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    /// A tool name alone says "some tool ran". Which command ran is what a case about
    /// routing actually asserts, so only the calls naming it are counted.
    #[test]
    fn only_the_calls_that_named_what_a_case_asked_for_are_counted() {
        let fixture = Fixture::of(
            br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"npm test"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}]}}
{"type":"result","is_error":false,"result":"done"}
"#,
        );

        let ran_the_suite: Grader = serde_json::from_value(serde_json::json!({
            "type": "tool_used",
            "tool": "Bash",
            "input_match": "npm test"
        }))
        .unwrap();
        assert!(matches!(
            evaluate(&ran_the_suite, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));

        let ran_it_twice: Grader = serde_json::from_value(serde_json::json!({
            "type": "tool_used",
            "tool": "Bash",
            "input_match": "npm test",
            "min_calls": 2
        }))
        .unwrap();
        match evaluate(&ran_it_twice, &fixture.input()) {
            GraderOutcome::Failed { evidence, .. } => {
                assert!(evidence.contains("called 1 time(s)"), "{evidence}");
                assert!(evidence.contains("matching /npm test/"), "{evidence}");
            }
            other => panic!("two calls were not made, got {other:?}"),
        }
    }

    /// Over-triggering is a claim about one command, not about the tool: a skill may
    /// legitimately reach for a shell and still must not reach for this.
    #[test]
    fn a_command_a_case_forbids_leaves_the_tool_free_for_anything_else() {
        let grader: Grader = serde_json::from_value(serde_json::json!({
            "type": "tool_used",
            "tool": "Bash",
            "input_match": "rm -rf",
            "min_calls": 0,
            "max_calls": 0
        }))
        .unwrap();

        assert!(matches!(
            evaluate(&grader, &Fixture::new().input()),
            GraderOutcome::Passed { .. }
        ));

        let deleted = Fixture::of(
            br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"rm -rf outputs"}}]}}
{"type":"result","is_error":false,"result":"done"}
"#,
        );
        assert!(matches!(
            evaluate(&grader, &deleted.input()),
            GraderOutcome::Failed { .. }
        ));
    }

    #[test]
    fn an_input_pattern_is_rejected_at_parse_time() {
        let err = serde_json::from_value::<Grader>(serde_json::json!({
            "type": "tool_used",
            "tool": "Bash",
            "input_match": "["
        }))
        .unwrap_err();

        assert!(err.to_string().contains("invalid regex"), "{err}");
    }

    #[test]
    fn tool_order_matches_a_subsequence_not_an_exact_run() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::ToolOrder {
                tools: vec![tool("Read"), tool("Write")],
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(
            &Grader::ToolOrder {
                tools: vec![tool("Write"), tool("Read")],
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn skill_used_passes_on_a_staged_path_reference() {
        let fixture = Fixture::new();
        assert!(matches!(
            evaluate(&Grader::SkillUsed { negate: false }, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn tool_graders_are_unsupported_on_an_unobservable_runner() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = NormalizedTranscript::unavailable("mystery-runner");
        let input = GradeInput {
            final_text: "done",
            run_dir: dir.path(),
            workspace_dir: dir.path(),
            outputs_dir: dir.path(),
            raw_transcript: "",
            transcript: Some(&transcript),
            skill_dir: dir.path(),
            created_files: &[],
        };

        for grader in [
            Grader::SkillUsed { negate: false },
            Grader::ToolUsed {
                tool: tool("Read"),
                input_match: None,
                calls: CallBounds::at_least_once(),
            },
            Grader::ToolOrder {
                tools: vec![tool("Read"), tool("Write")],
            },
        ] {
            match evaluate(&grader, &input) {
                GraderOutcome::Unsupported { reason } => assert!(reason.contains("mystery-runner"), "{reason}"),
                other => panic!("{} should be unsupported, got {other:?}", grader.kind()),
            }
        }
    }

    #[test]
    fn skill_used_reads_the_shell_command_codex_ran() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
            "codex",
            &redact_transcript_bytes(br#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"sed -n '1,240p' .skill/SKILL.md\""}}
{"type":"turn.completed"}"#),
            &WorkspaceBoundary::unknown(),
        );
        let input = GradeInput {
            final_text: "done",
            run_dir: dir.path(),
            workspace_dir: dir.path(),
            outputs_dir: dir.path(),
            raw_transcript: "",
            transcript: Some(&transcript),
            skill_dir: dir.path(),
            created_files: &[],
        };

        assert!(matches!(
            evaluate(&Grader::SkillUsed { negate: false }, &input),
            GraderOutcome::Passed { .. }
        ));
        assert!(matches!(
            evaluate(
                &Grader::ToolUsed {
                    tool: tool("command_execution"),
                    input_match: None,
                    calls: CallBounds::at_least_once(),
                },
                &input
            ),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn text_graders_still_work_on_an_unobservable_runner() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = NormalizedTranscript::unavailable("mystery-runner");
        let input = GradeInput {
            final_text: "the answer is 42",
            run_dir: dir.path(),
            workspace_dir: dir.path(),
            outputs_dir: dir.path(),
            raw_transcript: "",
            transcript: Some(&transcript),
            skill_dir: dir.path(),
            created_files: &[],
        };
        let grader = Grader::Regex {
            pattern: RegexPattern::parse(r"\b42\b").unwrap(),
            target: GradeTarget::FinalText,
            negate: false,
            flags: RegexFlags::default(),
            count: None,
        };
        assert!(matches!(evaluate(&grader, &input), GraderOutcome::Passed { .. }));
    }

    #[test]
    fn absent_transcript_is_unsupported_not_failed() {
        let dir = tempfile::tempdir().unwrap();
        let input = GradeInput {
            final_text: "done",
            run_dir: dir.path(),
            workspace_dir: dir.path(),
            outputs_dir: dir.path(),
            raw_transcript: "",
            transcript: None,
            skill_dir: dir.path(),
            created_files: &[],
        };
        match evaluate(&Grader::SkillUsed { negate: false }, &input) {
            GraderOutcome::Unsupported { reason } => assert!(reason.contains("no normalized transcript"), "{reason}"),
            other => panic!("expected unsupported, got {other:?}"),
        }
    }

    fn declared(json: serde_json::Value) -> CaseGrader {
        serde_json::from_value(json).expect("grader parses")
    }

    #[test]
    fn a_skill_engagement_check_is_reported_but_never_scored() {
        let grader = declared(serde_json::json!({"type": "skill_used"}));

        assert_eq!(grader.arm, GraderArm::Auto);
        assert!(!grader.counts_toward_score());
        assert!(grader
            .exclusion_reason()
            .expect("a reason is recorded")
            .contains("settled by whether the skill was staged"));
    }

    #[test]
    fn declaring_both_arms_scores_a_check_that_presupposes_the_skill() {
        let grader = declared(serde_json::json!({"type": "skill_used", "arm": "both"}));

        assert!(grader.counts_toward_score());
        assert_eq!(grader.exclusion_reason(), None);
    }

    #[test]
    fn a_check_about_the_work_itself_is_scored_in_both_arms() {
        let grader = declared(serde_json::json!({"type": "contains", "text": "done"}));

        assert!(grader.counts_toward_score());
    }

    #[test]
    fn with_only_takes_any_check_out_of_the_score() {
        let grader = declared(serde_json::json!({
            "type": "tool_used",
            "tool": "Skill",
            "arm": "with_only"
        }));

        assert!(!grader.counts_toward_score());
        assert!(grader
            .exclusion_reason()
            .expect("a reason is recorded")
            .contains("scored in neither"));
    }

    #[test]
    fn the_declared_arm_round_trips_and_stays_out_of_the_manifest_when_left_alone() {
        let plain = declared(serde_json::json!({"type": "skill_used"}));
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            serde_json::json!({"type": "skill_used"})
        );

        let marked = declared(serde_json::json!({"type": "skill_used", "arm": "with_only"}));
        assert_eq!(
            serde_json::to_value(&marked).unwrap(),
            serde_json::json!({"type": "skill_used", "arm": "with_only"})
        );
    }

    #[test]
    fn llm_grader_defers_rather_than_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let input = GradeInput {
            final_text: "done",
            run_dir: dir.path(),
            workspace_dir: dir.path(),
            outputs_dir: dir.path(),
            raw_transcript: "",
            transcript: None,
            skill_dir: dir.path(),
            created_files: &[],
        };
        let grader = Grader::Llm {
            criterion: text("The summary avoids filler phrasing"),
            target: TargetDeclaration::default(),
        };
        assert_eq!(
            evaluate(&grader, &input),
            GraderOutcome::Deferred {
                criterion: "The summary avoids filler phrasing".to_string(),
                target: TargetDeclaration::Default,
            }
        );
    }

    /// A declared target on an `llm` grader must survive to the deferred
    /// outcome, since that is what tells the judge what to look at.
    #[test]
    fn llm_grader_carries_its_declared_target_into_the_deferred_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let input = GradeInput {
            final_text: "done",
            run_dir: dir.path(),
            workspace_dir: dir.path(),
            outputs_dir: dir.path(),
            raw_transcript: "the agent checked the config twice",
            transcript: None,
            skill_dir: dir.path(),
            created_files: &[],
        };
        let grader = Grader::Llm {
            criterion: text("the agent checked the existing config before overwriting it"),
            target: TargetDeclaration::Named(GradeTarget::Transcript),
        };
        assert_eq!(
            evaluate(&grader, &input),
            GraderOutcome::Deferred {
                criterion: "the agent checked the existing config before overwriting it".to_string(),
                target: TargetDeclaration::Named(GradeTarget::Transcript),
            }
        );
    }

    #[test]
    fn created_files_target_lists_what_the_run_produced() {
        let fixture = Fixture::new();
        let created = vec!["report.md".to_string(), "sub/data.json".to_string()];
        let input = GradeInput {
            created_files: &created,
            ..fixture.input()
        };
        let grader = Grader::Contains {
            text: text("sub/data.json"),
            target: GradeTarget::CreatedFiles,
            case: MatchCase::Sensitive,
            negate: false,
        };
        assert!(matches!(evaluate(&grader, &input), GraderOutcome::Passed { .. }));

        let missing = Grader::Contains {
            text: text("never-written.txt"),
            target: GradeTarget::CreatedFiles,
            case: MatchCase::Sensitive,
            negate: false,
        };
        assert!(matches!(evaluate(&missing, &input), GraderOutcome::Failed { .. }));
    }

    #[test]
    fn llm_grader_description_is_unchanged_by_the_default_target_but_names_a_declared_one() {
        let default_target = Grader::Llm {
            criterion: text("the summary avoids filler phrasing"),
            target: TargetDeclaration::default(),
        };
        assert_eq!(default_target.describe(), "the summary avoids filler phrasing");

        let declared_target = Grader::Llm {
            criterion: text("the summary avoids filler phrasing"),
            target: TargetDeclaration::Named(GradeTarget::Transcript),
        };
        assert_eq!(
            declared_target.describe(),
            "transcript: the summary avoids filler phrasing"
        );
    }

    #[test]
    fn every_grader_describes_itself_for_the_grading_artifact() {
        let cases = [
            (
                Grader::Regex {
                    pattern: RegexPattern::parse("foo").unwrap(),
                    target: GradeTarget::FinalText,
                    negate: false,
                    flags: RegexFlags::default(),
                    count: None,
                },
                "final text matches regex /foo/",
            ),
            (
                Grader::Contains {
                    text: text("bar"),
                    target: GradeTarget::File(path("out.md")),
                    case: MatchCase::Insensitive,
                    negate: true,
                },
                "file 'out.md' does not contain 'bar'",
            ),
            (
                Grader::FileExists {
                    path: glob("out.md"),
                    exists: true,
                },
                "file 'out.md' exists",
            ),
            (
                Grader::ToolUsed {
                    tool: tool("Read"),
                    input_match: None,
                    calls: CallBounds::at_least_once(),
                },
                "tool 'Read' was used",
            ),
            (
                Grader::ToolUsed {
                    tool: tool("Bash"),
                    input_match: Some(RegexPattern::parse("npm test").unwrap()),
                    calls: CallBounds::never(),
                },
                "tool 'Bash' was used never with an argument matching /npm test/",
            ),
            (
                Grader::ToolOrder {
                    tools: vec![tool("Read"), tool("Write")],
                },
                "tools were used in order: Read then Write",
            ),
            (Grader::SkillUsed { negate: false }, "the skill was engaged"),
            (Grader::SkillUsed { negate: true }, "the skill was not engaged"),
            (
                Grader::ValidJson {
                    target: GradeTarget::FinalText,
                },
                "final text is valid json",
            ),
            (
                Grader::SchemaValidation {
                    schema: path("schema.json"),
                    target: GradeTarget::File(path("out.json")),
                },
                "file 'out.json' validates against schema 'schema.json'",
            ),
        ];
        for (grader, expected) in cases {
            assert_eq!(grader.describe(), expected, "{}", grader.kind());
        }
    }

    #[test]
    fn graders_round_trip_through_the_manifest_representation() {
        let json = serde_json::json!([
            {"type": "regex", "pattern": "\\d+%", "target": {"file": "report.md"}},
            {"type": "contains", "text": "summary", "case": "sensitive"},
            {"type": "file_exists", "path": "outputs/report.md"},
            {"type": "tool_used", "tool": "Read", "min_calls": 2},
            {"type": "tool_order", "tools": ["Read", "Write"]},
            {"type": "skill_used"},
            {"type": "llm", "criterion": "reads naturally"},
            {"type": "valid_json"},
            {"type": "schema_validation", "schema": "schema.json"}
        ]);
        let graders: Vec<Grader> = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(graders.len(), 9);
        assert_eq!(
            graders.iter().map(Grader::kind).collect::<Vec<_>>(),
            vec![
                "regex",
                "contains",
                "file_exists",
                "tool_used",
                "tool_order",
                "skill_used",
                "llm",
                "valid_json",
                "schema_validation"
            ]
        );
        let reparsed: Vec<Grader> = serde_json::from_value(serde_json::to_value(&graders).unwrap()).unwrap();
        assert_eq!(reparsed, graders);
    }

    #[test]
    fn a_lower_bound_of_zero_alone_is_rejected_as_a_check_that_cannot_fail() {
        let err = serde_json::from_value::<Grader>(serde_json::json!({
            "type": "tool_used",
            "tool": "Read",
            "min_calls": 0
        }))
        .unwrap_err();
        assert!(err.to_string().contains("max_calls"), "{err}");
    }

    /// The only way to state that a tool must not be reached for, which is how a skill
    /// answering a prompt it was never meant to answer gets caught.
    #[test]
    fn a_tool_a_case_forbids_fails_the_moment_it_is_called() {
        let grader: Grader = serde_json::from_value(serde_json::json!({
            "type": "tool_used",
            "tool": "Read",
            "min_calls": 0,
            "max_calls": 0
        }))
        .unwrap();
        let fixture = Fixture::new();

        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Failed { .. }
        ));
        assert_eq!(grader.describe(), "tool 'Read' was used never");
    }

    /// Over-triggering is the mirror of not triggering, and until now only one of the
    /// two could be stated.
    #[test]
    fn a_skill_that_must_not_be_engaged_fails_on_the_run_that_engaged_it() {
        let fixture = Fixture::new();

        assert!(matches!(
            evaluate(&Grader::SkillUsed { negate: false }, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
        assert!(matches!(
            evaluate(&Grader::SkillUsed { negate: true }, &fixture.input()),
            GraderOutcome::Failed { .. }
        ));
    }

    #[test]
    fn valid_json_passes_when_the_target_parses() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("data.json", r#"{"ok": true}"#);
        let grader = Grader::ValidJson {
            target: GradeTarget::File(path("data.json")),
        };

        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn valid_json_fails_with_the_parse_location_when_the_target_does_not_parse() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("data.json", "{not json");
        let grader = Grader::ValidJson {
            target: GradeTarget::File(path("data.json")),
        };

        match evaluate(&grader, &fixture.input()) {
            GraderOutcome::Failed { evidence } => assert!(evidence.contains("line"), "{evidence}"),
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn schema_validation_passes_when_the_target_conforms() {
        let fixture = Fixture::new();
        fixture.write_to_skill(
            "schema.json",
            r#"{"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}}"#,
        );
        fixture.write_to_workspace("data.json", r#"{"name": "trg"}"#);
        let grader = Grader::SchemaValidation {
            schema: path("schema.json"),
            target: GradeTarget::File(path("data.json")),
        };

        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }

    #[test]
    fn schema_validation_fails_when_the_target_does_not_conform() {
        let fixture = Fixture::new();
        fixture.write_to_skill(
            "schema.json",
            r#"{"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}}"#,
        );
        fixture.write_to_workspace("data.json", r#"{"name": 42}"#);
        let grader = Grader::SchemaValidation {
            schema: path("schema.json"),
            target: GradeTarget::File(path("data.json")),
        };

        match evaluate(&grader, &fixture.input()) {
            GraderOutcome::Failed { evidence } => assert!(evidence.contains("schema.json"), "{evidence}"),
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn schema_validation_is_an_authoring_error_when_the_schema_file_is_missing() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("data.json", r#"{"name": "trg"}"#);
        let grader = Grader::SchemaValidation {
            schema: path("missing-schema.json"),
            target: GradeTarget::File(path("data.json")),
        };

        match evaluate(&grader, &fixture.input()) {
            GraderOutcome::AuthoringError { reason } => assert!(reason.contains("missing-schema.json"), "{reason}"),
            other => panic!("expected an authoring error, got {other:?}"),
        }
    }

    #[test]
    fn schema_validation_is_an_authoring_error_when_the_schema_is_not_valid_json() {
        let fixture = Fixture::new();
        fixture.write_to_skill("schema.json", "{not json");
        fixture.write_to_workspace("data.json", r#"{"name": "trg"}"#);
        let grader = Grader::SchemaValidation {
            schema: path("schema.json"),
            target: GradeTarget::File(path("data.json")),
        };

        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::AuthoringError { .. }
        ));
    }

    #[test]
    fn schema_validation_is_an_authoring_error_when_the_schema_document_is_not_a_valid_schema() {
        let fixture = Fixture::new();
        fixture.write_to_skill("schema.json", r#"{"properties": "not an object"}"#);
        fixture.write_to_workspace("data.json", r#"{"name": "trg"}"#);
        let grader = Grader::SchemaValidation {
            schema: path("schema.json"),
            target: GradeTarget::File(path("data.json")),
        };

        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::AuthoringError { .. }
        ));
    }

    #[test]
    fn a_broken_schema_is_an_authoring_error_even_when_the_run_produced_no_target() {
        let fixture = Fixture::new();
        let grader = Grader::SchemaValidation {
            schema: path("missing-schema.json"),
            target: GradeTarget::File(path("never-written.json")),
        };

        match evaluate(&grader, &fixture.input()) {
            GraderOutcome::AuthoringError { reason } => assert!(reason.contains("missing-schema.json"), "{reason}"),
            other => {
                panic!("a suite naming a schema that does not exist is broken however the run went, got {other:?}")
            }
        }
    }

    #[test]
    fn a_broken_schema_is_an_authoring_error_even_when_the_target_is_not_json() {
        let fixture = Fixture::new();
        fixture.write_to_skill("schema.json", "{not json");
        fixture.write_to_workspace("data.json", "this is prose, not json");
        let grader = Grader::SchemaValidation {
            schema: path("schema.json"),
            target: GradeTarget::File(path("data.json")),
        };

        match evaluate(&grader, &fixture.input()) {
            GraderOutcome::AuthoringError { .. } => {}
            other => {
                panic!("the schema is unreadable, which is the author's problem and not the skill's, got {other:?}")
            }
        }
    }

    #[test]
    fn schema_validation_is_resolved_relative_to_the_skill_directory_not_the_workspace() {
        let fixture = Fixture::new();
        fixture.write_to_skill("schema.json", r#"{"type": "object"}"#);
        fixture.write_to_workspace("schema.json", "{not json");
        fixture.write_to_workspace("data.json", r#"{"name": "trg"}"#);
        let grader = Grader::SchemaValidation {
            schema: path("schema.json"),
            target: GradeTarget::File(path("data.json")),
        };

        assert!(matches!(
            evaluate(&grader, &fixture.input()),
            GraderOutcome::Passed { .. }
        ));
    }
}
