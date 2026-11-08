use super::*;
use crate::agentskills::evals::{EvalSplit, RelativeSkillPath};
use crate::agentskills::redact::redact_transcript_bytes;
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, RunMetrics, RunNotStarted, RunPaths, ScenarioKind,
    WriteReportOptions,
};
use crate::agentskills::runner::capabilities::HarnessControl;
use crate::agentskills::runner::Runner;
use crate::agentskills::span_reference::SpanReference;
use crate::agentskills::transcript::write_normalized_transcript;
use crate::agentskills::transcript::{normalize_stream_json, WorkspaceBoundary};
use crate::fs::testutil::MemFS;
use crate::telemetry::semconv::generated::attributes::{GEN_AI_EVALUATION_EXPLANATION, GEN_AI_EVALUATION_NAME};
use opentelemetry_semantic_conventions::attribute::{PROCESS_EXIT_CODE, PROCESS_PID};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

const UNOBSERVABLE_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used", "arm": "both"},
                    {"type": "contains", "text": "all done"}
                ]
            }
        ]
    }"#;

const ALL_UNSUPPORTED_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used", "arm": "both"}
                ]
            }
        ]
    }"#;

const MOCK_CALLS_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "contains", "text": "github.create_issue", "target": "mock_calls"},
                    {"type": "contains", "text": "all done"}
                ]
            }
        ]
    }"#;

fn unobservable_report_dir(temp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    unobservable_report_dir_with_suite(temp, UNOBSERVABLE_SUITE)
}

fn unobservable_report_dir_with_suite(temp: &tempfile::TempDir, suite_json: &str) -> (PathBuf, PathBuf) {
    let skill_dir = temp.path().join("demo-skill");
    fs::create_dir_all(skill_dir.join("evals")).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    )
    .unwrap();
    fs::write(skill_dir.join("evals/evals.json"), suite_json).unwrap();

    let mem = MemFS::new();
    let mem_skill = Path::new("demo-skill");
    mem.insert(
        mem_skill.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    mem.insert(mem_skill.join("evals/evals.json"), UNOBSERVABLE_SUITE);

    let bundle = build_report_bundle(
        &mem,
        mem_skill,
        &skill_dir,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("report-unsupported".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            runner: Some("codex".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let report_dir = write_report_bundle(&temp.path().join("out"), &bundle, WriteReportOptions::default()).unwrap();

    let workspace = report_dir.join(&bundle.document.runs[0].paths.workspace);
    let run_dir = workspace.parent().unwrap().to_path_buf();
    fs::write(workspace.join("outputs").join(FINAL_MD), "all done\n").unwrap();
    let transcript_path = run_dir.join("transcript.jsonl");
    fs::write(&transcript_path, "{\"type\":\"turn.completed\"}\n").unwrap();
    write_normalized_transcript(&transcript_path, &NormalizedTranscript::unavailable("codex")).unwrap();

    (report_dir, run_dir)
}

#[test]
fn a_mock_calls_grader_on_a_harness_that_hosts_no_mock_server_is_unsupported_not_failed() {
    let temp = tempdir().unwrap();
    let (report_dir, run_dir) = unobservable_report_dir_with_suite(&temp, MOCK_CALLS_SUITE);

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.unsupported, 1);
    assert_eq!(report.passed, 1);
    assert_eq!(
        report.failed, 0,
        "a harness that cannot host a mock must not read as a skill that called one wrong"
    );

    let grading: GradingFile =
        serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
    let unsupported = grading
        .assertion_results
        .iter()
        .find(|result| result.is_unsupported())
        .unwrap();
    assert!(
        unsupported
            .unsupported
            .as_deref()
            .unwrap()
            .contains("no mcp mock server"),
        "{unsupported:?}"
    );
    assert_eq!(grading.summary.pass_rate, Some(1.0));
}

const ENGAGED_STREAM: &[u8] = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":".skill/SKILL.md"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/summary.md"}}]}}
{"type":"result","is_error":false,"result":"all done"}
"#;

const IMPROVISED_STREAM: &[u8] = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/summary.md"}}]}}
{"type":"result","is_error":false,"result":"all done"}
"#;

/// Both arms of a suite, each run leaving the same final text so the only
/// thing that differs between them is whether the skill was engaged.
fn both_arms_report_dir(temp: &tempfile::TempDir, suite_json: &str) -> PathBuf {
    let skill_dir = temp.path().join("demo-skill");
    fs::create_dir_all(skill_dir.join("evals")).unwrap();
    let skill_md = "---\nname: demo-skill\ndescription: d\n---\n";
    fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
    fs::write(skill_dir.join("evals/evals.json"), suite_json).unwrap();

    let mem = MemFS::new();
    let mem_skill = Path::new("demo-skill");
    mem.insert(mem_skill.join("SKILL.md"), skill_md);
    mem.insert(mem_skill.join("evals/evals.json"), suite_json);

    let bundle = build_report_bundle(
        &mem,
        mem_skill,
        &skill_dir,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions {
            report_id: Some("report-arms".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            runner: Some("claude-code".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let report_dir = write_report_bundle(&temp.path().join("out"), &bundle, WriteReportOptions::default()).unwrap();

    for run in &bundle.document.runs {
        let workspace = report_dir.join(&run.paths.workspace);
        let run_dir = workspace.parent().unwrap().to_path_buf();
        fs::write(workspace.join("outputs").join(FINAL_MD), "all done\n").unwrap();
        let stream = match run.scenario_id {
            ScenarioKind::WithSkill => ENGAGED_STREAM,
            _ => IMPROVISED_STREAM,
        };
        let transcript_path = run_dir.join("transcript.jsonl");
        fs::write(&transcript_path, stream).unwrap();
        write_normalized_transcript(
            &transcript_path,
            &normalize_stream_json(
                "claude-code",
                &redact_transcript_bytes(stream),
                &WorkspaceBoundary::unknown(),
            ),
        )
        .unwrap();
    }

    report_dir
}

fn grading_files_by_scenario(report_dir: &Path) -> Vec<(ScenarioKind, GradingFile)> {
    let document: ReportDocument =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    document
        .runs
        .iter()
        .map(|run| {
            let workspace = report_dir.join(&run.paths.workspace);
            let run_dir = workspace.parent().unwrap();
            let grading: GradingFile =
                serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
            (run.scenario_id, grading)
        })
        .collect()
}

const ARM_SCOPED_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used"},
                    {"type": "contains", "text": "all done"}
                ]
            }
        ]
    }"#;

const TRIGGERING_ONLY_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "skill_used"}
                ]
            }
        ]
    }"#;

#[test]
fn a_check_that_presupposes_the_skill_scores_in_neither_arm() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, ARM_SCOPED_SUITE);

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.excluded, 2, "one skill_used result per arm");
    assert_eq!(report.assertions_graded, 4);
    assert_eq!(report.passed, 2);
    assert_eq!(report.failed, 0);

    for (scenario, grading) in grading_files_by_scenario(&report_dir) {
        assert_eq!(grading.summary.excluded, 1, "{scenario:?}");
        assert_eq!(
            grading.summary.pass_rate,
            Some(1.0),
            "{scenario:?} must not be scored on the skill's own premise"
        );
        let skill_result = grading
            .assertion_results
            .iter()
            .find(|result| result.assertion.contains("skill was engaged"))
            .expect("the indicator is still reported");
        assert!(skill_result.is_excluded());
        assert_eq!(skill_result.passed, scenario == ScenarioKind::WithSkill);
    }
}

#[test]
fn a_case_whose_every_check_is_arm_scoped_is_scored_as_declared() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, TRIGGERING_ONLY_SUITE);

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.excluded, 0, "excluding everything would measure nothing");
    assert_eq!(report.passed, 1);
    assert_eq!(report.failed, 1);

    for (_, grading) in grading_files_by_scenario(&report_dir) {
        assert_eq!(grading.summary.excluded, 0);
        assert!(grading.summary.pass_rate.is_some());
    }
}

#[test]
fn a_case_scored_only_by_unsupported_checks_has_no_case_score() {
    let temp = tempdir().unwrap();
    let (report_dir, _run_dir) = unobservable_report_dir_with_suite(&temp, ALL_UNSUPPORTED_SUITE);

    grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    let document: ReportDocument =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let run = &document.runs[0];
    assert_eq!(
        run.case_score, None,
        "a case scored on nothing must not read as a score of zero"
    );
}

#[test]
fn a_case_scored_and_failing_every_check_reports_a_score_of_zero_not_no_score() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, TRIGGERING_ONLY_SUITE);

    grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    let document: ReportDocument =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    for run in &document.runs {
        let workspace = report_dir.join(&run.paths.workspace);
        let run_dir = workspace.parent().unwrap();
        let grading: GradingFile =
            serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
        let expected = GradingCounts::tally(&grading.assertion_results).pass_rate();
        assert_eq!(run.case_score, expected, "{:?}", run.scenario_id);
    }

    let with_skill = document
        .runs
        .iter()
        .find(|r| r.scenario_id == ScenarioKind::WithSkill)
        .unwrap();
    let without_skill = document
        .runs
        .iter()
        .find(|r| r.scenario_id == ScenarioKind::WithoutSkill)
        .unwrap();
    assert_eq!(with_skill.case_score, Some(1.0));
    assert_eq!(
        without_skill.case_score,
        Some(0.0),
        "a case that scored and failed everything is a zero, not a missing score"
    );
}

#[test]
fn grading_recomputes_pass_at_k_and_pass_pow_k_rather_than_leaving_them_at_zero_trials() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, TRIGGERING_ONLY_SUITE);

    let before: ReportDocument =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    for summary in &before.summaries.by_scenario {
        assert_eq!(
            summary.pass_at_k.trials, 0,
            "a run report written before grading has no case_score to draw a k rate from yet"
        );
    }

    grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    let after: ReportDocument =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let with_skill = after
        .summaries
        .by_scenario
        .iter()
        .find(|summary| summary.scenario_id == ScenarioKind::WithSkill)
        .unwrap();
    let without_skill = after
        .summaries
        .by_scenario
        .iter()
        .find(|summary| summary.scenario_id == ScenarioKind::WithoutSkill)
        .unwrap();

    assert_eq!(
        with_skill.pass_at_k.trials, 1,
        "grading must recompute the k rates, not leave the pre-grading placeholder in the written report"
    );
    assert_eq!(with_skill.pass_at_k.rate, Some(1.0));
    assert_eq!(with_skill.pass_pow_k.rate, Some(1.0));
    assert_eq!(without_skill.pass_at_k.trials, 1);
    assert_eq!(without_skill.pass_at_k.rate, Some(0.0));
    assert_eq!(without_skill.pass_pow_k.rate, Some(0.0));
}

#[test]
fn a_run_the_harness_never_started_is_withheld_from_grading() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, ARM_SCOPED_SUITE);

    let report_path = report_dir.join("report.json");
    let mut document: ReportDocument = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    document.runs[0].not_started(RunNotStarted::ControlUnsupported {
        control: HarnessControl::ConversationSeeding,
        runner: Runner::ClaudeCode,
    });
    let abandoned = report_dir.join(&document.runs[0].paths.workspace);
    fs::remove_dir_all(&abandoned).unwrap();
    document.runs[1].status = "completed".to_string();
    fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        report.runs_graded, 1,
        "a run that never reached the harness has no workspace to grade"
    );
    assert_eq!(report.run_statuses.skipped, 1);
    assert_eq!(report.run_statuses.completed, 1);
    assert_eq!(
        report.failed, 0,
        "a run trg declined to start must not be reported as a regression"
    );
}

const NAMED_GRADER_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "contains", "text": "all done", "name": "wraps-up"}
                ]
            }
        ]
    }"#;

#[test]
fn a_named_grader_carries_its_name_into_the_grading_result() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, NAMED_GRADER_SUITE);

    grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    for (scenario, grading) in grading_files_by_scenario(&report_dir) {
        let named_result = grading
            .assertion_results
            .iter()
            .find(|result| result.name.as_deref() == Some("wraps-up"))
            .unwrap_or_else(|| panic!("{scenario:?} must report the declared grader name"));
        assert!(named_result.assertion.contains("all done"));
    }
}

const WEIGHTED_BASELINE_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {
                        "type": "baseline",
                        "reference": "golden.md",
                        "criterion": "is as complete",
                        "name": "matches-the-golden",
                        "weight": 3.0
                    }
                ]
            }
        ]
    }"#;

/// A comparison is the one outcome that reaches its verdict through a second
/// grader, and the weight has to survive that hop. Losing it is silent: an
/// undeclared weight counts as one full vote, so the case still grades and
/// still reports, just against a weighting nobody asked for.
#[test]
fn a_weighted_baseline_grader_keeps_its_weight_through_a_comparison() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, WEIGHTED_BASELINE_SUITE);
    fs::write(
        temp.path().join("demo-skill").join("golden.md"),
        "the reference answer\n",
    )
    .unwrap();

    grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    for (scenario, grading) in grading_files_by_scenario(&report_dir) {
        let result = grading
            .assertion_results
            .iter()
            .find(|result| result.name.as_deref() == Some("matches-the-golden"))
            .unwrap_or_else(|| panic!("{scenario:?} must report the declared grader"));
        assert_eq!(
            result.weight,
            Some(GraderWeight::parse(3.0).unwrap()),
            "{scenario:?} dropped the weight the case declared"
        );
    }
}

const NAMED_AND_UNNAMED_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "contains", "text": "all done", "name": "wraps-up"},
                    {"type": "contains", "text": "professional"}
                ]
            }
        ]
    }"#;

/// A grader the author never named must not pick one up along the way,
/// whether from a sibling grader in the same case or from its own
/// rendered description: an unnamed grader indistinguishable from a
/// named one defeats the point of naming one at all.
#[test]
fn an_unnamed_grader_stays_unnamed_next_to_a_named_grader() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, NAMED_AND_UNNAMED_SUITE);

    grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    for (scenario, grading) in grading_files_by_scenario(&report_dir) {
        let unnamed_result = grading
            .assertion_results
            .iter()
            .find(|result| result.assertion.contains("professional"))
            .unwrap_or_else(|| panic!("{scenario:?} must still report the unnamed grader"));
        assert_eq!(
            unnamed_result.name, None,
            "{scenario:?} unnamed grader must stay unnamed"
        );
    }

    let published: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let flattened_unnamed = published["assertion_results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| {
            result["assertion"]
                .as_str()
                .unwrap_or_default()
                .contains("professional")
        })
        .expect("the unnamed result must survive the flatten");
    assert!(
        flattened_unnamed.get("name").is_none(),
        "an unnamed grader must not gain a name key in the published report.json"
    );

    let flattened_named = published["assertion_results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["assertion"].as_str().unwrap_or_default().contains("all done"))
        .expect("the named result must survive the flatten");
    assert_eq!(flattened_named["name"], "wraps-up");
}

#[test]
fn a_named_grader_result_is_absent_from_the_serialized_json_when_unnamed() {
    let result = AssertionGradeResult {
        name: None,
        assertion: "the output includes a summary".to_string(),
        passed: true,
        evidence: "found the summary section".to_string(),
        grader: GraderInfo {
            kind: GraderKind::Mechanical,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    };

    let json = serde_json::to_string(&result).unwrap();
    assert!(!json.contains("\"name\""));
}

/// The flatten in `update_report_after_grading` used to rebuild the object
/// field by field, which is exactly how `name`, `excluded`, `rationale`
/// and `votes` were previously lost between `grading.json` and the
/// published `report.json`. This pins every one of those fields against
/// the artifact a reader actually opens, not just the per-run file.
#[test]
fn a_named_excluded_multi_voted_result_survives_the_flatten_into_report_json() {
    let temp = tempdir().unwrap();
    let report_dir = both_arms_report_dir(&temp, ARM_SCOPED_SUITE);

    let mut document: ReportDocument =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let runs = document.runs.clone();
    let run = runs.first().expect("both_arms_report_dir produced at least one run");
    let run_dir = report_dir.join(&run.paths.workspace).parent().unwrap().to_path_buf();

    let crafted = GradingFile {
        assertion_results: vec![AssertionGradeResult {
            name: Some("wraps-up".to_string()),
            assertion: "the summary reads well".to_string(),
            passed: true,
            evidence: "the summary names every column".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Llm,
                model: Some("judge-model".to_string()),
                command: None,
            },
            rationale: Some("both judges agreed".to_string()),
            unsupported: None,
            excluded: Some("scoped to the with_skill arm".to_string()),
            ungraded: None,
            votes: Some(JudgeVoteTally { passed: 2, failed: 1 }),
            weight: None,
        }],
        summary: GradingSummary {
            passed: 1,
            failed: 0,
            total: 1,
            unsupported: 0,
            excluded: 1,
            ungraded: 0,
            pass_rate: Some(1.0),
        },
    };
    fs::write(
        run_dir.join("grading.json"),
        serde_json::to_string_pretty(&crafted).unwrap(),
    )
    .unwrap();

    let grading_strategy = build_grading_strategy(&GradeOptions::default(), None);
    update_report_after_grading(&mut document, &runs, &report_dir, &grading_strategy).unwrap();
    std::fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();

    let published: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let flattened = published["assertion_results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result["assertion"] == "the summary reads well")
        .expect("the crafted result must survive the flatten into the published report.json");

    assert_eq!(flattened["name"], "wraps-up");
    assert_eq!(flattened["excluded"], "scoped to the with_skill arm");
    assert_eq!(flattened["rationale"], "both judges agreed");
    assert_eq!(flattened["votes"]["passed"], 2);
    assert_eq!(flattened["votes"]["failed"], 1);
}

fn weighted_result(passed: bool, unsupported: bool, weight: Option<GraderWeight>) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: "the output includes a summary".to_string(),
        passed,
        evidence: "found the summary section".to_string(),
        grader: GraderInfo {
            kind: GraderKind::Mechanical,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: unsupported.then(|| "runner cannot observe this".to_string()),
        excluded: None,
        ungraded: None,
        votes: None,
        weight,
    }
}

/// A grader that leaves weight undeclared inside an otherwise-weighted case
/// still has to count for exactly as much as it always did: one full vote,
/// same as a declared weight of 1.0 would.
#[test]
fn an_undeclared_weight_counts_as_one_full_vote_alongside_a_declared_weight() {
    let results = vec![
        weighted_result(true, false, Some(GraderWeight::parse(3.0).unwrap())),
        weighted_result(false, false, None),
    ];

    assert_eq!(case_score(&results), Some(0.75));
}

/// A case where every grader left weight undeclared has to take the exact
/// pre-weighting code path, not merely a formula that happens to agree with
/// it, so a suite that never opts in cannot see its score move.
#[test]
fn case_score_falls_back_to_the_pre_weighting_pass_rate_when_nothing_declares_a_weight() {
    let results = vec![
        weighted_result(true, false, None),
        weighted_result(true, false, None),
        weighted_result(true, false, None),
        weighted_result(false, false, None),
        weighted_result(true, true, None),
    ];

    assert_eq!(case_score(&results), Some(0.75));
    assert_eq!(case_score(&results), GradingCounts::tally(&results).pass_rate());
}

/// An undeclared weight must not be constructible from a build that never
/// wrote one, so the field cannot appear where nothing asked for it.
#[test]
fn an_unweighted_result_is_absent_from_the_serialized_json() {
    let result = weighted_result(true, false, None);
    let json = serde_json::to_string(&result).unwrap();
    assert!(!json.contains("\"weight\""));
}

fn suite_from(json: &str) -> EvalSuite {
    crate::agentskills::evals::parse_eval_suite(json).unwrap()
}

#[test]
fn the_recorded_grading_strategy_tells_a_panel_from_a_single_opinion() {
    let llm_options = GradeOptions {
        grader: GraderMode::Llm,
        ..GradeOptions::default()
    };
    let model = JudgeModel::new("judge-model").unwrap();
    let single = build_grading_strategy(&llm_options, Some(&model));
    let panel = build_grading_strategy(
        &GradeOptions {
            grader_votes: JudgeVotes::parse(3).unwrap(),
            ..llm_options
        },
        Some(&model),
    );

    assert_eq!(
        single.grader,
        GraderChoice::Llm {
            judge: JudgeSettings {
                provider: JudgeProvider::default(),
                model: Some("judge-model".to_string()),
                votes: JudgeVotes::single(),
            },
        }
    );
    assert_eq!(
        panel.grader,
        GraderChoice::Llm {
            judge: JudgeSettings {
                provider: JudgeProvider::default(),
                model: Some("judge-model".to_string()),
                votes: JudgeVotes::parse(3).unwrap(),
            },
        }
    );
}

/// A record that named the flag rather than the resolved model would say nothing was
/// judged by on a pass whose model came from the environment, while a judge answered
/// every assertion in it.
#[test]
fn the_recorded_strategy_names_the_model_that_answered_rather_than_the_flag() {
    let options = GradeOptions {
        grader: GraderMode::Llm,
        grader_model: None,
        ..GradeOptions::default()
    };
    let resolved = JudgeModel::new("model-from-the-environment").unwrap();

    let strategy = build_grading_strategy(&options, Some(&resolved));

    assert_eq!(
        strategy.grader,
        GraderChoice::Llm {
            judge: JudgeSettings {
                provider: JudgeProvider::default(),
                model: Some("model-from-the-environment".to_string()),
                votes: JudgeVotes::single(),
            },
        }
    );
}

/// An `auto` pass that never reached a judge must record `Auto` with no judge, not one
/// whose settings merely go unused: an absent judge and an unused-but-present judge are
/// different claims about what the pass did.
#[test]
fn an_auto_pass_that_never_reaches_a_judge_records_no_judge() {
    let options = GradeOptions {
        grader: GraderMode::Auto,
        ..GradeOptions::default()
    };

    let strategy = build_grading_strategy(&options, None);

    assert_eq!(strategy.grader, GraderChoice::Auto { judge: None });
}

/// The bug this guards: two `auto` passes over the same bundle, each resolving a
/// different judge model, must be recorded as two distinct strategies rather than
/// collapsing to one under `Auto`'s old judge-blind equality. This is the same
/// push-if-absent dedup `update_report_after_grading` runs against
/// `dimensions.grading_strategies`, exercised directly against a `Vec` so the test
/// does not have to stand up a full graded bundle to reach it.
#[test]
fn two_auto_passes_with_different_judge_models_are_recorded_as_two_strategies() {
    let mut grading_strategies: Vec<GradingStrategy> = Vec::new();

    let first_options = GradeOptions {
        grader: GraderMode::Auto,
        grader_model: Some("model-a".to_string()),
        ..GradeOptions::default()
    };
    let second_options = GradeOptions {
        grader_model: Some("model-b".to_string()),
        ..first_options.clone()
    };

    let model_a = JudgeModel::new("model-a").unwrap();
    let model_b = JudgeModel::new("model-b").unwrap();
    let first_strategy = build_grading_strategy(&first_options, Some(&model_a));
    let second_strategy = build_grading_strategy(&second_options, Some(&model_b));

    for strategy in [&first_strategy, &second_strategy] {
        if !grading_strategies.contains(strategy) {
            grading_strategies.push(strategy.clone());
        }
    }

    assert_eq!(grading_strategies.len(), 2);
    assert_ne!(first_strategy, second_strategy);
}

#[test]
fn a_suite_of_typed_graders_needs_no_judge_under_auto() {
    let suite = suite_from(UNOBSERVABLE_SUITE);
    let options = GradeOptions {
        grader: GraderMode::Auto,
        ..GradeOptions::default()
    };

    assert!(!suite_needs_a_judge(&options, &suite));
    assert!(GradeSession::open(&options, &suite).unwrap().judge.is_none());
}

#[test]
fn a_declared_llm_grader_needs_a_judge_under_auto() {
    let suite = suite_from(
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{"type": "llm", "criterion": "the summary reads well"}]
                    }
                ]
            }"#,
    );
    let options = GradeOptions {
        grader: GraderMode::Auto,
        ..GradeOptions::default()
    };

    assert!(suite_needs_a_judge(&options, &suite));
    let err = GradeSession::open(&options, &suite).unwrap_err();
    assert!(err.to_string().contains("--grader-model"), "{err}");
}

#[test]
fn a_declared_baseline_grader_needs_a_judge_under_auto() {
    let suite = suite_from(
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{"type": "baseline", "reference": "golden.md", "criterion": "is as complete"}]
                    }
                ]
            }"#,
    );
    let options = GradeOptions {
        grader: GraderMode::Auto,
        ..GradeOptions::default()
    };

    assert!(
        suite_needs_a_judge(&options, &suite),
        "a comparison is a judgement call, so the credential is demanded before the first run is graded"
    );
    let err = GradeSession::open(&options, &suite).unwrap_err();
    assert!(err.to_string().contains("--grader-model"), "{err}");
}

#[test]
fn a_baseline_comparison_is_put_to_the_judge_blind_and_the_same_way_every_time() {
    let pairing = BaselinePairing::for_criterion("case-a", "is as complete");
    let outputs = pairing.outputs("what the run wrote", "the reference", TruncateDirection::Head);

    assert_eq!(outputs.len(), 2, "the judge is shown both sides and nothing else");
    assert_eq!(
        outputs.keys().copied().collect::<Vec<_>>(),
        vec!["A", "B"],
        "the labels carry no hint of which side is which"
    );
    assert_eq!(outputs[pairing.run.as_str()], "what the run wrote");
    assert_eq!(outputs[pairing.reference().as_str()], "the reference");
    assert_ne!(pairing.run, pairing.reference());

    assert_eq!(
        pairing,
        BaselinePairing::for_criterion("case-a", "is as complete"),
        "re-grading a report must ask the judge the question it asked before, not its mirror"
    );
}

#[test]
fn which_side_a_baseline_puts_first_is_decided_by_the_case_and_the_criterion() {
    let pairings: Vec<BlindLabel> = [
        "is as complete",
        "is as well organized",
        "is as precise",
        "reads as well",
    ]
    .into_iter()
    .map(|criterion| BaselinePairing::for_criterion("case-a", criterion).run)
    .collect();

    assert!(
        pairings.contains(&BlindLabel::A) && pairings.contains(&BlindLabel::B),
        "a fixed position would leave every comparison carrying the judge's position bias, got {pairings:?}"
    );
}

#[test]
fn a_run_the_judge_cannot_separate_from_its_baseline_has_met_it() {
    for run in [BlindLabel::A, BlindLabel::B] {
        let pairing = BaselinePairing { run };
        let reference = pairing.reference();

        assert!(
            pairing.at_least_as_good(ComparisonWinner::Tie),
            "at least as good is met by a tie, or no baseline could ever be held"
        );
        assert!(pairing.at_least_as_good(winner_of(run)));
        assert!(!pairing.at_least_as_good(winner_of(reference)));
    }
}

fn winner_of(label: BlindLabel) -> ComparisonWinner {
    match label {
        BlindLabel::A => ComparisonWinner::A,
        BlindLabel::B => ComparisonWinner::B,
    }
}

#[test]
fn both_sides_of_a_baseline_comparison_are_clipped_to_the_same_size() {
    let pairing = BaselinePairing::for_criterion("case-a", "is as complete");
    let run = "r".repeat(BaselinePairing::SIDE_BYTES * 2);
    let reference = "f".repeat(BaselinePairing::SIDE_BYTES * 4);

    let outputs = pairing.outputs(&run, &reference, TruncateDirection::Head);

    let shown = |label: BlindLabel| outputs[label.as_str()].len();
    assert_eq!(
        shown(pairing.run),
        shown(pairing.reference()),
        "reading one side whole and the other clipped compares two different things"
    );
    assert!(shown(pairing.run) < run.len());
}

#[test]
fn a_short_baseline_reference_is_never_clipped_to_make_room_for_a_long_run() {
    let pairing = BaselinePairing::for_criterion("case-a", "is as complete");
    let reference = "the whole reference answer";

    let outputs = pairing.outputs(
        &"r".repeat(BaselinePairing::SIDE_BYTES * 3),
        reference,
        TruncateDirection::Head,
    );

    assert_eq!(outputs[pairing.reference().as_str()], reference);
}

fn run_record_with_artifacts(artifacts: Vec<serde_json::Value>) -> RunRecord {
    RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: "run-001".to_string(),
        eval_case_id: "case-a".to_string(),
        eval_slug: "case-a".to_string(),
        split: EvalSplit::Train,
        scenario_id: ScenarioKind::WithSkill,
        iteration: 1,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 1,
        status: "completed".to_string(),
        paths: RunPaths {
            workspace: "runs/run-001/workspace".to_string(),
            outputs: "runs/run-001/workspace/outputs".to_string(),
        },
        mirror_path: "iteration-1/eval-case-a/with-skill/".to_string(),
        tool_grant: None,
        artifacts,
        metrics: RunMetrics {
            duration_ms: Some(1),
            exit_code: Some(0),
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        trace: None,
        grade_trace: None,
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score: None,
    }
}

fn run_under(model: Option<&str>) -> RunRecord {
    let mut run = run_record_with_artifacts(vec![]);
    run.runner_model = model.map(|name| crate::agentskills::model_name::ModelName::parse(name).unwrap());
    run
}

#[test]
fn a_judge_from_the_family_that_ran_the_runs_is_warned_about_but_not_refused() {
    let judge = JudgeModel::new("claude-opus-4-1").unwrap();
    let runs = [run_under(Some("sonnet")), run_under(Some("gpt-5")), run_under(None)];

    let warning = JudgeIndependenceWarning::assess(&judge, Some(Runner::ClaudeCode), &runs).unwrap();

    assert_eq!(warning.family, ModelFamily::Claude);
    assert_eq!(
        warning.runner_models,
        vec!["claude-code default".to_string(), "sonnet".to_string()]
    );
    assert!(warning.describe().starts_with("WARN: "), "{}", warning.describe());
}

#[test]
fn a_judge_from_another_family_than_every_run_raises_no_warning() {
    let judge = JudgeModel::new("gpt-5").unwrap();
    let runs = [run_under(Some("claude-sonnet-4-5")), run_under(None)];

    assert!(JudgeIndependenceWarning::assess(&judge, Some(Runner::ClaudeCode), &runs).is_none());
    assert!(
        JudgeIndependenceWarning::assess(&JudgeModel::new("my-finetune").unwrap(), Some(Runner::Codex), &runs)
            .is_none(),
        "a judge whose family is unknown cannot be said to share one"
    );
}

#[test]
fn created_files_from_artifacts_strips_a_mismatched_outputs_prefix() {
    let mut run = run_record_with_artifacts(vec![serde_json::json!({
        "kind": "output",
        "path": "runs/run-001/workspace/outputs/report.md",
        "size_bytes": 4,
        "sha256": "abc",
    })]);
    run.paths.outputs = "runs/run-001/workspace/outputs/".to_string();

    let created = created_files_from_artifacts(&run);

    assert_eq!(
        created,
        vec!["report.md".to_string()],
        "a path-aware strip must not be defeated by a trailing separator on the outputs root"
    );
}

#[test]
fn created_files_from_artifacts_strips_the_outputs_prefix_and_ignores_other_kinds() {
    let run = run_record_with_artifacts(vec![
        serde_json::json!({
            "kind": "output",
            "path": "runs/run-001/workspace/outputs/report.md",
            "size_bytes": 4,
            "sha256": "abc",
        }),
        serde_json::json!({
            "kind": "output",
            "path": "runs/run-001/workspace/outputs/sub/data.json",
            "size_bytes": 2,
            "sha256": "def",
        }),
        serde_json::json!({
            "kind": "transcript",
            "path": "runs/run-001/transcript.jsonl",
        }),
    ]);

    let created = created_files_from_artifacts(&run);

    assert_eq!(created, vec!["report.md".to_string(), "sub/data.json".to_string()]);
}

const BRIEF: CaseBrief<'static> = CaseBrief {
    prompt: "the question",
    expected_output: "the reference",
};

/// A brief that claims none of the request, for the tests that pin down how the run's
/// own artifacts share the whole budget.
const NO_BRIEF: CaseBrief<'static> = CaseBrief {
    prompt: "",
    expected_output: "",
};

fn declarative_context(final_text: &str, raw_transcript: &str) -> DeclarativeContext {
    DeclarativeContext {
        final_text: final_text.to_string(),
        raw_transcript: raw_transcript.to_string(),
        transcript: None,
    }
}

#[test]
fn the_default_target_reproduces_the_pre_target_payload_shape() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    fs::write(ctx.outputs_dir.join("out.txt"), "an output file").unwrap();
    let declarative = declarative_context("the final answer", "irrelevant to this target");

    let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "assertion": "the assertion",
            "prompt": "the question",
            "expected_output": "the reference",
            "final_text": "the final answer",
            "outputs": {"out.txt": "an output file"},
        })
    );
}

#[test]
fn any_output_target_uses_the_same_structured_shape_as_the_default() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    fs::write(ctx.outputs_dir.join("out.txt"), "an output file").unwrap();
    let declarative = declarative_context("the final answer", "irrelevant to this target");

    let payload = llm_grader_payload("the assertion", &GradeTarget::AnyOutput, &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "assertion": "the assertion",
            "prompt": "the question",
            "expected_output": "the reference",
            "final_text": "the final answer",
            "outputs": {"out.txt": "an output file"},
        })
    );
}

#[test]
fn any_output_walks_nested_output_files_but_the_default_final_text_target_does_not() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    fs::write(ctx.outputs_dir.join("out.txt"), "top level").unwrap();
    fs::create_dir_all(ctx.outputs_dir.join("sub")).unwrap();
    fs::write(ctx.outputs_dir.join("sub").join("nested.txt"), "buried").unwrap();
    let declarative = declarative_context("the final answer", "irrelevant to this target");

    let any_output = llm_grader_payload("the assertion", &GradeTarget::AnyOutput, &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let any_output: serde_json::Value = serde_json::from_str(&any_output).unwrap();
    assert_eq!(
        any_output["outputs"],
        serde_json::json!({"out.txt": "top level", "sub/nested.txt": "buried"}),
        "any_output must see everything the mechanical grader's collect_text would see"
    );

    let final_text = llm_grader_payload("the assertion", &GradeTarget::FinalText, &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let final_text: serde_json::Value = serde_json::from_str(&final_text).unwrap();
    assert_eq!(
        final_text["outputs"],
        serde_json::json!({"out.txt": "top level"}),
        "final_text keeps the pre-target, top-level-only scope"
    );
}

#[test]
fn transcript_target_names_itself_and_carries_the_transcript() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let declarative = declarative_context("the final answer", "the agent read the config then wrote it back");

    let payload = llm_grader_payload("the assertion", &GradeTarget::Transcript, &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "assertion": "the assertion",
            "prompt": "the question",
            "expected_output": "the reference",
            "target": "transcript",
            "content": "the agent read the config then wrote it back",
        })
    );
}

#[test]
fn file_target_reports_missing_reason_when_the_file_does_not_exist() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let declarative = declarative_context("the final answer", "");

    let path: RelativeSkillPath = serde_json::from_value(serde_json::json!("missing.json")).unwrap();
    let target = GradeTarget::File(path);
    let payload = llm_grader_payload("the assertion", &target, &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    assert_eq!(value["assertion"], "the assertion");
    assert_eq!(value["target"], "file 'missing.json'");
    assert!(value.get("content").is_none());
    assert!(value["missing_reason"].as_str().unwrap().contains("missing.json"));
}

#[test]
fn a_file_target_pointing_at_a_picture_attaches_it_instead_of_failing_to_read_it_as_text() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    fs::write(
        ctx.outputs_dir.join("screenshot.png"),
        [0x89, 0x50, 0x4E, 0x47, 1, 2, 3],
    )
    .unwrap();
    let declarative = declarative_context("the final answer", "");

    let path: RelativeSkillPath = serde_json::from_value(serde_json::json!("screenshot.png")).unwrap();
    let target = GradeTarget::File(path);
    let payload = llm_grader_payload("the assertion", &target, &BRIEF, &declarative, &ctx).unwrap();

    assert!(
        payload.image.is_some(),
        "a picture target must carry an image attachment for the judge to see"
    );
    let value: serde_json::Value = serde_json::from_str(&payload.text).unwrap();
    assert!(
        value.get("missing_reason").is_none(),
        "an image target must not be reported as unreadable: {value}"
    );
}

#[test]
fn created_files_target_lists_what_the_run_produced() {
    let tmp = tempdir().unwrap();
    let mut ctx = ctx_with_outputs(tmp.path());
    ctx.created_files = vec!["report.md".to_string(), "sub/data.json".to_string()];
    let declarative = declarative_context("the final answer", "");

    let payload = llm_grader_payload("the assertion", &GradeTarget::CreatedFiles, &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    assert_eq!(
        value,
        serde_json::json!({
            "assertion": "the assertion",
            "prompt": "the question",
            "expected_output": "the reference",
            "target": "created files",
            "content": "report.md\nsub/data.json",
        })
    );
}

#[test]
fn payload_budget_divides_across_every_artifact_and_flags_what_it_drops() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    // Far larger than the whole request budget, so this artifact alone
    // must be truncated no matter how the remaining budget is split.
    let oversized = "a".repeat(PayloadBudget::TOTAL_BYTES * 5);
    fs::write(ctx.outputs_dir.join("a.txt"), &oversized).unwrap();
    fs::write(ctx.outputs_dir.join("b.txt"), "b").unwrap();
    let declarative = declarative_context("c", "");

    let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    let a = value["outputs"]["a.txt"].as_str().unwrap();
    assert!(
        a.len() < oversized.len(),
        "an oversized artifact must be truncated, got {} bytes back unchanged",
        a.len()
    );
    assert!(a.contains("[truncated after"), "truncation must stay visible: {a}");
    // What b.txt received is whatever a.txt's truncation left unused,
    // rolled forward, so its exact size depends on that share; only its
    // content need survive untouched, since one byte never needs truncating.
    assert_eq!(value["outputs"]["b.txt"], "b");
    assert_eq!(value["final_text"], "c");
    assert!(value.get("artifacts_omitted").is_none());
}

#[test]
fn an_artifact_needing_less_than_its_share_leaves_the_rest_for_the_one_after_it() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    // With 3 artifacts, final_text's initial share is TOTAL_BYTES / 3, but
    // final_text ("c") needs almost none of it. a.txt is bigger than that
    // fixed share, so it is only placed whole if the unused share rolled
    // forward into a.txt's own share instead of being wasted.
    let fixed_share = PayloadBudget::TOTAL_BYTES / 3;
    let a_len = fixed_share + 500;
    fs::write(ctx.outputs_dir.join("a.txt"), "a".repeat(a_len)).unwrap();
    fs::write(ctx.outputs_dir.join("b.txt"), "b").unwrap();
    let declarative = declarative_context("c", "");

    let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    let a = value["outputs"]["a.txt"].as_str().unwrap();
    assert_eq!(
        a.len(),
        a_len,
        "a.txt fits within the rolled-forward share and must not be truncated: {a}"
    );
    assert!(!a.contains("[truncated after"));
}

#[test]
fn payload_budget_answers_a_placement_it_was_never_told_to_expect() {
    let mut budget = PayloadBudget::after(&NO_BRIEF, 1);

    assert!(matches!(
        budget.place("first", TruncateDirection::Head),
        Placement::Whole(_)
    ));
    assert!(matches!(
        budget.place("second", TruncateDirection::Head),
        Placement::Whole(_)
    ));
}

#[test]
fn payload_budget_omits_rather_than_silently_dropping_when_slots_outnumber_bytes() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    for i in 0..(PayloadBudget::TOTAL_BYTES + 10) {
        fs::write(ctx.outputs_dir.join(format!("file-{i}.txt")), "x").unwrap();
    }
    let declarative = declarative_context("final", "");

    let payload = llm_grader_payload("the assertion", &GradeTarget::default(), &NO_BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    let note = value["artifacts_omitted"]
        .as_str()
        .expect("omissions must be reported, not silent");
    assert!(note.contains("omitted"));
    // The budget's per-slot share is a non-decreasing ratio, so every omission
    // sits in a prefix of the placement order: final_text plus the first ten
    // output files (the ones seen while remaining_slots still exceeds 8000)
    // before the ratio recovers to one byte per slot for the rest. A count
    // that drifts from 11 means an artifact was silently dropped somewhere
    // in the loop without being tallied.
    assert!(
        note.contains("11 artifact"),
        "expected exactly 11 omissions (final_text plus the first ten output files): {note}"
    );
}

#[test]
fn transcript_truncation_keeps_the_tail_not_the_head() {
    let head = "A".repeat(50);
    let tail = "B".repeat(50);
    let long_transcript = format!("{head}{tail}");

    let truncated = truncate(&long_transcript, 50, TruncateDirection::Tail);

    assert!(!truncated.contains('A'), "the head must be dropped: {truncated}");
    assert!(truncated.contains(&tail));
}

#[test]
fn transcript_truncation_keeps_the_first_and_last_message_and_names_what_it_drops() {
    let first = "FIRST_MESSAGE".to_string();
    let middle = "MIDDLE_MESSAGE_".repeat(400);
    let last = "LAST_MESSAGE".to_string();
    let long_transcript = format!("{first}\n{middle}\n{last}");

    let truncated = truncate(&long_transcript, 100, TruncateDirection::Tail);

    assert!(
        truncated.contains(&first),
        "the first message must survive so the judge sees how the run began: {truncated}"
    );
    assert!(
        truncated.contains(&last),
        "the last message must survive so the judge sees how the run ended: {truncated}"
    );
    assert!(
        !truncated.contains(&middle),
        "the dropped middle message must not appear: {truncated}"
    );
    assert!(
        truncated.contains("omitted"),
        "the elision must be visible rather than silent: {truncated}"
    );
}

/// A transcript is written a line at a time, so it ends with a newline that the
/// join never writes back. Counted against the budget anyway, a transcript that is
/// over the cap by that byte and nothing else pays for it with a whole message.
#[test]
fn a_transcript_over_the_cap_by_only_its_trailing_newline_keeps_every_message() {
    let messages = ["FIRST_MESSAGE", "MIDDLE_MESSAGE", "LAST_MESSAGE"];
    let joined = messages.join("\n");
    let transcript = format!("{joined}\n");
    let cap = joined.len();
    assert!(transcript.len() > cap, "the transcript is over the cap by its newline");

    let truncated = truncate(&transcript, cap, TruncateDirection::Tail);

    assert_eq!(
        truncated, joined,
        "nothing was over budget, so nothing may be dropped or annotated"
    );
}

/// What is left over is whatever the tail did not spend, so an overstated tail
/// understates it. A first message that fits the room actually left is dropped and
/// reported as omitted.
#[test]
fn a_first_message_that_fits_the_leftover_budget_exactly_is_kept() {
    let first = "FIRST";
    let last = "LAST";
    let transcript = format!("{first}\n{}\n{last}", "M".repeat(100));
    let cap = first.len() + 1 + last.len();

    let truncated = truncate(&transcript, cap, TruncateDirection::Tail);

    assert!(
        truncated.contains(first),
        "the first message fits the room left, so the judge must still see how the run began: {truncated}"
    );
    assert!(truncated.contains(last), "the tail must survive: {truncated}");
    assert!(
        !truncated.contains("from the start"),
        "the first message was kept, so nothing was omitted from the start: {truncated}"
    );
}

/// The other side of the same boundary: the newline the first message is joined by
/// is a real byte, so a first message that would fit only without it does not fit,
/// and keeping it would spend more of the judge's budget than the cap allows.
#[test]
fn a_first_message_one_byte_past_the_leftover_budget_is_not_kept() {
    let first = "FIRSTX";
    let last = "LAST";
    let transcript = format!("{first}\n{}\n{last}", "M".repeat(100));
    let cap = first.len() + last.len();

    let truncated = truncate(&transcript, cap, TruncateDirection::Tail);

    assert!(
        !truncated.contains(first),
        "the first message is one byte past what is left, so it cannot be kept: {truncated}"
    );
    assert!(
        truncated.contains("from the start"),
        "dropping the opening of the run must be said out loud: {truncated}"
    );
}

#[test]
fn an_oversized_transcript_reaches_the_judge_tail_truncated_via_the_full_payload() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let head = "A".repeat(50);
    let tail = "B".repeat(PayloadBudget::TOTAL_BYTES + 100);
    let declarative = declarative_context("final", &format!("{head}{tail}"));

    let payload = llm_grader_payload("the assertion", &GradeTarget::Transcript, &BRIEF, &declarative, &ctx)
        .unwrap()
        .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
    let content = value["content"].as_str().unwrap();

    assert!(
        !content.contains('A'),
        "the head must be dropped through the full payload path: {content}"
    );
}

#[test]
fn final_text_truncation_keeps_the_head_not_the_tail() {
    let head = "A".repeat(50);
    let tail = "B".repeat(50);
    let long_text = format!("{head}{tail}");

    let truncated = truncate(&long_text, 50, TruncateDirection::Head);

    assert!(!truncated.contains('B'), "the tail must be dropped: {truncated}");
    assert!(truncated.contains(&head));
}

#[test]
fn script_mode_never_resolves_a_judge() {
    let suite = suite_from(
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{"type": "llm", "criterion": "the summary reads well"}]
                    }
                ]
            }"#,
    );
    let options = GradeOptions {
        grader: GraderMode::Script,
        grader_command: Some("./grade.sh".to_string()),
        ..GradeOptions::default()
    };

    assert!(!suite_needs_a_judge(&options, &suite));
}

/// A declared `llm` grader's whole contract is deferring to the configured judge; it
/// is the one grader kind that does. Whether its criterion happens to read like a
/// mechanical pattern is not the author's declaration, so it must not change which
/// grader answers it, defaulted target or explicit alike.
#[test]
fn a_declared_llm_grader_reaches_the_judge_even_when_its_criterion_reads_like_a_mechanical_check() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let declarative = declarative_context("done", "the agent reported that report.md exists");
    let options = GradeOptions::default();
    let session = GradeSession {
        options: &options,
        judge: None,
    };

    let mechanical_sounding = "report.md exists";

    for target in [
        TargetDeclaration::Default,
        TargetDeclaration::Named(GradeTarget::Transcript),
    ] {
        let result = grade_with_llm(mechanical_sounding, &target, &BRIEF, &declarative, &ctx, &session);
        let err = result.expect_err(
                "a declared llm grader must reach the judge regardless of target, even when its criterion reads like a mechanical pattern",
            );
        assert!(
            err.to_string().contains("no LLM judge was resolved"),
            "expected the judge lookup to fail, got: {err}"
        );
    }
}

#[test]
fn pass_rate_ignores_unsupported_results_so_an_unobservable_runner_cannot_lower_it() {
    let counts = GradingCounts::tally(&[
        AssertionGradeResult {
            name: None,
            assertion: "contains 'all done'".to_string(),
            passed: true,
            evidence: "matched".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Declarative,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
            weight: None,
        },
        AssertionGradeResult {
            name: None,
            assertion: "the skill was engaged".to_string(),
            passed: false,
            evidence: String::new(),
            grader: GraderInfo {
                kind: GraderKind::Declarative,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: Some("runner 'codex' does not expose tool calls".to_string()),
            excluded: None,
            ungraded: None,
            votes: None,
            weight: None,
        },
    ]);

    assert_eq!(counts.total, 2);
    assert_eq!(counts.unsupported, 1);
    assert_eq!(counts.passed, 1);
    assert_eq!(counts.failed, 0);
    assert_eq!(counts.scored(), 1);
    assert_eq!(counts.pass_rate(), Some(1.0));
}

#[test]
fn a_summary_with_nothing_scored_reports_no_pass_rate_rather_than_zero() {
    let counts = GradingCounts::tally(&[AssertionGradeResult {
        name: None,
        assertion: "the skill was engaged".to_string(),
        passed: false,
        evidence: "runner 'codex' does not expose tool calls".to_string(),
        grader: GraderInfo {
            kind: GraderKind::Declarative,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: Some("runner 'codex' does not expose tool calls".to_string()),
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    }]);

    assert_eq!(counts.scored(), 0);
    assert_eq!(counts.pass_rate(), None);

    let summary = counts.summary();
    assert_eq!(summary.pass_rate, None);
    assert_eq!(
        serde_json::to_value(&summary).unwrap()["pass_rate"],
        serde_json::Value::Null
    );
}

#[test]
fn a_grade_tally_names_the_runs_that_never_completed() {
    let mut statuses = GradedRunStatuses::default();
    for status in ["completed", "failed", "timeout", "skipped", "exploded"] {
        statuses.record(status);
    }

    assert_eq!(statuses.completed, 1);
    assert_eq!(statuses.not_completed(), 4);
    assert_eq!(
        statuses.describe_not_completed().as_deref(),
        Some("1 failed, 1 timed out, 1 skipped, 1 of an unrecognized status")
    );

    let clean = GradedRunStatuses {
        completed: 3,
        ..GradedRunStatuses::default()
    };
    assert_eq!(clean.not_completed(), 0);
    assert_eq!(clean.describe_not_completed(), None);
}

fn result_for_test(assertion: &str, passed: bool) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: assertion.to_string(),
        passed,
        evidence: "e".to_string(),
        grader: GraderInfo {
            kind: GraderKind::Mechanical,
            model: None,
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    }
}

#[test]
fn an_assertion_nothing_could_grade_is_not_counted_as_a_failure() {
    let results = vec![
        result_for_test("a", true),
        needs_llm_result("b", "grader mode is none, so no judge was consulted"),
    ];

    let counts = GradingCounts::tally(&results);

    assert_eq!(counts.ungraded, 1);
    assert_eq!(
        counts.failed, 0,
        "an assertion no grader could attempt is not evidence the skill failed it"
    );
    assert_eq!(counts.passed, 1);
    assert_eq!(counts.scored(), 1);

    let summary = counts.summary();
    assert_eq!(summary.ungraded, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        summary.pass_rate,
        Some(1.0),
        "pass_rate is over the scored assertions, so an ungraded one cannot dilute it"
    );
}

/// An arm-scoped check that nothing could attempt carries the excluded and the
/// ungraded marker at once, so every count taken over those markers has to agree
/// on which one wins. Excluded wins, because a check nobody was going to score
/// loses nothing by also having been unanswerable. The list and the number are
/// asserted together: letting them drift is how a report names assertions that
/// the figure printed beside them does not count.
#[test]
fn an_excluded_assertion_that_nothing_attempted_is_counted_and_listed_the_same_way() {
    let mut unattempted_and_out_of_scope = needs_llm_result("b", "no judge was consulted");
    unattempted_and_out_of_scope.excluded = Some("scored in the with-skill arm only".to_string());
    let results = vec![
        result_for_test("a", true),
        unattempted_and_out_of_scope,
        needs_llm_result("c", "no judge was consulted"),
    ];

    assert!(
        results[1].is_ungraded(),
        "the ungraded marker is still there for a second predicate to misread"
    );
    assert_eq!(results[1].outcome(), AssertionOutcome::Excluded);

    let counts = GradingCounts::tally(&results);
    assert_eq!(counts.excluded, 1);
    assert_eq!(counts.ungraded, 1, "only the check that was in scope went unmeasured");
    assert_eq!(counts.failed, 0);

    let listed = ungraded_assertion_texts(&results);
    assert_eq!(
        listed.len(),
        counts.ungraded,
        "the assertions listed as ungraded must be the ones the count counted"
    );
    assert_eq!(listed, vec!["c".to_string()]);
}

#[test]
fn every_assertion_going_ungraded_leaves_no_pass_rate_at_all() {
    let results = vec![needs_llm_result("a", "no judge was consulted")];

    let counts = GradingCounts::tally(&results);

    assert_eq!(counts.ungraded, 1);
    assert_eq!(counts.failed, 0);
    assert_eq!(counts.scored(), 0);
    assert_eq!(
        counts.pass_rate(),
        None,
        "nothing answered is not the same result as everything failed"
    );
}

/// A bundle that fails to parse as the current report schema (here, a `model_configs`
/// entry with a `capture_status` value no variant, not even a retired one accepted for
/// compatibility, recognizes) has the failure name the `report.json` it came from, the
/// same way a read failure a few lines above this parse already does: an operator who
/// ran `grade` against the wrong bundle, or scripted it over several, cannot otherwise
/// tell which report.json is at fault from a bare serde error.
#[test]
fn an_unreadable_bundle_names_its_own_report_json_in_the_error() {
    let temp = tempdir().unwrap();
    let (report_dir, _run_dir) = unobservable_report_dir(&temp);

    let report_path = report_dir.join("report.json");
    let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    document["dimensions"]["model_configs"][0]["capture_status"] = serde_json::json!("unknown_status");
    fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let result = grade_report_bundle(&report_dir, GradeOptions::default());

    let error = result.expect_err("a report.json with an unrecognized capture_status must not parse");
    let message = error.to_string();
    assert!(
        message.contains(&report_path.display().to_string()),
        "expected the error to name '{}', got: {message}",
        report_path.display()
    );
}

/// `capture_status: "partial"` is a retired spelling of `incomplete` that older builds
/// wrote; a bundle carrying it has to grade cleanly rather than being reported as
/// unreadable.
#[test]
fn a_bundle_with_the_retired_partial_capture_status_still_grades() {
    let temp = tempdir().unwrap();
    let (report_dir, _run_dir) = unobservable_report_dir(&temp);

    let report_path = report_dir.join("report.json");
    let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    document["dimensions"]["model_configs"][0]["capture_status"] = serde_json::json!("partial");
    fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let result = grade_report_bundle(&report_dir, GradeOptions::default());

    assert!(
        result.is_ok(),
        "a report.json using the retired 'partial' capture_status must still grade: {:?}",
        result.err()
    );
}

#[test]
fn a_run_with_no_mcp_servers_control_is_skipped_rather_than_graded() {
    let temp = tempdir().unwrap();
    let (report_dir, run_dir) = unobservable_report_dir(&temp);

    let report_path = report_dir.join("report.json");
    let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    document["runs"][0]["status"] = serde_json::json!("skipped");
    document["runs"][0]["failure_kind"] = serde_json::json!(crate::agentskills::runner::FAILURE_KIND_UNSUPPORTED);
    fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.runs_graded, 0);
    assert_eq!(report.assertions_graded, 0);
    assert_eq!(report.passed, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(report.unsupported, 0);
    assert_eq!(report.run_statuses.skipped, 1);
    assert!(
        !run_dir.join("grading.json").is_file(),
        "a run that never started has nothing to write grading.json from"
    );
}

#[test]
fn a_logged_mock_violation_becomes_a_failing_assertion() {
    let temp = tempdir().unwrap();
    let (report_dir, run_dir) = unobservable_report_dir(&temp);

    let report_path = report_dir.join("report.json");
    let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    document["runs"][0]["mock_violations"] = serde_json::json!([{
        "server": "github",
        "tool": "create_issue",
        "path": "title",
        "constraint": "matches /^feat/",
        "received": "bug: oops",
    }]);
    fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        report.assertions_graded, 3,
        "the two declared graders plus the logged violation"
    );
    assert_eq!(
        report.failed, 1,
        "the violation never passes, whatever the declared graders did"
    );

    let grading: GradingFile =
        serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
    let violation_result = grading
        .assertion_results
        .iter()
        .find(|result| result.assertion.contains("github/create_issue"))
        .expect("the violation surfaces as an ordinary assertion result");
    assert!(!violation_result.passed);
    assert!(violation_result.evidence.contains("bug: oops"));
}

#[test]
fn grading_a_run_from_an_unobservable_runner_reports_unsupported_instead_of_failed() {
    let temp = tempdir().unwrap();
    let (report_dir, run_dir) = unobservable_report_dir(&temp);

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            grader_provider: JudgeProvider::default(),
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.assertions_graded, 2);
    assert_eq!(report.unsupported, 1);
    assert_eq!(report.excluded, 0);
    assert_eq!(report.passed, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(report.ungraded, 0);
    assert_eq!(report.run_statuses.skipped, 1);
    assert_eq!(report.run_statuses.completed, 0);
    assert_eq!(
        report.run_statuses.describe_not_completed().as_deref(),
        Some("1 skipped")
    );

    let grading: GradingFile =
        serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
    assert_eq!(grading.summary.total, 2);
    assert_eq!(grading.summary.unsupported, 1);
    assert_eq!(grading.summary.passed, 1);
    assert_eq!(grading.summary.failed, 0);
    assert_eq!(grading.summary.pass_rate, Some(1.0));

    let unsupported = grading
        .assertion_results
        .iter()
        .find(|result| result.is_unsupported())
        .unwrap();
    assert!(unsupported.unsupported.as_deref().unwrap().contains("codex"));

    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let flattened = document["assertion_results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|result| result.get("unsupported").is_some())
        .expect("report.json carries the unsupported reason forward");
    assert_eq!(flattened["passed"], false);
    assert!(flattened["unsupported"].as_str().unwrap().contains("codex"));
}

fn ctx_with_outputs(dir: &Path) -> RunContext {
    let run_dir = dir.join("runs/run-001");
    let workspace = run_dir.join("workspace");
    let outputs = run_dir.join("outputs");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outputs).unwrap();
    RunContext {
        run_dir: run_dir.clone(),
        workspace_dir: workspace,
        outputs_dir: outputs,
        transcript_path: run_dir.join("transcript.jsonl"),
        skill_dir: dir.join("skill"),
        created_files: Vec::new(),
    }
}

#[test]
fn an_absent_final_answer_is_read_as_the_absence_it_is() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());

    let declarative = DeclarativeContext::load(&ctx).unwrap();

    assert!(declarative.final_text.is_empty());
    assert!(declarative.raw_transcript.is_empty());
}

#[test]
fn a_final_answer_that_cannot_be_read_is_not_reported_as_one_that_was_never_written() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    fs::write(ctx.outputs_dir.join(FINAL_MD), [0xff, 0xfe, 0x00]).unwrap();

    let err = match DeclarativeContext::load(&ctx) {
        Err(err) => err,
        Ok(_) => panic!("an unreadable final answer was read as if it had been written"),
    };

    assert_eq!(err.reported_as(), crate::exit_code::ExitCode::InfrastructureFailure);
    assert!(
        err.to_string().contains(FINAL_MD),
        "the report has to name the file nothing could read, got: {err}"
    );
}

#[test]
fn evidence_is_trivial_rejects_restatement() {
    assert!(evidence_is_trivial(
        "The output includes a summary",
        "The output includes a summary"
    ));
    assert!(evidence_is_trivial("includes summary", ""));
    assert!(!evidence_is_trivial(
        "includes summary",
        "found summary section in outputs/report.md"
    ));
}

#[test]
fn validate_grading_rejects_trivial_pass_evidence() {
    let grading = GradingFile {
        assertion_results: vec![AssertionGradeResult {
            name: None,
            assertion: "file out.json exists".to_string(),
            passed: true,
            evidence: "file out.json exists".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
            weight: None,
        }],
        summary: GradingSummary {
            passed: 1,
            failed: 0,
            total: 1,
            unsupported: 0,
            excluded: 0,
            ungraded: 0,
            pass_rate: Some(1.0),
        },
    };

    assert!(validate_grading_document(&grading, false).is_err());
}

#[test]
fn script_grader_integration() {
    let tmp = tempdir().unwrap();
    let script = tmp.path().join("fake-grader.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
read input
echo '{"passed": true, "evidence": "script verified workspace contents", "rationale": "ok"}'
"#,
    )
    .unwrap();

    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let ctx = ctx_with_outputs(tmp.path());
    fs::write(ctx.workspace_dir.join("done.txt"), "ok").unwrap();

    let options = GradeOptions {
        grader: GraderMode::Script,
        grader_provider: JudgeProvider::default(),
        grader_model: None,
        grader_command: Some(script.to_string_lossy().into_owned()),
        grader_votes: JudgeVotes::single(),
        strict: false,
    };

    let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "case",
        "prompt": "prompt long enough",
        "expected_output": "expected output",
    }))
    .unwrap();

    let result = grade_with_script("custom assertion", None, &eval_case, &ctx, &options).unwrap();
    assert!(result.passed);
    assert_eq!(result.grader.kind, GraderKind::Script);
    assert!(result.evidence.contains("script verified"));
}

fn baseline_case() -> EvalCase {
    serde_json::from_value(serde_json::json!({
        "id": "case",
        "prompt": "prompt long enough",
        "expected_output": "expected output",
    }))
    .unwrap()
}

fn reference_in(dir: &Path, text: &str) -> BaselineReference {
    let resolved = dir.join("golden.md");
    fs::create_dir_all(dir).unwrap();
    fs::write(&resolved, text).unwrap();
    BaselineReference::read(
        &serde_json::from_value(serde_json::json!("golden.md")).unwrap(),
        &resolved,
    )
    .unwrap()
}

/// A run that produced nothing is not at least as good as a reference that
/// exists, and there is nothing for a judge to read but the absence. Billing one
/// to be told so is the cost half of the same mistake.
#[test]
fn a_run_that_produced_nothing_falls_short_of_its_baseline_without_a_judge_being_asked() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let reference = reference_in(&ctx.skill_dir, "The reference answer.\n");
    let options = GradeOptions {
        grader: GraderMode::Llm,
        ..GradeOptions::default()
    };
    let session = GradeSession {
        options: &options,
        judge: None,
    };

    let result = grade_against_baseline(
        "final text is at least as good as baseline 'golden.md' on: is as complete",
        "is as complete",
        &GradeTarget::File(serde_json::from_value(serde_json::json!("missing.json")).unwrap()),
        &reference,
        &baseline_case(),
        &DeclarativeContext::load(&ctx).unwrap(),
        &ctx,
        &session,
    )
    .unwrap();

    assert!(!result.passed);
    assert_eq!(result.grader.kind, GraderKind::Declarative);
    assert!(result.grader.model.is_none(), "no judge answered this one");
}

#[test]
fn a_run_whose_output_is_empty_falls_short_of_its_baseline_without_a_judge_being_asked() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let reference = reference_in(&ctx.skill_dir, "The reference answer.\n");
    let options = GradeOptions {
        grader: GraderMode::Llm,
        ..GradeOptions::default()
    };
    let session = GradeSession {
        options: &options,
        judge: None,
    };

    let result = grade_against_baseline(
        "final text is at least as good as baseline 'golden.md' on: is as complete",
        "is as complete",
        &GradeTarget::FinalText,
        &reference,
        &baseline_case(),
        &DeclarativeContext::load(&ctx).unwrap(),
        &ctx,
        &session,
    )
    .unwrap();

    assert!(!result.passed, "nothing is not at least as good as a baseline");
    assert!(result.grader.model.is_none(), "no judge was billed for an empty run");
}

struct LocalJudge(String);

impl judge::JudgeEnv for LocalJudge {
    fn var(&self, key: &str) -> Option<String> {
        match key {
            judge::BASE_URL_ENV => Some(self.0.clone()),
            judge::API_KEY_ENV => Some("test-key".to_string()),
            _ => None,
        }
    }
}

fn local_endpoint(listener: &std::net::TcpListener) -> JudgeEndpoint {
    JudgeEndpoint::resolve_from(
        &LocalJudge(format!("http://{}/v1", listener.local_addr().unwrap())),
        JudgeProvider::OpenAi,
        &JudgeModel::new("judge-model").unwrap(),
        "--grader-model",
    )
    .unwrap()
}

/// Answers one judge request with `verdict` and hands back the body it was sent.
fn serve_one_verdict(listener: std::net::TcpListener, verdict: serde_json::Value) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
            }
        }
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        let body = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": verdict.to_string()}, "finish_reason": "stop"}],
        })
        .to_string();
        let mut stream = stream;
        write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        String::from_utf8(request).unwrap()
    })
}

/// A judge shown nothing still answers, and a lenient one passes an assertion that
/// reads as satisfied by saying nothing. The listener stands in for the judge, so a
/// request reaching it, rather than a missing endpoint, is what would fail this.
#[test]
fn an_empty_answer_fails_its_llm_assertion_without_a_judge_being_asked() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let options = GradeOptions {
        grader: GraderMode::Llm,
        grader_votes: "3".parse().unwrap(),
        ..GradeOptions::default()
    };
    let session = GradeSession {
        options: &options,
        judge: Some(local_endpoint(&listener)),
    };

    for target in [
        TargetDeclaration::Default,
        TargetDeclaration::Named(GradeTarget::AnyOutput),
    ] {
        let result = grade_with_llm(
            "the answer does not invent facts",
            &target,
            &BRIEF,
            &declarative_context(" \n\t", ""),
            &ctx,
            &session,
        )
        .unwrap();

        assert!(!result.passed, "an empty answer must not pass");
        assert_eq!(result.grader.kind, GraderKind::Declarative);
        assert!(result.grader.model.is_none(), "no judge answered this one");
        assert!(result.evidence.contains("is empty"), "{}", result.evidence);
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "no request may reach the judge for an empty answer"
    );
}

/// A run that put its answer in a file and left the final message blank has still
/// answered, and the default judge is shown that file.
#[test]
fn an_empty_final_text_with_an_output_file_is_still_put_to_the_judge() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let blank = declarative_context(" \n", "");

    fs::write(ctx.outputs_dir.join("notes.txt"), "   \n").unwrap();
    assert!(
        blank_answer(&GradeTarget::FinalText, &blank, &ctx).is_some(),
        "a blank output file is no more an answer than blank final text"
    );

    fs::write(ctx.outputs_dir.join("answer.md"), "The answer is in this file.").unwrap();
    assert!(
        blank_answer(&GradeTarget::FinalText, &blank, &ctx).is_none(),
        "an output file with content is an answer the judge must read"
    );
    assert!(
        blank_answer(&GradeTarget::AnyOutput, &blank, &ctx).is_none(),
        "any_output already reads the file as part of the answer"
    );
}

#[test]
fn the_judge_is_asked_with_the_question_and_the_verified_reference() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = local_endpoint(&listener);
    let server = serve_one_verdict(
        listener,
        serde_json::json!({"passed": true, "evidence": "Paris", "rationale": "matches the reference"}),
    );
    let options = GradeOptions {
        grader: GraderMode::Llm,
        ..GradeOptions::default()
    };
    let session = GradeSession {
        options: &options,
        judge: Some(endpoint),
    };
    let case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "capital",
        "prompt": "What is the capital of France?",
        "expected_output": "Paris",
    }))
    .unwrap();

    let result = grade_with_llm(
        "names the capital correctly",
        &TargetDeclaration::Default,
        &CaseBrief::of(&case),
        &declarative_context("Paris", ""),
        &ctx,
        &session,
    )
    .unwrap();

    assert!(result.passed);
    let body: serde_json::Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert!(body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("ground truth"));
    let sent: serde_json::Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(sent["prompt"], "What is the capital of France?");
    assert_eq!(sent["expected_output"], "Paris");
    assert_eq!(sent["final_text"], "Paris");
}

#[test]
fn the_run_answer_keeps_first_claim_on_the_payload_and_the_brief_is_cut_before_it() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let huge = "x".repeat(PayloadBudget::TOTAL_BYTES * 2);
    let brief = CaseBrief {
        prompt: &huge,
        expected_output: &huge,
    };
    let answer = "a".repeat(PayloadBudget::TOTAL_BYTES);

    let payload = llm_grader_payload(
        "the assertion",
        &GradeTarget::default(),
        &brief,
        &declarative_context(&answer, ""),
        &ctx,
    )
    .unwrap()
    .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    let kept = |field: &str| value[field].as_str().unwrap().chars().filter(|c| *c != '\n').count();
    assert!(
        value["final_text"].as_str().unwrap().len() > PayloadBudget::TOTAL_BYTES - 2 * CaseBrief::FLOOR_BYTES - 50,
        "the answer keeps everything the brief does not reserve"
    );
    assert!(kept("expected_output") < CaseBrief::FLOOR_BYTES + 50);
    assert!(kept("prompt") < CaseBrief::FLOOR_BYTES + 50);
    assert!(value["expected_output"].as_str().unwrap().contains("[truncated after"));
}

#[test]
fn a_reference_takes_what_a_short_answer_leaves_before_the_question_does() {
    let tmp = tempdir().unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let reference = "r".repeat(PayloadBudget::TOTAL_BYTES - 100);
    let question = "q".repeat(PayloadBudget::TOTAL_BYTES);
    let brief = CaseBrief {
        prompt: &question,
        expected_output: &reference,
    };

    let payload = llm_grader_payload(
        "the assertion",
        &GradeTarget::default(),
        &brief,
        &declarative_context("short answer", ""),
        &ctx,
    )
    .unwrap()
    .text;
    let value: serde_json::Value = serde_json::from_str(&payload).unwrap();

    assert_eq!(value["final_text"], "short answer");
    let sent_reference = value["expected_output"].as_str().unwrap();
    assert!(
        sent_reference.len() >= PayloadBudget::TOTAL_BYTES - CaseBrief::FLOOR_BYTES - 20,
        "the reference keeps all but the question's floor: {} bytes",
        sent_reference.len()
    );
    assert!(value["prompt"].as_str().unwrap().contains("[truncated after"));
}

#[test]
fn a_baseline_graded_by_script_hands_the_script_what_it_must_compare_against() {
    let tmp = tempdir().unwrap();
    let script = tmp.path().join("grader.sh");
    fs::write(
        &script,
        r#"#!/bin/bash
payload=$(cat)
case "$payload" in
  *"The reference answer."*) echo '{"passed": true, "evidence": "the reference reached the script"}' ;;
  *) echo '{"passed": false, "evidence": "the script was asked to compare against nothing"}' ;;
esac
"#,
    )
    .unwrap();

    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let ctx = ctx_with_outputs(tmp.path());
    let reference = reference_in(&ctx.skill_dir, "The reference answer.\n");
    let options = GradeOptions {
        grader: GraderMode::Script,
        grader_command: Some(script.to_string_lossy().into_owned()),
        ..GradeOptions::default()
    };

    let result = grade_with_script("is as complete", Some(&reference), &baseline_case(), &ctx, &options).unwrap();

    assert!(result.passed, "{}", result.evidence);
}

/// A crash is not a verdict. Counting a broken grader as a failed assertion reports that
/// the skill did the wrong thing on the strength of evidence that says nothing about the
/// skill at all.
#[test]
fn a_crashed_grader_script_grades_nothing_rather_than_failing_the_assertion() {
    let tmp = tempdir().unwrap();
    let script = tmp.path().join("broken-grader.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
echo 'grader could not reach the model' >&2
exit 3
"#,
    )
    .unwrap();

    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let ctx = ctx_with_outputs(tmp.path());
    let options = GradeOptions {
        grader: GraderMode::Script,
        grader_provider: JudgeProvider::default(),
        grader_model: None,
        grader_command: Some(script.to_string_lossy().into_owned()),
        grader_votes: JudgeVotes::single(),
        strict: false,
    };
    let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "case",
        "prompt": "prompt long enough",
        "expected_output": "expected output",
    }))
    .unwrap();

    let result = grade_with_script("custom assertion", None, &eval_case, &ctx, &options).unwrap();

    assert!(
        result.is_ungraded(),
        "a grader that never exited cleanly attempted nothing"
    );
    assert!(!result.is_scored());
    assert!(result.evidence.contains("grader could not reach the model"));

    let counts = GradingCounts::tally(std::slice::from_ref(&result));
    assert_eq!(counts.ungraded, 1);
    assert_eq!(counts.failed, 0, "a broken grader must not read as the skill failing");
}

/// A payload too large for the pipe buffer cannot be handed over without the script reading
/// it, so the script exiting first closes the pipe every time rather than only when it wins
/// a race. That is the shape a real grader crash takes, and it must still reach a verdict.
#[test]
fn a_grader_that_exits_without_reading_its_payload_is_answered_by_its_exit_status() {
    let tmp = tempdir().unwrap();
    let script = tmp.path().join("deaf-grader.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
echo 'grader refused the payload' >&2
exit 4
"#,
    )
    .unwrap();

    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let ctx = ctx_with_outputs(tmp.path());
    let options = GradeOptions {
        grader: GraderMode::Script,
        grader_provider: JudgeProvider::default(),
        grader_model: None,
        grader_command: Some(script.to_string_lossy().into_owned()),
        grader_votes: JudgeVotes::single(),
        strict: false,
    };
    let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "case",
        "prompt": "prompt long enough",
        "expected_output": "expected output",
        "grader_hints": { "blob": "x".repeat(512 * 1024) },
    }))
    .unwrap();

    let result = grade_with_script("custom assertion", None, &eval_case, &ctx, &options).unwrap();

    assert!(
        result.is_ungraded(),
        "a grader that never exited cleanly attempted nothing"
    );
    assert!(result.evidence.contains("grader refused the payload"));
}

/// Both pipes are filled past what they hold, in the one order that wedges a parent which
/// insists on finishing the write before it starts reading. The deadline is the assertion:
/// a regression here hangs rather than fails, and a hung test reports nothing at all.
#[test]
fn a_grader_printing_more_than_a_pipe_holds_does_not_wedge_the_harness() {
    let tmp = tempdir().unwrap();
    let script = tmp.path().join("loud-grader.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
awk 'BEGIN { while (i++ < 40000) print "padding for the stderr pipe" }' >&2
echo '{"passed": true, "evidence": "script verified"}'
"#,
    )
    .unwrap();

    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let outputs = tmp.path().to_path_buf();
    let command = script.to_string_lossy().into_owned();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ctx = ctx_with_outputs(&outputs);
        let options = GradeOptions {
            grader: GraderMode::Script,
            grader_provider: JudgeProvider::default(),
            grader_model: None,
            grader_command: Some(command),
            grader_votes: JudgeVotes::single(),
            strict: false,
        };
        let eval_case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "case",
            "prompt": "prompt long enough",
            "expected_output": "expected output",
            "grader_hints": { "blob": "x".repeat(512 * 1024) },
        }))
        .unwrap();
        let _ = done.send(grade_with_script("custom assertion", None, &eval_case, &ctx, &options).map(|r| r.passed));
    });

    match finished.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(passed)) => assert!(passed, "the grader reported a pass once both pipes drained"),
        Ok(Err(e)) => panic!("grading the loud script failed: {e}"),
        Err(_) => panic!("the harness and the grader each waited on a pipe only the other could drain"),
    }
}

fn result_with_votes(votes: Option<JudgeVoteTally>) -> AssertionGradeResult {
    AssertionGradeResult {
        name: None,
        assertion: "the summary reads well".to_string(),
        passed: true,
        evidence: "the summary names every column".to_string(),
        grader: GraderInfo {
            kind: GraderKind::Llm,
            model: Some("judge-model".to_string()),
            command: None,
        },
        rationale: None,
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes,
        weight: None,
    }
}

/// A reader who never asked for a panel should see the same grading.json they saw
/// before panels existed, so the field has to stay away rather than report a
/// one-vote panel that says nothing.
#[test]
fn a_result_no_panel_decided_reports_no_split() {
    let json = serde_json::to_value(result_with_votes(None)).unwrap();
    assert!(json.get("votes").is_none());
}

#[test]
fn a_result_a_divided_panel_decided_carries_the_split() {
    let json = serde_json::to_value(result_with_votes(Some(JudgeVoteTally { passed: 2, failed: 1 }))).unwrap();
    assert_eq!(json["votes"]["passed"], 2);
    assert_eq!(json["votes"]["failed"], 1);

    let parsed: AssertionGradeResult = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.votes, Some(JudgeVoteTally { passed: 2, failed: 1 }));
    assert!(!parsed.votes.unwrap().is_unanimous());
}

/// A grading.json written before panels existed still has to parse, since a report
/// bundle outlives the pass that produced it.
#[test]
fn a_result_written_before_panels_existed_still_parses() {
    let parsed: AssertionGradeResult = serde_json::from_value(serde_json::json!({
        "assertion": "the summary reads well",
        "passed": true,
        "evidence": "the summary names every column",
        "grader": {"kind": "llm", "model": "judge-model"}
    }))
    .unwrap();
    assert!(parsed.votes.is_none());
}

const READ_ONLY_FIXTURE_SUITE: &str = r#"{
        "skill_name": "demo-skill",
        "evals": [
            {
                "id": "case-a",
                "prompt": "prompt a",
                "expected_output": "output a",
                "graders": [
                    {"type": "contains", "text": "all done"}
                ]
            }
        ]
    }"#;

fn single_run_report_dir(temp: &tempfile::TempDir) -> PathBuf {
    let skill_dir = temp.path().join("demo-skill");
    fs::create_dir_all(skill_dir.join("evals")).unwrap();
    let skill_md = "---\nname: demo-skill\ndescription: d\n---\n";
    fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
    fs::write(skill_dir.join("evals/evals.json"), READ_ONLY_FIXTURE_SUITE).unwrap();

    let mem = MemFS::new();
    let mem_skill = Path::new("demo-skill");
    mem.insert(mem_skill.join("SKILL.md"), skill_md);
    mem.insert(mem_skill.join("evals/evals.json"), READ_ONLY_FIXTURE_SUITE);

    let bundle = build_report_bundle(
        &mem,
        mem_skill,
        &skill_dir,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("report-read-only".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            runner: Some("codex".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let report_dir = write_report_bundle(&temp.path().join("out"), &bundle, WriteReportOptions::default()).unwrap();
    let workspace = report_dir.join(&bundle.document.runs[0].paths.workspace);
    let run_dir = workspace.parent().unwrap().to_path_buf();
    fs::write(workspace.join("outputs").join(FINAL_MD), "all done\n").unwrap();
    let transcript_path = run_dir.join("transcript.jsonl");
    fs::write(&transcript_path, "{\"type\":\"turn.completed\"}\n").unwrap();
    write_normalized_transcript(&transcript_path, &NormalizedTranscript::unavailable("codex")).unwrap();

    report_dir
}

/// The run-level outcome field is what the grading layer turns into a failed case: this
/// proves that path end to end, including that the case cannot pass on the strength of
/// its own grader once a read-only fixture it was handed came back changed.
#[test]
fn a_read_only_fixture_violation_fails_the_case_even_when_its_own_grader_passes() {
    let temp = tempdir().unwrap();
    let report_dir = single_run_report_dir(&temp);

    let report_path = report_dir.join("report.json");
    let mut document: ReportDocument = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    document.runs[0].read_only_fixture_violations = vec!["evals/files/input.csv".to_string()];
    fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let report = grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    assert_eq!(report.passed, 1, "the case's own grader still passes");
    assert_eq!(
        report.failed, 1,
        "a mutated read-only fixture fails the case regardless of what its own checks found"
    );

    let (_, grading) = &grading_files_by_scenario(&report_dir)[0];
    let violation = grading
        .assertion_results
        .iter()
        .find(|result| result.evidence.contains("evals/files/input.csv"))
        .expect("the operator sees the fixture path, not just a count");
    assert!(!violation.passed);
}

fn run_reference() -> SpanReference {
    serde_json::from_value(serde_json::json!({
        "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
        "span_id": "00f067aa0ba902b7",
    }))
    .unwrap()
}

fn with_run_reference(report_dir: &Path, reference: SpanReference) -> String {
    let path = report_dir.join("report.json");
    let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    document["runs"][0]["trace"] = serde_json::to_value(reference).unwrap();
    fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();
    document["runs"][0]["id"].as_str().unwrap().to_string()
}

#[test]
fn grading_a_run_links_to_the_run_span_and_reports_its_evaluations_in_that_trace() {
    let temp = tempdir().unwrap();
    let (report_dir, _) = unobservable_report_dir(&temp);
    let reference = run_reference();
    let run_id = with_run_reference(&report_dir, reference);

    let (report, trace, logs) = telemetry::testing::capture_with_logs(|| {
        grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
    });

    report.unwrap();
    let run_span = trace
        .span_where("grade run", EVAL_RUN_ID, &run_id)
        .expect("the run's grade span, told apart by its run id");
    assert_eq!(
        run_span.parent_span_id,
        trace.span("grade").unwrap().span_context.span_id()
    );
    assert!(
        run_span
            .links
            .iter()
            .any(|link| link.span_context.trace_id() == reference.trace_id()
                && link.span_context.span_id() == reference.span_id()),
        "the grade span links back to the run span"
    );
    for child in ["grader skill_used", "grader contains", "write grading results"] {
        assert_eq!(trace.children_named(run_span, child).len(), 1, "{child}");
    }

    let evaluations: Vec<_> = logs
        .iter()
        .filter(|record| record.event_name() == Some("gen_ai.evaluation.result"))
        .collect();
    assert!(evaluations.len() >= 3, "one per grader plus the case score");
    for record in &evaluations {
        assert_eq!(
            record.trace_context().map(|context| context.trace_id),
            Some(reference.trace_id()),
            "an evaluation lands in the run's trace"
        );
    }
    let names: Vec<String> = evaluations
        .iter()
        .filter_map(|record| {
            record
                .attributes_iter()
                .find(|(key, _)| key.as_str() == GEN_AI_EVALUATION_NAME)
                .map(|(_, value)| format!("{value:?}"))
        })
        .collect();
    assert!(
        names.iter().any(|name| name.contains(telemetry::CASE_SCORE_EVALUATION)),
        "{names:?}"
    );
    assert!(
        evaluations.iter().all(|record| record
            .attributes_iter()
            .all(|(key, _)| key.as_str() != GEN_AI_EVALUATION_EXPLANATION)),
        "the explanation quotes graded content, so it stays off by default"
    );
}

#[test]
fn grading_a_run_records_its_grade_span_in_the_report_for_later_links() {
    let temp = tempdir().unwrap();
    let (report_dir, _) = unobservable_report_dir(&temp);
    let run_id = with_run_reference(&report_dir, run_reference());

    let (report, trace) = crate::telemetry::testing::capture(|| {
        grade_report_bundle(
            &report_dir,
            GradeOptions {
                grader: GraderMode::None,
                ..GradeOptions::default()
            },
        )
    });

    report.unwrap();
    let grade_span = trace.span_where("grade run", EVAL_RUN_ID, &run_id).unwrap();
    let document: ReportDocument =
        serde_json::from_str(&fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let run = document.runs.iter().find(|run| run.id == run_id).unwrap();
    let recorded = run.grade_span_reference().expect("a traced grade is recorded");
    assert_eq!(recorded.span_id(), grade_span.span_context.span_id());
    assert_eq!(recorded.trace_id(), grade_span.span_context.trace_id());
    assert_eq!(
        run.span_reference(),
        Some(&run_reference()),
        "the run keeps the span it executed under"
    );
}

#[test]
fn regrading_without_tracing_drops_a_grade_span_that_no_longer_describes_the_grade() {
    let temp = tempdir().unwrap();
    let (report_dir, _) = unobservable_report_dir(&temp);
    let path = report_dir.join("report.json");
    let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    document["runs"][0]["grade_trace"] = serde_json::to_value(run_reference()).unwrap();
    fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    grade_report_bundle(
        &report_dir,
        GradeOptions {
            grader: GraderMode::None,
            ..GradeOptions::default()
        },
    )
    .unwrap();

    let document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(document["runs"][0].get("grade_trace").is_none());
}

#[test]
fn a_grader_script_runs_under_a_cli_span_it_receives_as_traceparent() {
    let tmp = tempdir().unwrap();
    let script = tmp.path().join("trace-grader.sh");
    fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\necho \"{\\\"passed\\\": true, \\\"evidence\\\": \\\"$TRACEPARENT\\\"}\"\n",
    )
    .unwrap();

    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let ctx = ctx_with_outputs(tmp.path());
    let options = GradeOptions {
        grader: GraderMode::Script,
        grader_command: Some(script.to_string_lossy().into_owned()),
        ..GradeOptions::default()
    };

    let (result, trace) = crate::telemetry::testing::capture(|| {
        grade_with_script("custom assertion", None, &baseline_case(), &ctx, &options)
    });

    let result = result.unwrap();
    let process = trace.span("trace-grader.sh").expect("the script runs under a CLI span");
    assert_eq!(process.span_kind, opentelemetry::trace::SpanKind::Client);
    assert_eq!(
        trace.attribute("trace-grader.sh", PROCESS_EXIT_CODE),
        Some(opentelemetry::Value::I64(0))
    );
    assert!(matches!(
        trace.attribute("trace-grader.sh", PROCESS_PID),
        Some(opentelemetry::Value::I64(_))
    ));
    let context = &process.span_context;
    assert_eq!(
        result.evidence,
        format!("00-{}-{}-01", context.trace_id(), context.span_id()),
        "the script sees the CLI span as its parent"
    );
}
