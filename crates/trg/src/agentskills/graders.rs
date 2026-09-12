use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use regex::Regex;
use schemars::JsonSchema;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use super::evals::{NonEmptyString, RelativeSkillPath};
use super::transcript::{NormalizedTranscript, ToolName};

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
}

impl fmt::Display for GradeTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FinalText => f.write_str("final text"),
            Self::Transcript => f.write_str("transcript"),
            Self::AnyOutput => f.write_str("any output file"),
            Self::File(path) => write!(f, "file '{path}'"),
        }
    }
}

fn one() -> NonZeroUsize {
    NonZeroUsize::new(1).expect("1 is non-zero")
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
        path: RelativeSkillPath,
    },
    ToolUsed {
        tool: ToolName,
        #[serde(default = "one")]
        min_calls: NonZeroUsize,
    },
    ToolOrder {
        #[schemars(length(min = 2))]
        tools: Vec<ToolName>,
    },
    SkillUsed,
    Llm {
        criterion: NonEmptyString,
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
            Self::SkillUsed => "skill_used",
            Self::Llm { .. } => "llm",
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
            } => {
                let verb = if *negate { "does not match" } else { "matches" };
                format!("{target} {verb} regex /{pattern}/")
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
            Self::FileExists { path } => format!("file '{path}' exists"),
            Self::ToolUsed { tool, min_calls } => {
                if min_calls.get() == 1 {
                    format!("tool '{tool}' was used")
                } else {
                    format!("tool '{tool}' was used at least {min_calls} times")
                }
            }
            Self::ToolOrder { tools } => {
                let names = tools.iter().map(ToolName::as_str).collect::<Vec<_>>().join(" then ");
                format!("tools were used in order: {names}")
            }
            Self::SkillUsed => "the skill was engaged".to_string(),
            Self::Llm { criterion } => criterion.to_string(),
        }
    }

    pub fn needs_tool_visibility(&self) -> bool {
        matches!(self, Self::ToolUsed { .. } | Self::ToolOrder { .. } | Self::SkillUsed)
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetContent {
    Text(String),
    Missing(String),
}

impl<'a> GradeInput<'a> {
    /// Declared outputs win, then whatever the agent left in its working
    /// directory, then the run directory for files placed there out of band.
    /// An unfound path reports as the workspace candidate, since that is where
    /// the agent was asked to write.
    pub fn resolve(&self, path: &RelativeSkillPath) -> PathBuf {
        let workspace_candidate = self.workspace_dir.join(path.as_path());
        for candidate in [
            self.outputs_dir.join(path.as_path()),
            workspace_candidate.clone(),
            self.run_dir.join(path.as_path()),
        ] {
            if candidate.exists() {
                return candidate;
            }
        }
        workspace_candidate
    }

    fn target_content(&self, target: &GradeTarget) -> TargetContent {
        match target {
            GradeTarget::FinalText => TargetContent::Text(self.final_text.to_string()),
            GradeTarget::Transcript => TargetContent::Text(self.raw_transcript.to_string()),
            GradeTarget::File(path) => {
                let resolved = self.resolve(path);
                match std::fs::read_to_string(&resolved) {
                    Ok(text) => TargetContent::Text(text),
                    Err(e) => TargetContent::Missing(format!("cannot read '{}': {e}", resolved.display())),
                }
            }
            GradeTarget::AnyOutput => {
                let mut parts = vec![self.final_text.to_string()];
                collect_text(self.outputs_dir, &mut parts);
                TargetContent::Text(parts.join("\n"))
            }
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

pub fn evaluate(grader: &Grader, input: &GradeInput) -> GraderOutcome {
    match grader {
        Grader::Llm { criterion } => GraderOutcome::Deferred {
            criterion: criterion.to_string(),
        },
        Grader::FileExists { path } => {
            let resolved = input.resolve(path);
            let evidence = match std::fs::metadata(&resolved) {
                Ok(meta) if meta.is_file() => {
                    format!("'{}' exists and holds {} bytes", resolved.display(), meta.len())
                }
                Ok(_) => format!("'{}' exists but is not a file", resolved.display()),
                Err(e) => format!("'{}' could not be read: {e}", resolved.display()),
            };
            GraderOutcome::from_bool(resolved.is_file(), evidence)
        }
        Grader::Regex {
            pattern,
            target,
            negate,
        } => match input.target_content(target) {
            TargetContent::Missing(reason) => GraderOutcome::Failed { evidence: reason },
            TargetContent::Text(text) => {
                let found = pattern.compile().find(&text);
                let evidence = match &found {
                    Some(m) => format!("{target} matched /{pattern}/ at byte {}: '{}'", m.start(), m.as_str()),
                    None => format!("{target} did not match /{pattern}/"),
                };
                GraderOutcome::from_bool(found.is_some() != *negate, evidence)
            }
        },
        Grader::Contains {
            text,
            target,
            case,
            negate,
        } => match input.target_content(target) {
            TargetContent::Missing(reason) => GraderOutcome::Failed { evidence: reason },
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
        Grader::ToolUsed { tool, min_calls } => with_transcript(input, |transcript| {
            let calls = transcript.tool_call_count(tool.as_str());
            GraderOutcome::from_bool(
                calls >= min_calls.get(),
                format!("'{tool}' was called {calls} time(s), needed {min_calls}"),
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
        Grader::SkillUsed => with_transcript(input, |transcript| {
            let engagement = transcript.skill_engagement();
            match engagement.engaged() {
                Some(engaged) => GraderOutcome::from_bool(engaged, engagement.describe().to_string()),
                None => GraderOutcome::Unsupported {
                    reason: unobservable_reason(transcript),
                },
            }
        }),
    }
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
        transcript: NormalizedTranscript,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let run_dir = dir.path().to_path_buf();
            let workspace_dir = run_dir.join("workspace");
            let outputs_dir = workspace_dir.join("outputs");
            std::fs::create_dir_all(&outputs_dir).unwrap();
            std::fs::write(outputs_dir.join("report.md"), "# Title\nRevenue grew 12% in Q3.\n").unwrap();
            Self {
                _dir: dir,
                run_dir,
                workspace_dir,
                outputs_dir,
                transcript: normalize_stream_json(
                    "claude",
                    &redact_transcript_bytes(CLAUDE_STREAM),
                    &WorkspaceBoundary::unknown(),
                ),
            }
        }

        fn write_to_workspace(&self, name: &str, contents: &str) {
            std::fs::write(self.workspace_dir.join(name), contents).unwrap();
        }

        fn input(&self) -> GradeInput<'_> {
            GradeInput {
                final_text: "Wrote the report to outputs/report.md",
                run_dir: &self.run_dir,
                workspace_dir: &self.workspace_dir,
                outputs_dir: &self.outputs_dir,
                raw_transcript: "",
                transcript: Some(&self.transcript),
            }
        }
    }

    fn path(value: &str) -> RelativeSkillPath {
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
        };
        let outcome = evaluate(&grader, &fixture.input());
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");
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
                path: path("report.md"),
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(&Grader::FileExists { path: path("nope.md") }, &fixture.input());
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn file_exists_finds_what_the_agent_wrote_in_its_working_directory() {
        let fixture = Fixture::new();
        fixture.write_to_workspace("summary.md", "May revenue was up.\n");

        let outcome = evaluate(
            &Grader::FileExists {
                path: path("summary.md"),
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
        match evaluate(&Grader::FileExists { path: path("nope.md") }, &fixture.input()) {
            GraderOutcome::Failed { evidence } => assert!(evidence.contains("workspace/nope.md"), "{evidence}"),
            other => panic!("expected fail, got {other:?}"),
        }
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
                min_calls: one(),
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Passed { .. }), "{outcome:?}");

        let outcome = evaluate(
            &Grader::ToolUsed {
                tool: tool("Read"),
                min_calls: NonZeroUsize::new(2).unwrap(),
            },
            &fixture.input(),
        );
        assert!(matches!(outcome, GraderOutcome::Failed { .. }), "{outcome:?}");
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
            evaluate(&Grader::SkillUsed, &fixture.input()),
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
        };

        for grader in [
            Grader::SkillUsed,
            Grader::ToolUsed {
                tool: tool("Read"),
                min_calls: one(),
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
        };

        assert!(matches!(
            evaluate(&Grader::SkillUsed, &input),
            GraderOutcome::Passed { .. }
        ));
        assert!(matches!(
            evaluate(
                &Grader::ToolUsed {
                    tool: tool("command_execution"),
                    min_calls: one(),
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
        };
        let grader = Grader::Regex {
            pattern: RegexPattern::parse(r"\b42\b").unwrap(),
            target: GradeTarget::FinalText,
            negate: false,
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
        };
        match evaluate(&Grader::SkillUsed, &input) {
            GraderOutcome::Unsupported { reason } => assert!(reason.contains("no normalized transcript"), "{reason}"),
            other => panic!("expected unsupported, got {other:?}"),
        }
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
        };
        let grader = Grader::Llm {
            criterion: text("The summary avoids filler phrasing"),
        };
        assert_eq!(
            evaluate(&grader, &input),
            GraderOutcome::Deferred {
                criterion: "The summary avoids filler phrasing".to_string()
            }
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
            (Grader::FileExists { path: path("out.md") }, "file 'out.md' exists"),
            (
                Grader::ToolUsed {
                    tool: tool("Read"),
                    min_calls: one(),
                },
                "tool 'Read' was used",
            ),
            (
                Grader::ToolOrder {
                    tools: vec![tool("Read"), tool("Write")],
                },
                "tools were used in order: Read then Write",
            ),
            (Grader::SkillUsed, "the skill was engaged"),
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
            {"type": "llm", "criterion": "reads naturally"}
        ]);
        let graders: Vec<Grader> = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(graders.len(), 7);
        assert_eq!(
            graders.iter().map(Grader::kind).collect::<Vec<_>>(),
            vec![
                "regex",
                "contains",
                "file_exists",
                "tool_used",
                "tool_order",
                "skill_used",
                "llm"
            ]
        );
        let reparsed: Vec<Grader> = serde_json::from_value(serde_json::to_value(&graders).unwrap()).unwrap();
        assert_eq!(reparsed, graders);
    }

    #[test]
    fn min_calls_of_zero_is_rejected() {
        let err = serde_json::from_value::<Grader>(serde_json::json!({
            "type": "tool_used",
            "tool": "Read",
            "min_calls": 0
        }))
        .unwrap_err();
        assert!(err.to_string().contains("zero"), "{err}");
    }
}
