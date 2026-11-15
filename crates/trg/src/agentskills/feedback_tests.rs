use super::*;
use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::fs::testutil::MemFS;
use std::path::Path;

fn sample_report_dir(temp: &tempfile::TempDir) -> PathBuf {
    let fs = MemFS::new();
    let skill_path = Path::new("demo-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(
        skill_path.join("evals/evals.json"),
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "graders": [{ "type": "contains", "text": "assert a" }]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "graders": [{ "type": "contains", "text": "assert b" }]
                    }
                ]
            }"#,
    );

    let bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("demo-skill"),
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("report-test".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap()
}

/// Writes a `grading.json` for each of `sample_report_dir`'s two runs directly,
/// rather than through `grade_report_bundle`, since a fabricated result is enough to
/// exercise the assertion-id join and none of these tests need a real runner output.
fn grade_sample_report(report_dir: &Path) {
    for (run_id, assertion_text, passed) in [("run-001", "assert a", true), ("run-002", "assert b", false)] {
        let result = AssertionGradeResult {
            assertion: assertion_text.to_string(),
            passed,
            evidence: format!("evidence for {assertion_text}"),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            name: None,
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
            weight: None,
        };
        let grading = build_grading_file(vec![result]).unwrap();
        std::fs::write(
            report_dir.join(format!("runs/{run_id}/grading.json")),
            serde_json::to_string_pretty(&grading).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn parse_feedback_document_accepts_empty_notes() {
    let document = parse_feedback_document(
        r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": []
            }"#,
    )
    .unwrap();

    assert_eq!(document.reviewer, "reviewer@example.com");
    assert!(document.notes.is_empty());
}

#[test]
fn parse_feedback_document_rejects_invalid_severity() {
    let err = parse_feedback_document(
        r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [
                    {"severity": "critical", "category": "correctness", "text": "bad"}
                ]
            }"#,
    )
    .unwrap_err();

    assert!(err.to_string().contains("critical") || err.to_string().contains("severity"));
}

#[test]
fn parse_feedback_document_rejects_empty_note_text() {
    let err = parse_feedback_document(
        r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [
                    {"severity": "warning", "category": "style", "text": "   "}
                ]
            }"#,
    )
    .unwrap_err();

    assert!(err.to_string().contains("notes[0].text"));
}

#[test]
fn init_creates_feedback_for_each_run() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);

    let report = init_feedback(&report_dir, Some("human@example.com")).unwrap();
    assert_eq!(report.created, 2);
    assert_eq!(report.skipped, 0);

    for run_id in ["run-001", "run-002"] {
        let path = report_dir.join(format!("runs/{run_id}/feedback.json"));
        assert!(path.is_file());
        let document = read_feedback_document(&path).unwrap();
        assert_eq!(document.reviewer, "human@example.com");
        assert!(document.notes.is_empty());
    }
}

#[test]
fn init_skips_existing_feedback_files() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    init_feedback(&report_dir, Some("first@example.com")).unwrap();

    let report = init_feedback(&report_dir, Some("second@example.com")).unwrap();
    assert_eq!(report.created, 0);
    assert_eq!(report.skipped, 2);

    let path = report_dir.join("runs/run-001/feedback.json");
    let document = read_feedback_document(&path).unwrap();
    assert_eq!(document.reviewer, "first@example.com");
}

#[test]
fn list_reports_runs_without_feedback() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);

    let pending = list_runs_needing_review(&report_dir).unwrap();
    assert_eq!(pending, vec!["run-001".to_string(), "run-002".to_string()]);

    init_feedback(&report_dir, Some("human@example.com")).unwrap();
    let pending = list_runs_needing_review(&report_dir).unwrap();
    assert!(pending.is_empty());
}

#[test]
fn validate_reports_schema_errors() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    init_feedback(&report_dir, Some("human@example.com")).unwrap();

    let invalid_path = report_dir.join("runs/run-001/feedback.json");
    std::fs::write(
        &invalid_path,
        r#"{
                "reviewer": "human@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [
                    {"severity": "blocker", "category": "correctness", "text": ""}
                ]
            }"#,
    )
    .unwrap();

    let report = validate_feedback(&report_dir).unwrap();
    assert_eq!(report.validated, 1);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("run run-001"));
}

#[test]
fn summarize_feedback_counts_by_severity_and_category() {
    let entries = vec![RunFeedbackEntry {
        run_id: "run-001".to_string(),
        eval_case_id: "case-a".to_string(),
        scenario_id: ScenarioKind::WithSkill,
        source_path: "runs/run-001/feedback.json".to_string(),
        feedback: FeedbackDocument {
            reviewer: "human@example.com".to_string(),
            reviewed_at: "2026-05-26T12:00:00Z".to_string(),
            notes: vec![
                FeedbackNote {
                    severity: FeedbackSeverity::Info,
                    category: FeedbackCategory::Style,
                    text: "Minor formatting issue in summary.md".to_string(),
                },
                FeedbackNote {
                    severity: FeedbackSeverity::Blocker,
                    category: FeedbackCategory::Correctness,
                    text: "Missing required chart output".to_string(),
                },
            ],
            assertion_verdicts: BTreeMap::new(),
        },
    }];

    let summary = summarize_feedback(2, &entries);
    assert_eq!(summary.total_runs, 2);
    assert_eq!(summary.reviewed_runs, 1);
    assert_eq!(summary.pending_runs, 1);
    assert_eq!(summary.by_severity.info, 1);
    assert_eq!(summary.by_severity.blocker, 1);
    assert_eq!(summary.by_category.style, 1);
    assert_eq!(summary.by_category.correctness, 1);
}

#[test]
fn collect_improvement_feedback_omits_clean_reviews() {
    let entries = vec![
        RunFeedbackEntry {
            run_id: "run-001".to_string(),
            eval_case_id: "case-a".to_string(),
            scenario_id: ScenarioKind::WithSkill,
            source_path: "runs/run-001/feedback.json".to_string(),
            feedback: FeedbackDocument {
                reviewer: "human@example.com".to_string(),
                reviewed_at: "2026-05-26T12:00:00Z".to_string(),
                notes: vec![],
                assertion_verdicts: BTreeMap::new(),
            },
        },
        RunFeedbackEntry {
            run_id: "run-002".to_string(),
            eval_case_id: "case-b".to_string(),
            scenario_id: ScenarioKind::WithSkill,
            source_path: "runs/run-002/feedback.json".to_string(),
            feedback: FeedbackDocument {
                reviewer: "human@example.com".to_string(),
                reviewed_at: "2026-05-26T12:00:00Z".to_string(),
                notes: vec![FeedbackNote {
                    severity: FeedbackSeverity::Warning,
                    category: FeedbackCategory::Completeness,
                    text: "Add edge-case coverage for empty CSV".to_string(),
                }],
                assertion_verdicts: BTreeMap::new(),
            },
        },
    ];

    let records = collect_improvement_feedback(&entries);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run_id, "run-002");
    assert_eq!(records[0].notes.len(), 1);
}

#[test]
fn parse_feedback_document_accepts_assertion_verdicts() {
    let document = parse_feedback_document(
        r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-a:g0": {"verdict": "fail", "rationale": "evidence quoted the wrong file"}
                }
            }"#,
    )
    .unwrap();

    let verdict = &document.assertion_verdicts["case-a:g0"];
    assert_eq!(verdict.verdict, HumanVerdict::Fail);
    assert_eq!(verdict.rationale.as_deref(), Some("evidence quoted the wrong file"));
}

#[test]
fn parse_feedback_document_rejects_invalid_verdict() {
    let err = parse_feedback_document(
        r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-a:g0": {"verdict": "maybe"}
                }
            }"#,
    )
    .unwrap_err();

    assert!(err.to_string().contains("maybe") || err.to_string().contains("verdict"));
}

#[test]
fn parse_feedback_document_rejects_blank_rationale() {
    let err = parse_feedback_document(
        r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-a:g0": {"verdict": "pass", "rationale": "   "}
                }
            }"#,
    )
    .unwrap_err();

    assert!(err.to_string().contains("rationale"));
}

#[test]
fn record_assertion_verdict_creates_feedback_and_is_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_sample_report(&report_dir);

    let report = record_assertion_verdict(
        &report_dir,
        "run-001",
        "case-a:g0",
        HumanVerdict::Pass,
        Some("looks right".to_string()),
        Some("reviewer@example.com"),
    )
    .unwrap();
    assert!(!report.replaced);

    let document = read_feedback_document(&report_dir.join("runs/run-001/feedback.json")).unwrap();
    let verdict = &document.assertion_verdicts["case-a:g0"];
    assert_eq!(verdict.verdict, HumanVerdict::Pass);
    assert_eq!(verdict.rationale.as_deref(), Some("looks right"));

    let report = record_assertion_verdict(
        &report_dir,
        "run-001",
        "case-a:g0",
        HumanVerdict::Fail,
        None,
        Some("reviewer@example.com"),
    )
    .unwrap();
    assert!(report.replaced);

    let document = read_feedback_document(&report_dir.join("runs/run-001/feedback.json")).unwrap();
    let verdict = &document.assertion_verdicts["case-a:g0"];
    assert_eq!(verdict.verdict, HumanVerdict::Fail);
    assert!(verdict.rationale.is_none());
    assert_eq!(document.assertion_verdicts.len(), 1);
}

#[test]
fn record_assertion_verdict_rejects_an_unknown_assertion() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_sample_report(&report_dir);

    let err = record_assertion_verdict(
        &report_dir,
        "run-001",
        "case-a:g7",
        HumanVerdict::Pass,
        None,
        Some("reviewer@example.com"),
    )
    .unwrap_err();

    assert!(err.to_string().contains("case-a:g7"));
}

#[test]
fn record_assertion_verdict_rejects_an_ungraded_run() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);

    let err = record_assertion_verdict(
        &report_dir,
        "run-001",
        "case-a:g0",
        HumanVerdict::Pass,
        None,
        Some("reviewer@example.com"),
    )
    .unwrap_err();

    assert!(err.to_string().contains("has not been graded"));
}

#[test]
fn validate_reports_an_assertion_id_the_run_never_graded() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_sample_report(&report_dir);
    init_feedback(&report_dir, Some("human@example.com")).unwrap();

    let feedback_path = report_dir.join("runs/run-001/feedback.json");
    std::fs::write(
        &feedback_path,
        r#"{
                "reviewer": "human@example.com",
                "reviewed_at": "2026-05-26T12:00:00Z",
                "notes": [],
                "assertion_verdicts": {
                    "case-b:g0": {"verdict": "pass"}
                }
            }"#,
    )
    .unwrap();

    let report = validate_feedback(&report_dir).unwrap();
    assert_eq!(report.validated, 1);
    assert_eq!(report.errors.len(), 1);
    assert!(report.errors[0].contains("case-b:g0"));
}

#[test]
fn validate_accepts_a_verdict_on_a_graded_assertion() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_sample_report(&report_dir);
    init_feedback(&report_dir, Some("human@example.com")).unwrap();

    record_assertion_verdict(
        &report_dir,
        "run-001",
        "case-a:g0",
        HumanVerdict::Pass,
        None,
        Some("human@example.com"),
    )
    .unwrap();

    let report = validate_feedback(&report_dir).unwrap();
    assert_eq!(report.validated, 2);
    assert!(report.errors.is_empty());
}

#[test]
fn invalid_review_timestamp_retains_the_parse_error() {
    let error = parse_feedback_document(r#"{"reviewer":"reviewer","reviewed_at":"invalid","notes":[]}"#)
        .expect_err("invalid timestamps must be rejected");
    assert_eq!(
        error.to_string(),
        "reviewed_at 'invalid' is not a valid RFC3339 timestamp"
    );
    let source = std::error::Error::source(&error).expect("timestamp parse cause is retained");
    assert!(
        source.downcast_ref::<chrono::ParseError>().is_some(),
        "the typed timestamp parser error must remain available"
    );
}
