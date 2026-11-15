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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
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
        normalize: TextNormalization::default(),
    };
    assert!(matches!(evaluate(&grader, &input), GraderOutcome::Passed { .. }));

    let missing = Grader::Contains {
        text: text("never-written.txt"),
        target: GradeTarget::CreatedFiles,
        case: MatchCase::Sensitive,
        negate: false,
        normalize: TextNormalization::default(),
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
                normalize: TextNormalization::default(),
            },
            "final text matches regex /foo/",
        ),
        (
            Grader::Contains {
                text: text("bar"),
                target: GradeTarget::File(path("out.md")),
                case: MatchCase::Insensitive,
                negate: true,
                normalize: TextNormalization::default(),
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
        (
            Grader::Identifier {
                identifier: Identifier::parse("rubric").unwrap(),
                target: GradeTarget::FinalText,
                case: MatchCase::Sensitive,
                normalize: TextNormalization::default(),
                occurrences: OccurrenceBounds::default(),
            },
            "final text names identifier 'rubric'",
        ),
        (
            Grader::Identifier {
                identifier: Identifier::parse("Fact.member").unwrap(),
                target: GradeTarget::FinalText,
                case: MatchCase::Insensitive,
                normalize: TextNormalization::Exact,
                occurrences: OccurrenceBounds::parse(0, Some(0)).unwrap(),
            },
            "final text names identifier 'Fact.member' never (case insensitive) (unnormalized)",
        ),
        (
            Grader::NoFakeToolCalls,
            "final text carries no tool-call markup unless the run made a real tool call",
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
        {"type": "schema_validation", "schema": "schema.json"},
        {"type": "identifier", "identifier": "rubric", "case": "insensitive", "max": 3},
        {"type": "no_fake_tool_calls"},
        {"type": "contains", "text": "2.1.0-M1", "normalize": false}
    ]);
    let graders: Vec<Grader> = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(graders.len(), 12);
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
            "schema_validation",
            "identifier",
            "no_fake_tool_calls",
            "contains"
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
        normalize: TextNormalization::default(),
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
            normalize: TextNormalization::default(),
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

fn graded_text(grader: serde_json::Value, final_text: &str, stream: &[u8]) -> GraderOutcome {
    let fixture = Fixture::of(stream);
    let input = GradeInput {
        final_text,
        ..fixture.input()
    };
    evaluate(&serde_json::from_value(grader).expect("grader parses"), &input)
}

const NO_TOOL_STREAM: &[u8] = br#"{"type":"result","is_error":false,"result":"done"}
"#;

#[test]
fn contains_matches_a_version_written_with_a_non_breaking_hyphen() {
    let grader = serde_json::json!({"type": "contains", "text": "2.1.0-M1"});
    match graded_text(grader, "Upgrade to 2.1.0\u{2011}M1 today.", CLAUDE_STREAM) {
        GraderOutcome::Passed { evidence } => assert!(evidence.contains("of the folded"), "{evidence}"),
        other => panic!("expected a pass, got {other:?}"),
    }
}

#[test]
fn contains_folds_dashes_quotes_compatibility_forms_and_whitespace() {
    for (needle, haystack) in [
        ("a-b", "a\u{2013}b"),
        ("a-b", "a\u{2212}b"),
        ("a-b", "a\u{2014}b"),
        ("it's \"done\"", "it\u{2019}s \u{201C}done\u{201D}"),
        ("ABC 12", "\u{FF21}\u{FF22}\u{FF23}\u{00A0}\u{FF11}\u{FF12}"),
        ("grew 12% in Q3", "grew   12%\n\tin Q3"),
        ("file", "\u{FB01}le"),
    ] {
        let grader = serde_json::json!({"type": "contains", "text": needle, "case": "sensitive"});
        assert!(
            matches!(
                graded_text(grader, haystack, CLAUDE_STREAM),
                GraderOutcome::Passed { .. }
            ),
            "'{needle}' in '{haystack}'"
        );
    }
}

#[test]
fn contains_collapses_whitespace_in_the_needle_as_well_as_the_text() {
    let grader = serde_json::json!({"type": "contains", "text": "grew  12%\nin Q3"});
    assert!(matches!(
        graded_text(grader, "Revenue grew 12% in\tQ3.", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
}

#[test]
fn a_needle_carrying_a_variant_character_is_matched_exactly() {
    let grader = || serde_json::json!({"type": "contains", "text": "2.1.0\u{2011}M1"});
    assert!(matches!(
        graded_text(grader(), "Upgrade to 2.1.0\u{2011}M1 today.", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
    assert!(matches!(
        graded_text(grader(), "Upgrade to 2.1.0-M1 today.", CLAUDE_STREAM),
        GraderOutcome::Failed { .. }
    ));
}

#[test]
fn contains_compares_text_as_written_when_normalization_is_opted_out() {
    let grader = serde_json::json!({"type": "contains", "text": "2.1.0-M1", "normalize": false});
    assert!(matches!(
        graded_text(grader, "Upgrade to 2.1.0\u{2011}M1 today.", CLAUDE_STREAM),
        GraderOutcome::Failed { .. }
    ));
}

#[test]
fn regex_folds_characters_but_keeps_line_breaks() {
    let folded = serde_json::json!({"type": "regex", "pattern": r"2\.1\.0-M1"});
    assert!(matches!(
        graded_text(folded, "2.1.0\u{2011}M1", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
    let anchored = serde_json::json!({"type": "regex", "pattern": "^second$", "flags": "m"});
    assert!(matches!(
        graded_text(anchored, "first\n\nsecond", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
    let exact = serde_json::json!({"type": "regex", "pattern": r"2\.1\.0-M1", "normalize": false});
    assert!(matches!(
        graded_text(exact, "2.1.0\u{2011}M1", CLAUDE_STREAM),
        GraderOutcome::Failed { .. }
    ));
}

#[test]
fn normalization_is_omitted_from_the_manifest_unless_opted_out() {
    let default: Grader = serde_json::from_value(serde_json::json!({"type": "contains", "text": "x"})).unwrap();
    assert!(serde_json::to_value(&default).unwrap().get("normalize").is_none());
    let exact: Grader =
        serde_json::from_value(serde_json::json!({"type": "contains", "text": "x", "normalize": false})).unwrap();
    assert_eq!(serde_json::to_value(&exact).unwrap()["normalize"], false);
}

#[test]
fn identifier_matches_only_the_whole_token() {
    let grader = || serde_json::json!({"type": "identifier", "identifier": "rubric"});
    assert!(matches!(
        graded_text(grader(), "Each rubric scores one thing.", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
    for haystack in ["Two rubrics.", "call getRubric()", "my_rubric", "rubric2", "Rubric"] {
        assert!(
            matches!(
                graded_text(grader(), haystack, CLAUDE_STREAM),
                GraderOutcome::Failed { .. }
            ),
            "{haystack}"
        );
    }
}

#[test]
fn identifier_is_case_insensitive_only_when_asked() {
    let grader = serde_json::json!({"type": "identifier", "identifier": "rubric", "case": "insensitive"});
    assert!(matches!(
        graded_text(grader.clone(), "The Rubric holds.", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
    assert!(matches!(
        graded_text(grader, "call getRubric()", CLAUDE_STREAM),
        GraderOutcome::Failed { .. }
    ));
}

#[test]
fn identifier_with_punctuation_matches_a_qualified_name() {
    let grader = || serde_json::json!({"type": "identifier", "identifier": "Fact.member"});
    assert!(matches!(
        graded_text(grader(), "Use `Fact.member(x)` here.", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
    assert!(matches!(
        graded_text(grader(), "Use Fact.members here.", CLAUDE_STREAM),
        GraderOutcome::Failed { .. }
    ));
}

#[test]
fn identifier_counts_occurrences_against_its_bounds() {
    let text = "rubric, rubrics, rubric.";
    let exactly_two = serde_json::json!({"type": "identifier", "identifier": "rubric", "min": 2, "max": 2});
    match graded_text(exactly_two, text, CLAUDE_STREAM) {
        GraderOutcome::Passed { evidence } => assert!(evidence.contains("2 time(s)"), "{evidence}"),
        other => panic!("expected a pass, got {other:?}"),
    }
    let absent = serde_json::json!({"type": "identifier", "identifier": "rubric", "min": 0, "max": 0});
    assert!(matches!(
        graded_text(absent, text, CLAUDE_STREAM),
        GraderOutcome::Failed { .. }
    ));
}

#[test]
fn identifier_rejects_bounds_and_tokens_that_cannot_mean_anything() {
    for (json, expected) in [
        (
            serde_json::json!({"type": "identifier", "identifier": "a", "min": 0}),
            "min 0",
        ),
        (
            serde_json::json!({"type": "identifier", "identifier": "a", "min": 3, "max": 1}),
            "below min",
        ),
        (
            serde_json::json!({"type": "identifier", "identifier": "two words"}),
            "one token",
        ),
        (serde_json::json!({"type": "identifier", "identifier": ""}), "empty"),
    ] {
        let err = serde_json::from_value::<Grader>(json).unwrap_err();
        assert!(err.to_string().contains(expected), "{err}");
    }
}

#[test]
fn identifier_folds_a_non_breaking_hyphen() {
    let grader = serde_json::json!({"type": "identifier", "identifier": "2.1.0-M1"});
    assert!(matches!(
        graded_text(grader, "pin 2.1.0\u{2011}M1 now", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
}

#[test]
fn tool_call_markup_without_a_real_call_fails() {
    for text in [
        "<tool>web_search</tool> The results say Rust 2.0 shipped.",
        "<tool_call>{\"name\": \"search\"}</tool_call>",
        "<function_call>search()</function_call>",
        "<invoke name=\"search\">",
        "<|channel|>commentary to=functions.search",
        "<|tool_call|>search",
        "```tool_call\nsearch(\"x\")\n```",
        "<TOOL_CALL>search</TOOL_CALL>",
    ] {
        match graded_text(serde_json::json!({"type": "no_fake_tool_calls"}), text, NO_TOOL_STREAM) {
            GraderOutcome::Failed { evidence } => assert!(evidence.contains("no real tool call"), "{evidence}"),
            other => panic!("'{text}' should fail, got {other:?}"),
        }
    }
}

#[test]
fn tool_call_markup_passes_when_the_run_made_real_calls() {
    match graded_text(
        serde_json::json!({"type": "no_fake_tool_calls"}),
        "<tool>Read</tool>",
        CLAUDE_STREAM,
    ) {
        GraderOutcome::Passed { evidence } => assert!(evidence.contains("3 real tool call(s)"), "{evidence}"),
        other => panic!("expected a pass, got {other:?}"),
    }
}

#[test]
fn text_without_tool_call_markup_passes_even_without_a_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let input = GradeInput {
        final_text: "A <tooltip> is not a tool call.",
        run_dir: dir.path(),
        workspace_dir: dir.path(),
        outputs_dir: dir.path(),
        raw_transcript: "",
        transcript: None,
        skill_dir: dir.path(),
        created_files: &[],
    };
    assert!(matches!(
        evaluate(&Grader::NoFakeToolCalls, &input),
        GraderOutcome::Passed { .. }
    ));
}

#[test]
fn tool_call_markup_is_unsupported_where_real_calls_cannot_be_seen() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = NormalizedTranscript::unavailable("mystery-runner");
    let input = GradeInput {
        final_text: "<tool>web_search</tool>",
        run_dir: dir.path(),
        workspace_dir: dir.path(),
        outputs_dir: dir.path(),
        raw_transcript: "",
        transcript: Some(&transcript),
        skill_dir: dir.path(),
        created_files: &[],
    };
    assert!(matches!(
        evaluate(&Grader::NoFakeToolCalls, &input),
        GraderOutcome::Unsupported { .. }
    ));
}

#[test]
fn regex_folds_its_pattern_so_an_em_dash_pattern_matches_em_dash_text() {
    let grader = serde_json::json!({"type": "regex", "pattern": "a\u{2014}b"});
    assert!(matches!(
        graded_text(grader, "a\u{2014}b", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
}

#[test]
fn a_negated_contains_for_an_em_dash_passes_on_ascii_hyphens() {
    let grader = serde_json::json!({"type": "contains", "text": "\u{2014}", "negate": true});
    assert!(matches!(
        graded_text(grader, "well-known, low-key", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
}

#[test]
fn a_contains_with_a_smart_quote_matches_smart_quote_text_exactly() {
    let grader = || serde_json::json!({"type": "contains", "text": "it\u{2019}s"});
    assert!(matches!(
        graded_text(grader(), "Yes, it\u{2019}s done.", CLAUDE_STREAM),
        GraderOutcome::Passed { .. }
    ));
    assert!(matches!(
        graded_text(grader(), "Yes, it's done.", CLAUDE_STREAM),
        GraderOutcome::Failed { .. }
    ));
}

#[test]
fn contains_evidence_names_the_text_its_offset_counts_into() {
    let grader = serde_json::json!({"type": "contains", "text": "done"});
    match graded_text(grader, "DONE", CLAUDE_STREAM) {
        GraderOutcome::Passed { evidence } => {
            assert!(evidence.contains("byte 0 of the lowercased text"), "{evidence}")
        }
        other => panic!("expected a pass, got {other:?}"),
    }
}
