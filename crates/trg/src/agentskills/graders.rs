use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;
use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::call_bounds::CallBounds;
use super::evals::{EvalError, GlobPattern, NonEmptyString, RelativeSkillPath, Result};
use super::mocks::{RenderedMockCalls, MOCK_CALLS_LOG_NAME};
use super::transcript::{NormalizedTranscript, StagedSkill, ToolName};
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

/// How much one grader's result counts toward its case's score, relative to
/// every other grader and free-text assertion in the same case.
///
/// Always positive. A grader worth nothing to the score belongs out of the
/// case entirely, scored with `arm: with_only` rather than weighted to zero,
/// and a negative weight has no share of a score to subtract from, so
/// neither reading is one this type can hold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, JsonSchema)]
#[schemars(schema_with = "grader_weight_schema")]
pub struct GraderWeight(f64);

fn grader_weight_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "number",
        "exclusiveMinimum": 0.0
    })
}

impl GraderWeight {
    pub const fn unweighted() -> Self {
        Self(1.0)
    }

    pub fn parse(weight: f64) -> std::result::Result<Self, String> {
        if !weight.is_finite() {
            return Err("grader weight must be a finite number".to_string());
        }
        if weight <= 0.0 {
            return Err(
                "grader weight must be greater than zero: a grader worth nothing to the score belongs out of the case (declare 'arm': 'with_only' instead of weighting it to zero), and a negative weight has no share of a score to subtract from"
                    .to_string(),
            );
        }
        Ok(Self(weight))
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

impl Default for GraderWeight {
    fn default() -> Self {
        Self::unweighted()
    }
}

impl<'de> Deserialize<'de> for GraderWeight {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
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
    /// The contents of every file under `outputs/` whose path matches a glob, in
    /// path order and each labelled with the path it came from.
    ///
    /// This narrows `any_output`, it does not widen it: the same tree, restricted
    /// to the files the pattern names. A pattern that matches nothing is a target
    /// that is missing rather than one that is empty.
    Files(GlobPattern),
    /// Every call the run made against its MCP mocks, rendered one per line as
    /// `<server>.<tool> <input>`, the input as compact JSON with object keys in
    /// sorted order. The `expect` violations the mock server also logs are left
    /// out, since a run already reports each one as its own failing assertion.
    ///
    /// This is the only target that grades the request rather than the answer: the
    /// other targets all describe what the agent produced, and this one describes
    /// what it asked for. A run that hosted mocks and called none of them is an
    /// empty target, so `negate` still means what it says; a run that hosted no
    /// mock server at all cannot answer the question either way.
    MockCalls,
}

impl fmt::Display for GradeTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FinalText => f.write_str("final text"),
            Self::Transcript => f.write_str("transcript"),
            Self::AnyOutput => f.write_str("any output file"),
            Self::File(path) => write!(f, "file '{path}'"),
            Self::CreatedFiles => f.write_str("created files"),
            Self::Files(pattern) => write!(f, "output files matching '{pattern}'"),
            Self::MockCalls => f.write_str("mock calls"),
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

/// A tool a `tool_order` pair names: either just its name, or its name
/// together with a pattern the call's arguments must match.
///
/// Folding the qualified shape into an object rather than adding a second
/// `input_match` field to `ToolOrderCheck` is what lets `before` and `after`
/// each carry their own pattern independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ToolReference {
    Named(ToolName),
    WithInputMatch { tool: ToolName, input_match: RegexPattern },
}

impl ToolReference {
    fn tool(&self) -> &ToolName {
        match self {
            Self::Named(tool) => tool,
            Self::WithInputMatch { tool, .. } => tool,
        }
    }

    fn input_match(&self) -> Option<&RegexPattern> {
        match self {
            Self::Named(_) => None,
            Self::WithInputMatch { input_match, .. } => Some(input_match),
        }
    }
}

impl fmt::Display for ToolReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.input_match() {
            Some(pattern) => write!(f, "'{}'{}", self.tool(), naming(pattern)),
            None => write!(f, "'{}'", self.tool()),
        }
    }
}

/// What a `tool_order` grader checks: either a whole ordered subsequence, or
/// that one tool ran before another.
///
/// The two shapes are pulled apart rather than folded into optional fields on
/// one struct, because "tools" and "before"/"after" answer different
/// questions, and a case that set both, or only one of the pair, would be
/// ambiguous about which question it meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOrderCheck {
    Subsequence(Vec<ToolName>),
    Pair {
        before: ToolReference,
        after: ToolReference,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DeclaredToolOrderCheck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 2))]
    tools: Option<Vec<ToolName>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before: Option<ToolReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after: Option<ToolReference>,
}

impl ToolOrderCheck {
    fn parse(declared: DeclaredToolOrderCheck) -> std::result::Result<Self, String> {
        match (declared.tools, declared.before, declared.after) {
            (Some(tools), None, None) => Ok(Self::Subsequence(tools)),
            (None, Some(before), Some(after)) => Ok(Self::Pair { before, after }),
            (None, None, None) => Err("tool_order needs either 'tools' or a 'before'/'after' pair".to_string()),
            (None, Some(_), None) | (None, None, Some(_)) => {
                Err("tool_order's 'before' and 'after' must both be given, or neither".to_string())
            }
            (Some(_), _, _) => Err("tool_order accepts 'tools' or 'before'/'after', not both".to_string()),
        }
    }

    fn declare(&self) -> DeclaredToolOrderCheck {
        match self {
            Self::Subsequence(tools) => DeclaredToolOrderCheck {
                tools: Some(tools.clone()),
                before: None,
                after: None,
            },
            Self::Pair { before, after } => DeclaredToolOrderCheck {
                tools: None,
                before: Some(before.clone()),
                after: Some(after.clone()),
            },
        }
    }
}

impl Serialize for ToolOrderCheck {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.declare().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ToolOrderCheck {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let declared = DeclaredToolOrderCheck::deserialize(deserializer)?;
        Self::parse(declared).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ToolOrderCheck {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        DeclaredToolOrderCheck::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        DeclaredToolOrderCheck::json_schema(generator)
    }
}

/// The reference output a `baseline` grader holds a run against, read off disk
/// while the grader is evaluated rather than when the judge is called.
///
/// Reading it here buys what `schema_validation` buys by opening its schema first:
/// a case naming a reference that is missing or blank is an authoring error the
/// first time it is graded, instead of a run that reads as a regression for as long
/// as the judge happens to agree with it. It also means no judge is ever billed for
/// a comparison that had nothing on the other side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineReference {
    declared: String,
    text: String,
}

impl BaselineReference {
    pub fn read(declared: &RelativeSkillPath, resolved: &Path) -> std::result::Result<Self, String> {
        let text = std::fs::read_to_string(resolved)
            .map_err(|e| format!("baseline reference '{}' cannot be read: {e}", resolved.display()))?;
        if text.trim().is_empty() {
            return Err(format!(
                "baseline reference '{declared}' is blank, and every output is at least as good as nothing"
            ));
        }
        Ok(Self {
            declared: declared.to_string(),
            text,
        })
    }

    pub fn declared(&self) -> &str {
        &self.declared
    }

    pub fn text(&self) -> &str {
        &self.text
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
        #[serde(flatten)]
        check: ToolOrderCheck,
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
    // `target` is a plain `GradeTarget` rather than the `TargetDeclaration` the `llm`
    // grader carries, because the distinction that type exists to preserve, whether
    // the mechanical shortcut may still fire ahead of the judge, cannot arise here:
    // no mechanical check answers a comparison.
    /// Is this run at least as good as a reference output the suite already accepts?
    Baseline {
        reference: RelativeSkillPath,
        criterion: NonEmptyString,
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
            Self::Baseline { .. } => "baseline",
        }
    }

    /// What this grader reads, for the graders that read something. `file_exists`
    /// and the transcript graders answer from the run itself rather than from a
    /// target, so they name none.
    pub fn target(&self) -> Option<GradeTarget> {
        match self {
            Self::Regex { target, .. }
            | Self::Contains { target, .. }
            | Self::ValidJson { target }
            | Self::SchemaValidation { target, .. }
            | Self::Baseline { target, .. } => Some(target.clone()),
            Self::Llm { target, .. } => Some(target.resolve()),
            Self::FileExists { .. } | Self::ToolUsed { .. } | Self::ToolOrder { .. } | Self::SkillUsed { .. } => None,
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
            Self::ToolOrder { check } => match check {
                ToolOrderCheck::Subsequence(tools) => {
                    let names = tools.iter().map(ToolName::as_str).collect::<Vec<_>>().join(" then ");
                    format!("tools were used in order: {names}")
                }
                ToolOrderCheck::Pair { before, after } => {
                    format!("tool {before} was used before tool {after}")
                }
            },
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
            Self::Baseline {
                reference,
                criterion,
                target,
            } => format!("{target} is at least as good as baseline '{reference}' on: {criterion}"),
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CaseGrader {
    #[serde(flatten)]
    pub grader: Grader,
    #[serde(default, skip_serializing_if = "is_default_arm")]
    pub arm: GraderArm,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<GraderName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<GraderWeight>,
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
            weight: None,
        }
    }

    pub fn counts_toward_score(&self) -> bool {
        self.exclusion_reason().is_none()
    }

    /// The weight to score this grader's result at, `unweighted` when the
    /// case did not declare one, so undeclared weight always resolves to the
    /// value that reproduces today's plain average.
    pub fn effective_weight(&self) -> GraderWeight {
        self.weight.unwrap_or_default()
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
    /// The grader is a judgement call about two outputs rather than one, so the
    /// judge is asked a different question, from a different prompt, about material
    /// this evaluation already read off disk. Kept apart from `Deferred` so that no
    /// caller can hand the judge a comparison with nothing to compare against.
    Comparison {
        criterion: String,
        target: GradeTarget,
        reference: BaselineReference,
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

/// Why a run cannot answer a question about a target at all, as opposed to
/// answering it in the negative.
///
/// A missing target is still a verdict about the agent: it was asked for something
/// and did not produce it. This is the other case, where nothing about the run was
/// ever going to fill the target in, and neither a pass nor a fail would be about
/// the skill under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnobservableTarget(String);

/// Said of a run that left no call log at all, which is a run no mock server ever came
/// up for.
const NO_MOCK_SERVER_REASON: &str =
    "this run hosted no mcp mock server, so there is no record of what the agent asked one for";

impl UnobservableTarget {
    fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }

    pub fn reason(&self) -> &str {
        &self.0
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

    /// What this run staged, as the run itself recorded it. A run whose transcript
    /// could not be read is treated as one that staged somewhere unrecorded, since
    /// that is the reading that cannot credit the agent with the harness's work.
    fn staged_skill(&self) -> StagedSkill {
        self.transcript
            .map(|transcript| transcript.staged_skill.clone())
            .unwrap_or_default()
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
            GradeTarget::Files(pattern) => self.matching_outputs(pattern),
            GradeTarget::MockCalls => match self.mock_calls() {
                Ok(Some(calls)) => TargetContent::Text(calls.into_text()),
                Ok(None) => TargetContent::Missing(NO_MOCK_SERVER_REASON.to_string()),
                Err(e) => TargetContent::Missing(format!("cannot read '{}': {e}", self.mock_calls_log().display())),
            },
        }
    }

    /// Why this run cannot be asked about `target`, if it cannot.
    ///
    /// Asked ahead of reading the target rather than folded into `TargetContent`,
    /// because every reader of that type has to turn what it finds into a pass or a
    /// fail, and this is the case where neither is honest.
    pub(crate) fn unobservable(&self, target: &GradeTarget) -> Option<UnobservableTarget> {
        match target {
            // Only an absent log is unobservable. A log that is there but unreadable is a
            // broken artifact of a run that did host a mock server, and withdrawing the
            // check from the score would report that as a control the harness never
            // offered, so it is left to the grader to fail on like any other.
            GradeTarget::MockCalls => match self.mock_calls() {
                Ok(None) => Some(UnobservableTarget::new(NO_MOCK_SERVER_REASON)),
                Ok(Some(_)) | Err(_) => None,
            },
            _ => None,
        }
    }

    fn mock_calls_log(&self) -> PathBuf {
        self.run_dir.join(MOCK_CALLS_LOG_NAME)
    }

    /// The mock call log this run left behind.
    ///
    /// A mock server writes the log once it has loaded its mocks, so no log at all means
    /// none ever came up: the case declares no mocks, or the harness cannot host one and
    /// trg declined to start the run, or the server died before it could answer anything.
    fn mock_calls(&self) -> io::Result<Option<RenderedMockCalls>> {
        RenderedMockCalls::read(&self.mock_calls_log())
    }

    /// Reads the output files a glob names, in path order so the same run grades
    /// the same way twice, each labelled so a verdict about one file is
    /// attributable to it.
    ///
    /// Only `outputs/` is walked, which is the tree `any_output` covers. Searching
    /// the workspace too would make a narrowed target read more than the wide one,
    /// and would hand the judge the agent's scratch files and the skill the harness
    /// staged alongside the work being graded.
    fn matching_outputs(&self, pattern: &GlobPattern) -> TargetContent {
        let regex = pattern.within_outputs().compile();
        let mut matched = Vec::new();
        collect_matching(self.outputs_dir, self.outputs_dir, &regex, &mut matched);
        if matched.is_empty() {
            return TargetContent::Missing(format!("no file under outputs/ matches '{pattern}'"));
        }
        matched.sort_by(|(left, _), (right, _)| left.cmp(right));
        let parts: Vec<String> = matched
            .into_iter()
            .map(|(relative, body)| format!("=== {relative} ===\n{body}"))
            .collect();
        TargetContent::Text(parts.join("\n\n"))
    }
}

/// Walks `dir` for files whose path relative to `base` matches `regex`.
///
/// A file that cannot be read as UTF-8 text is named with its size rather than
/// skipped, because a label with nothing under it reads to a judge as a file the
/// agent left empty, which is a different fact about the run.
fn collect_matching(base: &Path, dir: &Path, regex: &Regex, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_matching(base, &path, regex, out);
            continue;
        }
        let Ok(relative) = path.strip_prefix(base) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !regex.is_match(&relative) {
            continue;
        }
        let body = match std::fs::read(&path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(e) => format!("<{} bytes that are not UTF-8 text>", e.as_bytes().len()),
            },
            Err(e) => format!("<unreadable: {e}>"),
        };
        out.push((relative, body));
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
fn find_glob_match(searches: &[(&Path, &Regex)], staged: &StagedSkill) -> Option<PathBuf> {
    searches
        .iter()
        .find_map(|(dir, regex)| walk_for_glob_match(dir, dir, regex, staged))
}

fn walk_for_glob_match(base: &Path, dir: &Path, regex: &Regex, staged: &StagedSkill) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk_for_glob_match(base, &path, regex, staged) {
                return Some(found);
            }
        } else if let Ok(relative) = path.strip_prefix(base) {
            let relative = relative.to_string_lossy().replace('\\', "/");
            if staged.planted(&relative) {
                continue;
            }
            if regex.is_match(&relative) {
                return Some(path);
            }
        }
    }
    None
}

pub fn evaluate(grader: &Grader, input: &GradeInput) -> GraderOutcome {
    // Settled before the grader runs, and for every grader rather than inside the
    // arms that happen to read text, because a target this run could never have
    // filled in is not evidence about the agent in either direction: a plain check
    // would fail on it and a negated one would pass, and both would be reporting the
    // harness instead of the skill.
    if let Some(unobservable) = grader.target().and_then(|target| input.unobservable(&target)) {
        return GraderOutcome::Unsupported {
            reason: unobservable.reason().to_string(),
        };
    }

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
                    // The run directory is deliberately not searched, and neither is the
                    // skill this run staged into its workspace. A literal path names one
                    // file and an author who names `transcript.jsonl` meant it, but a glob
                    // is a description, and `timing.json` at the run root or the staged
                    // `SKILL.md` would answer it with a file the agent never wrote.
                    //
                    // The output tree is searched under a pattern that has had any
                    // `outputs/` prefix dropped, since paths taken relative to that tree
                    // do not carry one; the workspace keeps the pattern as written,
                    // because in the layout where the tree sits inside the workspace that
                    // is exactly how the same files read from there.
                    let staged = input.staged_skill();
                    let in_outputs = path.within_outputs().compile();
                    let as_written = path.compile();
                    let searches = [(input.outputs_dir, &in_outputs), (input.workspace_dir, &as_written)];
                    match find_glob_match(&searches, &staged) {
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
        Grader::ToolOrder { check } => with_transcript(input, |transcript| match check {
            ToolOrderCheck::Subsequence(tools) => {
                let observed = transcript.tool_sequence();
                let matched = is_subsequence(tools, &observed);
                let rendered = observed.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(", ");
                let verb = if matched { "contains" } else { "does not contain" };
                GraderOutcome::from_bool(
                    matched,
                    format!("observed tool order [{rendered}] {verb} the expected order"),
                )
            }
            ToolOrderCheck::Pair { before, after } => {
                let matched = before_after_holds(transcript, before, after);
                let verb = if matched { "was" } else { "was not" };
                GraderOutcome::from_bool(matched, format!("tool {before} {verb} used before tool {after}"))
            }
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
        Grader::Baseline {
            reference,
            criterion,
            target,
        } => {
            let resolved = input.skill_dir.join(reference.as_path());
            match BaselineReference::read(reference, &resolved) {
                Ok(reference) => GraderOutcome::Comparison {
                    criterion: criterion.to_string(),
                    target: target.clone(),
                    reference,
                },
                Err(reason) => GraderOutcome::AuthoringError { reason },
            }
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

fn matches_tool_reference(tool: &ToolName, paths: &[String], reference: &ToolReference) -> bool {
    if !tool.eq_ignore_case(reference.tool().as_str()) {
        return false;
    }
    match reference.input_match() {
        None => true,
        Some(pattern) => {
            let matcher = pattern.compile();
            paths.iter().any(|value| matcher.is_match(value))
        }
    }
}

/// Whether some call matching `before` happened, and some call matching
/// `after` happened later in the run's actual chronological order.
///
/// Only the first `before` match anchors the search: a call cannot count as
/// both halves of the pair at once, but a run that repeats `before` after
/// already satisfying `after` still passes, since the pair asks "did before
/// happen and then after", not "did every before precede every after".
fn before_after_holds(transcript: &NormalizedTranscript, before: &ToolReference, after: &ToolReference) -> bool {
    let mut seen_before = false;
    for (tool, paths) in transcript.tool_call_inputs() {
        if !seen_before && matches_tool_reference(tool, paths, before) {
            seen_before = true;
            continue;
        }
        if seen_before && matches_tool_reference(tool, paths, after) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::evals::SkillDisclosure;
    use crate::agentskills::prompt::{SkillName, StagedSkillDir};
    use crate::agentskills::redact::redact_transcript_bytes;
    use crate::agentskills::report::ScenarioKind;
    use crate::agentskills::transcript::StagedSkillName;
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

        fn write_to_run_dir(&self, name: &str, contents: impl AsRef<[u8]>) {
            std::fs::write(self.run_dir.join(name), contents).unwrap();
        }

        /// Moves the output tree beside the workspace instead of inside it, which is
        /// the layout `run_context` picks whenever `<run>/outputs` is a directory.
        /// No workspace-relative path carries an `outputs/` prefix in that layout.
        fn outputs_beside_the_workspace(&mut self) {
            let relocated = self.run_dir.join("outputs");
            std::fs::rename(&self.outputs_dir, &relocated).unwrap();
            self.outputs_dir = relocated;
        }

        fn write_to_outputs(&self, relative: &str, contents: impl AsRef<[u8]>) {
            let path = self.outputs_dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }

        /// Plants a file where the harness stages a skill, and records the staging the
        /// way a real run would, so a grader reads the same evidence it would in one.
        fn stage_skill(&mut self, staged: StagedSkill, relative: &str, contents: &str) {
            let path = self.workspace_dir.join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
            self.transcript.staged_skill = staged;
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

    fn staged_at(disclosure: SkillDisclosure) -> StagedSkill {
        StagedSkill::At {
            directory: StagedSkillDir::for_run(ScenarioKind::WithSkill, disclosure, "demo-skill").unwrap(),
            name: StagedSkillName::known(SkillName::parse("demo-skill").unwrap()),
        }
    }

    #[test]
    fn a_glob_does_not_answer_with_the_skill_the_harness_staged() {
        let mut fixture = Fixture::new();
        fixture.stage_skill(
            staged_at(SkillDisclosure::Announced),
            ".skill/SKILL.md",
            "---\nname: demo-skill\n---\n",
        );

        match evaluate(
            &Grader::FileExists {
                path: glob("**/SKILL.md"),
                exists: true,
            },
            &fixture.input(),
        ) {
            GraderOutcome::Failed { .. } => {}
            other => panic!("the agent wrote no SKILL.md; the harness staged one: {other:?}"),
        }
    }

    #[test]
    fn exists_false_over_a_glob_is_not_failed_by_the_skill_staged_under_a_plain_name() {
        let mut fixture = Fixture::new();
        fixture.stage_skill(
            staged_at(SkillDisclosure::Unannounced),
            "skills/demo-skill/SKILL.md",
            "---\nname: demo-skill\n---\n",
        );

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("skills/**/*.md"),
                exists: false,
            },
            &fixture.input(),
        );
        assert!(
            matches!(outcome, GraderOutcome::Passed { .. }),
            "an unannounced skill is staged under a plain directory, and it is still not the agent's: {outcome:?}"
        );
    }

    #[test]
    fn a_transcript_that_never_recorded_its_staging_is_read_against_every_place_one_is_staged() {
        let mut fixture = Fixture::new();
        fixture.stage_skill(
            StagedSkill::Unrecorded,
            ".skill/SKILL.md",
            "---\nname: demo-skill\n---\n",
        );

        match evaluate(
            &Grader::FileExists {
                path: glob("**/SKILL.md"),
                exists: true,
            },
            &fixture.input(),
        ) {
            GraderOutcome::Failed { .. } => {}
            other => panic!("an older transcript cannot say the agent wrote this: {other:?}"),
        }
    }

    #[test]
    fn a_glob_still_answers_with_a_file_the_agent_wrote_where_a_skill_could_have_been_staged() {
        let mut fixture = Fixture::new();
        fixture.stage_skill(StagedSkill::Nothing, ".skill/SKILL.md", "written by the agent\n");

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("**/SKILL.md"),
                exists: true,
            },
            &fixture.input(),
        );
        assert!(
            matches!(outcome, GraderOutcome::Passed { .. }),
            "this arm staged nothing, so every file under the workspace is the agent's: {outcome:?}"
        );
    }

    #[test]
    fn a_glob_does_not_answer_with_the_harnesss_own_run_artifacts() {
        let fixture = Fixture::new();
        fixture.write_to_run_dir("timing.json", r#"{"duration_ms":42}"#);

        match evaluate(
            &Grader::FileExists {
                path: glob("**/*.json"),
                exists: true,
            },
            &fixture.input(),
        ) {
            GraderOutcome::Failed { .. } => {}
            other => panic!("the agent wrote no json; only trg did: {other:?}"),
        }
    }

    #[test]
    fn exists_false_over_a_glob_is_not_failed_by_a_file_trg_wrote_itself() {
        let fixture = Fixture::new();
        fixture.write_to_run_dir("grading.json", "{}");

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("*.json"),
                exists: false,
            },
            &fixture.input(),
        );
        assert!(
            matches!(outcome, GraderOutcome::Passed { .. }),
            "a case cannot be failed by an artifact it has no way to avoid: {outcome:?}"
        );
    }

    #[test]
    fn a_glob_written_from_the_current_directory_matches_the_same_files() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("summary.md", "May revenue was up.\n");

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("./*.md"),
                exists: true,
            },
            &fixture.input(),
        );
        assert!(
            matches!(outcome, GraderOutcome::Passed { .. }),
            "a leading ./ is how half the world writes a relative path: {outcome:?}"
        );
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
                check: ToolOrderCheck::Subsequence(vec![tool("Read"), tool("Write")]),
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(
            &Grader::ToolOrder {
                check: ToolOrderCheck::Subsequence(vec![tool("Write"), tool("Read")]),
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn tool_order_pair_holds_when_before_precedes_after() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::ToolOrder {
                check: ToolOrderCheck::Pair {
                    before: ToolReference::Named(tool("Read")),
                    after: ToolReference::Named(tool("Write")),
                },
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(
            &Grader::ToolOrder {
                check: ToolOrderCheck::Pair {
                    before: ToolReference::Named(tool("Write")),
                    after: ToolReference::Named(tool("Read")),
                },
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn tool_order_pair_can_be_qualified_by_input_match() {
        let fixture = Fixture::new();
        let outcome = evaluate(
            &Grader::ToolOrder {
                check: ToolOrderCheck::Pair {
                    before: ToolReference::WithInputMatch {
                        tool: tool("Bash"),
                        input_match: RegexPattern::parse("^ls$").unwrap(),
                    },
                    after: ToolReference::Named(tool("Write")),
                },
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(
            &Grader::ToolOrder {
                check: ToolOrderCheck::Pair {
                    before: ToolReference::WithInputMatch {
                        tool: tool("Bash"),
                        input_match: RegexPattern::parse("npm test").unwrap(),
                    },
                    after: ToolReference::Named(tool("Write")),
                },
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn tool_order_check_requires_tools_or_a_complete_before_after_pair() {
        let neither = serde_json::from_value::<Grader>(serde_json::json!({ "type": "tool_order" })).unwrap_err();
        assert!(neither.to_string().contains("before"), "{neither}");

        let only_before = serde_json::from_value::<Grader>(serde_json::json!({
            "type": "tool_order",
            "before": "Read"
        }))
        .unwrap_err();
        assert!(only_before.to_string().contains("both"), "{only_before}");

        let both = serde_json::from_value::<Grader>(serde_json::json!({
            "type": "tool_order",
            "tools": ["Read", "Write"],
            "before": "Read",
            "after": "Write"
        }))
        .unwrap_err();
        assert!(both.to_string().contains("not both"), "{both}");
    }

    #[test]
    fn tool_order_pair_round_trips_through_the_fields_a_case_declares() {
        let declared = serde_json::json!({
            "type": "tool_order",
            "before": "Read",
            "after": { "tool": "Bash", "input_match": "npm test" }
        });
        let grader: Grader = serde_json::from_value(declared.clone()).unwrap();
        assert_eq!(
            grader,
            Grader::ToolOrder {
                check: ToolOrderCheck::Pair {
                    before: ToolReference::Named(tool("Read")),
                    after: ToolReference::WithInputMatch {
                        tool: tool("Bash"),
                        input_match: RegexPattern::parse("npm test").unwrap(),
                    },
                },
            }
        );
        assert_eq!(serde_json::to_value(grader).unwrap(), declared);
    }

    #[test]
    fn tool_order_subsequence_shape_is_unchanged_by_the_redesign() {
        let declared = serde_json::json!({
            "type": "tool_order",
            "tools": ["Read", "Write"]
        });
        let grader: Grader = serde_json::from_value(declared.clone()).unwrap();
        assert_eq!(
            grader,
            Grader::ToolOrder {
                check: ToolOrderCheck::Subsequence(vec![tool("Read"), tool("Write")]),
            }
        );
        assert_eq!(serde_json::to_value(grader).unwrap(), declared);
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
                check: ToolOrderCheck::Subsequence(vec![tool("Read"), tool("Write")]),
            },
            Grader::ToolOrder {
                check: ToolOrderCheck::Pair {
                    before: ToolReference::Named(tool("Read")),
                    after: ToolReference::Named(tool("Write")),
                },
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
                    check: ToolOrderCheck::Subsequence(vec![tool("Read"), tool("Write")]),
                },
                "tools were used in order: Read then Write",
            ),
            (
                Grader::ToolOrder {
                    check: ToolOrderCheck::Pair {
                        before: ToolReference::Named(tool("Read")),
                        after: ToolReference::WithInputMatch {
                            tool: tool("Bash"),
                            input_match: RegexPattern::parse("npm test").unwrap(),
                        },
                    },
                },
                "tool 'Read' was used before tool 'Bash' with an argument matching /npm test/",
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

    #[test]
    fn an_outputs_prefixed_glob_answers_the_same_wherever_the_run_put_the_output_tree() {
        for beside in [false, true] {
            let mut fixture = Fixture::new();
            if beside {
                fixture.outputs_beside_the_workspace();
            }
            let outcome = evaluate(
                &Grader::FileExists {
                    path: glob("outputs/**/*.md"),
                    exists: true,
                },
                &fixture.input(),
            );
            assert!(
                matches!(outcome, GraderOutcome::Passed { .. }),
                "outputs beside the workspace: {beside}, got {outcome:?}"
            );
        }
    }

    fn baseline(reference: &str, criterion: &str) -> Grader {
        Grader::Baseline {
            reference: path(reference),
            criterion: text(criterion),
            target: GradeTarget::FinalText,
        }
    }

    #[test]
    fn an_outputs_prefixed_glob_is_not_answered_by_a_file_outside_the_output_tree() {
        let mut fixture = Fixture::new();
        fixture.outputs_beside_the_workspace();
        fixture.write_to_workspace("notes.md", "scratch the agent left in its working directory");

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("outputs/note?.md"),
                exists: true,
            },
            &fixture.input(),
        );

        assert!(
            matches!(outcome, GraderOutcome::Failed { .. }),
            "dropping the prefix must not let the pattern reach the workspace: {outcome:?}"
        );
    }

    #[test]
    fn a_glob_that_names_no_output_directory_is_unchanged_by_the_prefix_rule() {
        let mut fixture = Fixture::new();
        fixture.outputs_beside_the_workspace();
        fixture.write_to_workspace("notes.md", "scratch the agent left in its working directory");

        let outcome = evaluate(
            &Grader::FileExists {
                path: glob("note?.md"),
                exists: true,
            },
            &fixture.input(),
        );

        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");
    }

    #[test]
    fn a_baseline_carries_its_reference_to_the_judge_rather_than_the_path_to_read_it_from() {
        let fixture = Fixture::new();
        fixture.write_to_skill("golden.md", "The reference answer.\n");

        match evaluate(&baseline("golden.md", "is as complete"), &fixture.input()) {
            GraderOutcome::Comparison {
                criterion,
                target,
                reference,
            } => {
                assert_eq!(criterion, "is as complete");
                assert_eq!(target, GradeTarget::FinalText);
                assert_eq!(reference.text(), "The reference answer.\n");
                assert_eq!(reference.declared(), "golden.md", "evidence names what the case wrote");
            }
            other => panic!("expected a comparison, got {other:?}"),
        }
    }

    #[test]
    fn a_baseline_naming_a_reference_that_is_not_there_is_an_authoring_error() {
        let fixture = Fixture::new();

        let outcome = evaluate(&baseline("golden.md", "is as complete"), &fixture.input());

        let GraderOutcome::AuthoringError { reason } = outcome else {
            panic!("a missing reference is a broken case, not a failed run: {outcome:?}");
        };
        assert!(reason.contains("cannot be read"), "{reason}");
    }

    #[test]
    fn a_blank_baseline_reference_is_refused_instead_of_passing_everything() {
        let fixture = Fixture::new();
        fixture.write_to_skill("golden.md", "   \n\t\n");

        let outcome = evaluate(&baseline("golden.md", "is as complete"), &fixture.input());

        let GraderOutcome::AuthoringError { reason } = outcome else {
            panic!("a blank reference cannot be cleared, so it must not be graded: {outcome:?}");
        };
        assert!(reason.contains("is blank"), "{reason}");
    }

    #[test]
    fn a_baseline_is_resolved_relative_to_the_skill_directory_not_the_workspace() {
        let fixture = Fixture::new();
        fixture.write_to_skill("golden.md", "The reference answer.\n");
        fixture.write_to_workspace("golden.md", "Something the run wrote itself.\n");

        match evaluate(&baseline("golden.md", "is as complete"), &fixture.input()) {
            GraderOutcome::Comparison { reference, .. } => {
                assert_eq!(
                    reference.text(),
                    "The reference answer.\n",
                    "a run that writes over the reference must not get to choose what it is measured against"
                );
            }
            other => panic!("expected a comparison, got {other:?}"),
        }
    }

    #[test]
    fn a_baseline_grader_names_both_sides_of_the_comparison_when_it_describes_itself() {
        let described = baseline("golden.md", "covers every column").describe();

        assert_eq!(
            described,
            "final text is at least as good as baseline 'golden.md' on: covers every column"
        );
    }

    fn glob_target_text(fixture: &Fixture, pattern: &str) -> TargetContent {
        fixture.input().target_content(&GradeTarget::Files(glob(pattern)))
    }

    #[test]
    fn a_glob_target_reads_only_the_output_files_it_names() {
        let fixture = Fixture::new();
        fixture.write_to_outputs("notes.txt", "not markdown");
        fixture.write_to_outputs("sub/deep.md", "buried markdown");

        let TargetContent::Text(text) = glob_target_text(&fixture, "**/*.md") else {
            panic!("a glob that matches files reads as text");
        };

        assert!(text.contains("=== report.md ==="), "{text}");
        assert!(text.contains("=== sub/deep.md ==="), "{text}");
        assert!(text.contains("buried markdown"), "{text}");
        assert!(
            !text.contains("not markdown"),
            "a .txt file is outside '**/*.md': {text}"
        );
    }

    #[test]
    fn a_glob_target_that_matches_nothing_is_missing_rather_than_empty() {
        let fixture = Fixture::new();

        let content = glob_target_text(&fixture, "**/*.csv");

        let TargetContent::Missing(reason) = content else {
            panic!("a glob that matches nothing must not read as an empty target: {content:?}");
        };
        assert!(reason.contains("**/*.csv"), "{reason}");
    }

    #[test]
    fn a_glob_target_reads_its_files_in_path_order() {
        let fixture = Fixture::new();
        fixture.write_to_outputs("zebra.md", "last");
        fixture.write_to_outputs("alpha.md", "first");
        fixture.write_to_outputs("sub/middle.md", "nested");

        let TargetContent::Text(text) = glob_target_text(&fixture, "**/*.md") else {
            panic!("a glob that matches files reads as text");
        };

        let labels: Vec<&str> = text.lines().filter(|line| line.starts_with("=== ")).collect();
        assert_eq!(
            labels,
            vec![
                "=== alpha.md ===",
                "=== report.md ===",
                "=== sub/middle.md ===",
                "=== zebra.md ==="
            ],
            "the same run has to grade the same way twice"
        );
    }

    #[test]
    fn a_single_star_in_a_glob_target_does_not_reach_into_a_subdirectory() {
        let fixture = Fixture::new();
        fixture.write_to_outputs("sub/deep.md", "buried markdown");

        let TargetContent::Text(text) = glob_target_text(&fixture, "*.md") else {
            panic!("a glob that matches files reads as text");
        };

        assert!(text.contains("=== report.md ==="), "{text}");
        assert!(!text.contains("deep.md"), "'*' stays within one segment: {text}");
    }

    #[test]
    fn a_glob_target_names_a_file_it_cannot_read_as_text_rather_than_showing_it_as_empty() {
        let fixture = Fixture::new();
        fixture.write_to_outputs("blob.md", [0xff, 0xfe, 0x00, 0x01]);

        let TargetContent::Text(text) = glob_target_text(&fixture, "blob.md") else {
            panic!("a glob that matches a file reads as text");
        };

        assert!(text.contains("=== blob.md ==="), "{text}");
        assert!(
            text.contains("not UTF-8 text"),
            "a label with nothing under it would read as a file the agent left empty: {text}"
        );
    }

    #[test]
    fn a_glob_target_naming_the_output_directory_reads_the_same_files_as_one_that_does_not() {
        for pattern in ["**/*.md", "outputs/**/*.md"] {
            let fixture = Fixture::new();
            fixture.write_to_outputs("sub/deep.md", "buried markdown");

            let TargetContent::Text(text) = glob_target_text(&fixture, pattern) else {
                panic!("'{pattern}' matched nothing");
            };

            assert!(text.contains("=== report.md ==="), "{pattern}: {text}");
            assert!(text.contains("=== sub/deep.md ==="), "{pattern}: {text}");
        }
    }

    #[test]
    fn a_glob_target_does_not_read_the_workspace_outside_outputs() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("scratch.md", "working notes the agent did not publish");

        let TargetContent::Text(text) = glob_target_text(&fixture, "**/*.md") else {
            panic!("a glob that matches files reads as text");
        };

        assert!(text.contains("=== report.md ==="), "{text}");
        assert!(
            !text.contains("working notes"),
            "narrowing any_output must not read more than any_output does: {text}"
        );
    }

    fn mock_calls_target_text(fixture: &Fixture) -> TargetContent {
        fixture.input().target_content(&GradeTarget::MockCalls)
    }

    fn mock_calls_grader(text_value: &str, negate: bool) -> Grader {
        Grader::Contains {
            text: text(text_value),
            target: GradeTarget::MockCalls,
            case: MatchCase::Sensitive,
            negate,
        }
    }

    #[test]
    fn a_mock_calls_target_renders_one_call_per_line() {
        let fixture = Fixture::new();
        fixture.write_to_run_dir(
            MOCK_CALLS_LOG_NAME,
            concat!(
                r#"{"server":"github","tool":"create_issue","input":{"title":"Flaky build","repo":"acme/widgets"}}"#,
                "\n",
                r#"{"server":"github","tool":"close_issue","input":{"number":41}}"#,
                "\n",
            ),
        );

        let TargetContent::Text(rendered) = mock_calls_target_text(&fixture) else {
            panic!("a run with a call log reads as text");
        };

        assert_eq!(
            rendered,
            concat!(
                r#"github.create_issue {"repo":"acme/widgets","title":"Flaky build"}"#,
                "\n",
                r#"github.close_issue {"number":41}"#
            ),
            "the documented rendering is what a case writes its patterns against"
        );
    }

    #[test]
    fn a_mock_call_rendering_leaves_out_the_violations_the_run_already_reports_on_its_own() {
        let fixture = Fixture::new();
        fixture.write_to_run_dir(
            MOCK_CALLS_LOG_NAME,
            concat!(
                r#"{"server":"github","tool":"create_issue","input":{"repo":"other/widgets"},"#,
                r#""violations":[{"server":"github","tool":"create_issue","path":"repo","#,
                r#""constraint":"/^acme\\//","received":"other/widgets"}]}"#,
                "\n",
            ),
        );

        let TargetContent::Text(rendered) = mock_calls_target_text(&fixture) else {
            panic!("a run with a call log reads as text");
        };

        assert_eq!(rendered, r#"github.create_issue {"repo":"other/widgets"}"#);
        assert!(
            !rendered.contains("constraint"),
            "a mismatch already fails a case once, and must not get a second way to: {rendered}"
        );
    }

    #[test]
    fn a_call_log_line_that_cannot_be_read_back_is_kept_rather_than_dropped() {
        let fixture = Fixture::new();
        fixture.write_to_run_dir(MOCK_CALLS_LOG_NAME, "{\"server\":\"github\",\"tool\":\n");

        let TargetContent::Text(rendered) = mock_calls_target_text(&fixture) else {
            panic!("a run with a call log reads as text");
        };

        assert!(
            rendered.starts_with("<unparsed> "),
            "a call that disappears reads as a call the agent never made: {rendered}"
        );
    }

    #[test]
    fn a_run_that_hosted_mocks_and_called_none_is_an_empty_target_not_a_missing_one() {
        let fixture = Fixture::new();
        fixture.write_to_run_dir(MOCK_CALLS_LOG_NAME, "");

        assert_eq!(mock_calls_target_text(&fixture), TargetContent::Text(String::new()));

        let asked = evaluate(&mock_calls_grader("github.create_issue", false), &fixture.input());
        assert!(
            matches!(asked, GraderOutcome::Failed { .. }),
            "a call the agent never made is a failure, not an unanswerable question: {asked:?}"
        );

        let never_asked = evaluate(&mock_calls_grader("github.delete_repo", true), &fixture.input());
        assert!(
            matches!(never_asked, GraderOutcome::Passed { .. }),
            "'the agent never asked for this' has to pass on a run that asked for nothing: {never_asked:?}"
        );
    }

    #[test]
    fn a_run_that_hosted_no_mock_server_cannot_answer_a_mock_calls_grader_either_way() {
        let fixture = Fixture::new();

        for negate in [false, true] {
            let outcome = evaluate(&mock_calls_grader("github.create_issue", negate), &fixture.input());
            let GraderOutcome::Unsupported { reason } = outcome else {
                panic!("a mock that was never there cannot credit or blame the skill: {outcome:?}");
            };
            assert!(reason.contains("no mcp mock server"), "{reason}");
        }
    }

    /// An artifact the reader broke on and an artifact that was never written are two
    /// different claims: the first says a mock server came up and left something unusable
    /// behind, the second says none ever came up. Only the second is a question the run
    /// cannot answer, so only the second may take the check out of the score.
    #[test]
    fn a_call_log_that_exists_but_cannot_be_read_fails_the_grader_rather_than_leaving_the_score() {
        let fixture = Fixture::new();
        fixture.write_to_run_dir(MOCK_CALLS_LOG_NAME, [0xff, 0xfe, 0x00]);

        let outcome = evaluate(&mock_calls_grader("github.create_issue", false), &fixture.input());

        let GraderOutcome::Failed { evidence } = outcome else {
            panic!("a broken artifact is a finding about the run, not a control it was never offered: {outcome:?}");
        };
        assert!(
            evidence.contains(MOCK_CALLS_LOG_NAME),
            "the evidence has to name the artifact that could not be read: {evidence}"
        );
    }

    #[test]
    fn a_mock_calls_grader_that_reaches_the_judge_is_stopped_before_it_costs_a_request() {
        let fixture = Fixture::new();

        let outcome = evaluate(
            &Grader::Llm {
                criterion: text("the issue was filed against the right repository"),
                target: TargetDeclaration::Named(GradeTarget::MockCalls),
            },
            &fixture.input(),
        );

        assert!(
            matches!(outcome, GraderOutcome::Unsupported { .. }),
            "a judge cannot be asked about a mock that was never hosted: {outcome:?}"
        );
    }

    #[test]
    fn mock_calls_round_trips_through_the_grade_target_deserializer() {
        let target: GradeTarget = serde_json::from_value(serde_json::json!("mock_calls")).unwrap();

        assert_eq!(target, GradeTarget::MockCalls);
        assert_eq!(serde_json::to_value(&target).unwrap(), serde_json::json!("mock_calls"));
        assert_eq!(target.to_string(), "mock calls");

        let grader: Grader = serde_json::from_value(serde_json::json!({
            "type": "regex",
            "pattern": "^github\\.create_issue ",
            "target": "mock_calls"
        }))
        .unwrap();

        assert_eq!(grader.target(), Some(GradeTarget::MockCalls));
    }

    #[test]
    fn every_grader_that_reads_a_target_hands_it_back() {
        let targeted = [
            Grader::Regex {
                pattern: RegexPattern::parse("x").unwrap(),
                target: GradeTarget::MockCalls,
                negate: false,
                flags: RegexFlags::default(),
                count: None,
            },
            mock_calls_grader("x", false),
            Grader::ValidJson {
                target: GradeTarget::MockCalls,
            },
            Grader::SchemaValidation {
                schema: path("schema.json"),
                target: GradeTarget::MockCalls,
            },
            Grader::Baseline {
                reference: path("golden.md"),
                criterion: text("is as complete"),
                target: GradeTarget::MockCalls,
            },
            Grader::Llm {
                criterion: text("is as complete"),
                target: TargetDeclaration::Named(GradeTarget::MockCalls),
            },
        ];

        for grader in targeted {
            assert_eq!(
                grader.target(),
                Some(GradeTarget::MockCalls),
                "{} reads a target and must say so, or an unobservable one reaches it unchecked",
                grader.kind()
            );
        }
    }
}
