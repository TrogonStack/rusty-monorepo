mod constants;
use constants::{
    DOUBLE_QUOTE_VARIANTS, HYPHEN_VARIANTS, NO_MOCK_SERVER_REASON, SINGLE_QUOTE_VARIANTS, TOOL_CALL_MARKUP,
};
use std::borrow::Cow;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;
use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

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

    /// Compiles the pattern folded the same way as the text it is matched
    /// against, so a pattern naming a dash or a curly quote still finds one.
    /// Folding can turn a full-width character into a regex metacharacter, so
    /// the folded pattern is compiled afresh rather than assumed valid.
    pub fn compile_normalized(
        &self,
        flags: &RegexFlags,
        normalize: TextNormalization,
    ) -> std::result::Result<Regex, regex::Error> {
        let folded = normalize.for_pattern(&self.0);
        let mut builder = regex::RegexBuilder::new(&folded);
        flags.apply(&mut builder);
        builder.build()
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
/// distinction decides whether re-serializing the grader writes `target` back
/// out or leaves it omitted the way the author wrote it, so it has to survive
/// deserialization as part of the value.
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

/// Whether a text grader compares text as the run wrote it, or folded first.
///
/// Folding is the default because a model that writes `2.1.0-M1` with a
/// non-breaking hyphen, or quotes a phrase in curly quotes, has still written
/// what the case asked for, and a check that fails on the typography reports
/// the font rather than the skill. Folding applies NFKC, then maps the hyphen
/// and dash variants to `-` and the curly quotes to their ASCII forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, JsonSchema)]
#[schemars(schema_with = "text_normalization_schema")]
pub enum TextNormalization {
    #[default]
    Folded,
    Exact,
}

fn text_normalization_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "boolean",
        "default": true
    })
}

impl TextNormalization {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// For substring search, where a line break the model wrapped a phrase at is
    /// no different from the space the case wrote, so whitespace runs collapse.
    pub fn for_search<'a>(self, text: &'a str) -> Cow<'a, str> {
        match self {
            Self::Exact => Cow::Borrowed(text),
            Self::Folded => {
                let mut folded = String::with_capacity(text.len());
                let mut in_whitespace = false;
                for c in fold_characters(text) {
                    if c.is_whitespace() {
                        if !in_whitespace {
                            folded.push(' ');
                        }
                        in_whitespace = true;
                    } else {
                        folded.push(c);
                        in_whitespace = false;
                    }
                }
                unchanged_or_owned(text, folded)
            }
        }
    }

    /// What a `contains` or `identifier` grader should compare with, given the
    /// text it looks for. A needle that itself carries a character folding would
    /// rewrite, such as an em dash, is asking about that character, so folding
    /// both sides would turn "must not contain an em dash" into "must not
    /// contain a hyphen". Such a needle is matched exactly.
    pub fn for_needle(self, needle: &str) -> Self {
        match self {
            Self::Folded if !fold_characters(needle).eq(needle.chars()) => Self::Exact,
            other => other,
        }
    }

    /// For regex, where a pattern may anchor on line breaks, so whitespace is
    /// left as written.
    pub fn for_pattern<'a>(self, text: &'a str) -> Cow<'a, str> {
        match self {
            Self::Exact => Cow::Borrowed(text),
            Self::Folded => unchanged_or_owned(text, fold_characters(text).collect()),
        }
    }
}

fn fold_characters(text: &str) -> impl Iterator<Item = char> + '_ {
    text.nfkc().map(|c| match c {
        c if HYPHEN_VARIANTS.contains(&c) => '-',
        c if SINGLE_QUOTE_VARIANTS.contains(&c) => '\'',
        c if DOUBLE_QUOTE_VARIANTS.contains(&c) => '"',
        c => c,
    })
}

fn unchanged_or_owned(original: &str, folded: String) -> Cow<'_, str> {
    if folded == original {
        Cow::Borrowed(original)
    } else {
        Cow::Owned(folded)
    }
}

/// The text a substring grader actually searched, together with what it is
/// called in evidence whenever it is no longer the text as written, since a
/// byte offset only means something against the text it was counted in.
struct ComparedText<'a> {
    text: Cow<'a, str>,
    form: Option<&'static str>,
}

impl<'a> ComparedText<'a> {
    fn of(original: &'a str, normalize: TextNormalization, case: MatchCase) -> Self {
        let folded = normalize.for_search(original);
        let was_folded = folded.as_ref() != original;
        let (text, was_lowered) = match case {
            MatchCase::Sensitive => (folded, false),
            MatchCase::Insensitive => {
                let lowered = folded.to_lowercase();
                let was_lowered = lowered != folded.as_ref();
                (Cow::Owned(lowered), was_lowered)
            }
        };
        let form = match (was_folded, was_lowered) {
            (false, false) => None,
            (true, false) => Some("folded text"),
            (false, true) => Some("lowercased text"),
            (true, true) => Some("folded, lowercased text"),
        };
        Self { text, form }
    }

    fn offset(&self, offset: usize) -> String {
        match self.form {
            Some(form) => format!("byte {offset} of the {form}"),
            None => format!("byte {offset}"),
        }
    }

    fn searched(&self) -> String {
        self.form
            .map(|form| format!(" (searched the {form})"))
            .unwrap_or_default()
    }
}

impl Serialize for TextNormalization {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_bool(matches!(self, Self::Folded))
    }
}

impl<'de> Deserialize<'de> for TextNormalization {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Ok(match bool::deserialize(deserializer)? {
            true => Self::Folded,
            false => Self::Exact,
        })
    }
}

/// One token an `identifier` grader looks for as a whole name, so that
/// `rubric` is not found inside `rubrics` or `getRubric`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[schemars(schema_with = "identifier_schema")]
pub struct Identifier(String);

fn identifier_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": "string",
        "minLength": 1,
        "pattern": "^\\S+$"
    })
}

impl Identifier {
    pub fn parse(value: impl Into<String>) -> std::result::Result<Self, String> {
        let value = value.into();
        if value.is_empty() {
            return Err("identifier must not be empty".to_string());
        }
        if value.chars().any(char::is_whitespace) {
            return Err(format!(
                "identifier '{value}' holds whitespace; an identifier is one token, so use contains for a phrase"
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Byte offsets of every whole-token occurrence in `text`. A side of the
    /// identifier that ends in a name character only matches where the text does
    /// not carry on with one, which is the same rule as a regex `\b`.
    fn occurrences_in(needle: &str, text: &str) -> Vec<usize> {
        let starts_with_name = needle.chars().next().is_some_and(is_name_char);
        let ends_with_name = needle.chars().next_back().is_some_and(is_name_char);
        text.match_indices(needle)
            .filter(|(start, found)| {
                let before = text[..*start].chars().next_back();
                let after = text[start + found.len()..].chars().next();
                !(starts_with_name && before.is_some_and(is_name_char))
                    && !(ends_with_name && after.is_some_and(is_name_char))
            })
            .map(|(start, _)| start)
            .collect()
    }
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Identifier {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// How many whole-token occurrences an `identifier` grader accepts, with the
/// same rules `tool_used` holds its call counts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OccurrenceBounds(CallBounds);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
struct DeclaredOccurrenceBounds {
    #[serde(default = "at_least_one")]
    #[schemars(range(min = 0))]
    min: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0))]
    max: Option<usize>,
}

fn at_least_one() -> usize {
    1
}

impl OccurrenceBounds {
    pub fn parse(min: usize, max: Option<usize>) -> std::result::Result<Self, String> {
        match max {
            Some(max) if max < min => Err(format!(
                "max {max} is below min {min}, so no number of occurrences satisfies it"
            )),
            None if min == 0 => {
                Err("min 0 without max accepts every run; give max to say the identifier must not appear".to_string())
            }
            _ => CallBounds::parse(min, max).map(Self),
        }
    }

    pub fn admits(&self, occurrences: usize) -> bool {
        self.0.admits(occurrences)
    }

    pub fn is_at_least_once(&self) -> bool {
        self.0 == CallBounds::at_least_once()
    }
}

impl fmt::Display for OccurrenceBounds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl Serialize for OccurrenceBounds {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        DeclaredOccurrenceBounds {
            min: self.0.min(),
            max: self.0.max(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for OccurrenceBounds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let declared = DeclaredOccurrenceBounds::deserialize(deserializer)?;
        Self::parse(declared.min, declared.max).map_err(de::Error::custom)
    }
}

impl JsonSchema for OccurrenceBounds {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        DeclaredOccurrenceBounds::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        DeclaredOccurrenceBounds::json_schema(generator)
    }
}

fn case_sensitive() -> MatchCase {
    MatchCase::Sensitive
}

fn is_case_sensitive(case: &MatchCase) -> bool {
    *case == MatchCase::Sensitive
}

/// Where a run's final text first carries tool-call markup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCallMarkup {
    marker: &'static str,
    offset: usize,
}

impl ToolCallMarkup {
    pub fn find_in(text: &str) -> Option<Self> {
        let lowered = text.to_ascii_lowercase();
        TOOL_CALL_MARKUP
            .iter()
            .filter_map(|marker| lowered.find(marker).map(|offset| Self { marker, offset }))
            .min_by_key(|found| found.offset)
    }
}

impl fmt::Display for ToolCallMarkup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tool-call markup '{}' at byte {}",
            self.marker.trim_end(),
            self.offset
        )
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
        #[serde(default, skip_serializing_if = "TextNormalization::is_default")]
        normalize: TextNormalization,
    },
    Contains {
        text: NonEmptyString,
        #[serde(default)]
        target: GradeTarget,
        #[serde(default)]
        case: MatchCase,
        #[serde(default)]
        negate: bool,
        #[serde(default, skip_serializing_if = "TextNormalization::is_default")]
        normalize: TextNormalization,
    },
    /// Is a name present as a whole token, rather than as part of a longer one?
    Identifier {
        identifier: Identifier,
        #[serde(default)]
        target: GradeTarget,
        #[serde(default = "case_sensitive", skip_serializing_if = "is_case_sensitive")]
        #[schemars(extend("default" = "sensitive"))]
        case: MatchCase,
        #[serde(default, skip_serializing_if = "TextNormalization::is_default")]
        normalize: TextNormalization,
        #[serde(flatten)]
        occurrences: OccurrenceBounds,
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
    /// Fails when the final text carries tool-call markup and the run made no
    /// real tool call at all. Any real call passes it; the markup is not matched
    /// against the calls the run made.
    NoFakeToolCalls,
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
    // grader carries, because a comparison always serializes its target: there is no
    // case here where the field should be omitted for having been left at its
    // default, which is the only distinction `TargetDeclaration` exists to preserve.
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
            Self::Identifier { .. } => "identifier",
            Self::FileExists { .. } => "file_exists",
            Self::ToolUsed { .. } => "tool_used",
            Self::ToolOrder { .. } => "tool_order",
            Self::SkillUsed { .. } => "skill_used",
            Self::NoFakeToolCalls => "no_fake_tool_calls",
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
            | Self::Identifier { target, .. }
            | Self::ValidJson { target }
            | Self::SchemaValidation { target, .. }
            | Self::Baseline { target, .. } => Some(target.clone()),
            Self::Llm { target, .. } => Some(target.resolve()),
            Self::FileExists { .. }
            | Self::ToolUsed { .. }
            | Self::ToolOrder { .. }
            | Self::SkillUsed { .. }
            | Self::NoFakeToolCalls => None,
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
                normalize,
            } => {
                let verb = if *negate { "does not match" } else { "matches" };
                let exact = exact_suffix(*normalize);
                match count {
                    Some(n) => format!("{target} {verb} regex /{pattern}/{flags} exactly {n} time(s){exact}"),
                    None => format!("{target} {verb} regex /{pattern}/{flags}{exact}"),
                }
            }
            Self::Contains {
                text,
                target,
                case,
                negate,
                normalize,
            } => {
                let verb = if *negate { "does not contain" } else { "contains" };
                let suffix = match case {
                    MatchCase::Sensitive => " (case sensitive)",
                    MatchCase::Insensitive => "",
                };
                format!("{target} {verb} '{text}'{suffix}{}", exact_suffix(*normalize))
            }
            Self::Identifier {
                identifier,
                target,
                case,
                normalize,
                occurrences,
            } => {
                let counted = match occurrences.is_at_least_once() {
                    true => format!("{target} names identifier '{identifier}'"),
                    false => format!("{target} names identifier '{identifier}' {occurrences}"),
                };
                let suffix = match case {
                    MatchCase::Sensitive => "",
                    MatchCase::Insensitive => " (case insensitive)",
                };
                format!("{counted}{suffix}{}", exact_suffix(*normalize))
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
            Self::NoFakeToolCalls => {
                "final text carries no tool-call markup unless the run made a real tool call".to_string()
            }
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
            Self::ToolUsed { .. } | Self::ToolOrder { .. } | Self::SkillUsed { .. } | Self::NoFakeToolCalls
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

fn exact_suffix(normalize: TextNormalization) -> &'static str {
    match normalize {
        TextNormalization::Folded => "",
        TextNormalization::Exact => " (unnormalized)",
    }
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
/// than as text, detected from the file extension.
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
            normalize,
        } => match input.target_content(target) {
            TargetContent::Missing(reason) => GraderOutcome::Failed { evidence: reason },
            TargetContent::Image { .. } => GraderOutcome::Failed {
                evidence: format!("{target} is an image; a pattern can only match text"),
            },
            TargetContent::Text(text) => {
                let folded = normalize.for_pattern(&text);
                let at = match folded.as_ref() == text.as_str() {
                    true => "at byte",
                    false => "at folded-text byte",
                };
                let text = folded;
                let regex = match pattern.compile_normalized(flags, *normalize) {
                    Ok(regex) => regex,
                    Err(e) => {
                        return GraderOutcome::AuthoringError {
                            reason: format!(
                                "pattern /{pattern}/ no longer compiles once folded ({e}); set normalize to false"
                            ),
                        }
                    }
                };
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
                                "{target} matched /{pattern}/{flags} {at} {}: '{}'",
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
            normalize,
        } => match input.target_content(target) {
            TargetContent::Missing(reason) => GraderOutcome::Failed { evidence: reason },
            TargetContent::Image { .. } => GraderOutcome::Failed {
                evidence: format!("{target} is an image; it cannot be searched for text"),
            },
            TargetContent::Text(haystack) => {
                let normalize = normalize.for_needle(text.as_str());
                let searched = ComparedText::of(&haystack, normalize, *case);
                let needle = ComparedText::of(text.as_str(), normalize, *case);
                let found = searched.text.find(needle.text.as_ref());
                let evidence = match found {
                    Some(offset) => format!(
                        "{target} ({} bytes) contains '{text}' at {}",
                        haystack.len(),
                        searched.offset(offset)
                    ),
                    None => format!(
                        "{target} ({} bytes) does not contain '{text}'{}",
                        haystack.len(),
                        searched.searched()
                    ),
                };
                GraderOutcome::from_bool(found.is_some() != *negate, evidence)
            }
        },
        Grader::Identifier {
            identifier,
            target,
            case,
            normalize,
            occurrences,
        } => match input.target_content(target) {
            TargetContent::Missing(reason) => GraderOutcome::Failed { evidence: reason },
            TargetContent::Image { .. } => GraderOutcome::Failed {
                evidence: format!("{target} is an image; it cannot be searched for an identifier"),
            },
            TargetContent::Text(haystack) => {
                let normalize = normalize.for_needle(identifier.as_str());
                let searched = ComparedText::of(&haystack, normalize, *case);
                let needle = ComparedText::of(identifier.as_str(), normalize, *case);
                let found = Identifier::occurrences_in(&needle.text, &searched.text);
                let first = match found.first() {
                    Some(offset) => format!(", first at {}", searched.offset(*offset)),
                    None => searched.searched(),
                };
                GraderOutcome::from_bool(
                    occurrences.admits(found.len()),
                    format!(
                        "{target} names '{identifier}' as a whole token {} time(s){first}, expected {occurrences}",
                        found.len()
                    ),
                )
            }
        },
        Grader::NoFakeToolCalls => match ToolCallMarkup::find_in(input.final_text) {
            None => GraderOutcome::Passed {
                evidence: "final text carries no tool-call markup".to_string(),
            },
            Some(markup) => with_transcript(input, |transcript| {
                let real = transcript.tool_calls().count();
                match real {
                    0 => GraderOutcome::Failed {
                        evidence: format!(
                            "final text carries {markup}, but the run made no real tool call, so the call was written out as text"
                        ),
                    },
                    _ => GraderOutcome::Passed {
                        evidence: format!("final text carries {markup}, and the run made {real} real tool call(s)"),
                    },
                }
            }),
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
#[path = "graders_tests.rs"]
mod tests;
