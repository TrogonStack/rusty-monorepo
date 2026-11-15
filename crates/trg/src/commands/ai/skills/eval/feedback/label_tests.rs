use super::*;
use crate::agentskills::feedback::FeedbackDocument;
use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
use crate::commands::ai::skills::eval::feedback::testutil::sample_report_dir;

fn read_feedback(report_dir: &std::path::Path, run_id: &str) -> FeedbackDocument {
    let content = std::fs::read_to_string(report_dir.join(format!("runs/{run_id}/feedback.json"))).unwrap();
    serde_json::from_str(&content).unwrap()
}

fn grade_run(report_dir: &std::path::Path, run_id: &str, assertion: &str) {
    let result = AssertionGradeResult {
        assertion: assertion.to_string(),
        passed: true,
        evidence: "matched".to_string(),
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

#[test]
fn label_command_records_a_verdict() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_run(&report_dir, "run-001", "assert a");

    let status = FeedbackLabelArgs {
        report_dir: report_dir.clone(),
        run: "run-001".to_string(),
        assertion: "case-a:g0".to_string(),
        verdict: HumanVerdict::Pass,
        rationale: Some("matches expected output".to_string()),
        reviewer: Some("reviewer@example.com".to_string()),
        output_format: OutputFormat::Text,
    }
    .handle();
    assert_eq!(status, ExitCode::Success);

    let document = read_feedback(&report_dir, "run-001");
    let verdict = &document.assertion_verdicts["case-a:g0"];
    assert_eq!(verdict.verdict, HumanVerdict::Pass);
}

#[test]
fn label_command_rejects_an_unknown_assertion() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_run(&report_dir, "run-001", "assert a");

    let status = FeedbackLabelArgs {
        report_dir,
        run: "run-001".to_string(),
        assertion: "case-a:g9".to_string(),
        verdict: HumanVerdict::Pass,
        rationale: None,
        reviewer: Some("reviewer@example.com".to_string()),
        output_format: OutputFormat::Text,
    }
    .handle();
    assert_eq!(status, ExitCode::InfrastructureFailure);
}
