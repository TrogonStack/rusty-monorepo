use super::*;
use crate::agentskills::feedback::record_assertion_verdict;
use crate::agentskills::grading::{build_grading_file, GraderInfo};
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::fs::testutil::MemFS;
use std::path::PathBuf;

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
                        "graders": [
                            { "type": "contains", "text": "assert a0" },
                            { "type": "llm", "criterion": "assert a1" }
                        ]
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

fn grade_result(assertion: &str, passed: bool, kind: GraderKind) -> AssertionGradeResult {
    AssertionGradeResult {
        assertion: assertion.to_string(),
        passed,
        evidence: format!("evidence for {assertion}"),
        grader: GraderInfo {
            kind,
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
    }
}

fn write_grading(report_dir: &Path, run_id: &str, results: Vec<AssertionGradeResult>) {
    let grading = build_grading_file(results).unwrap();
    std::fs::write(
        report_dir.join(format!("runs/{run_id}/grading.json")),
        serde_json::to_string_pretty(&grading).unwrap(),
    )
    .unwrap();
}

#[test]
fn agreements_and_disagreements_are_tallied_per_grader_kind() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);

    write_grading(
        &report_dir,
        "run-001",
        vec![
            grade_result("assert a0", true, GraderKind::Mechanical),
            grade_result("assert a1", true, GraderKind::Llm),
        ],
    );
    write_grading(
        &report_dir,
        "run-002",
        vec![grade_result("assert b", true, GraderKind::Mechanical)],
    );

    record_assertion_verdict(&report_dir, "run-001", "case-a:g0", HumanVerdict::Pass, None, Some("h")).unwrap();
    record_assertion_verdict(
        &report_dir,
        "run-001",
        "case-a:g1",
        HumanVerdict::Fail,
        Some("judge missed the caveat".to_string()),
        Some("h"),
    )
    .unwrap();
    record_assertion_verdict(&report_dir, "run-002", "case-b:g0", HumanVerdict::Pass, None, Some("h")).unwrap();

    let document = build_grader_agreement_document(&report_dir).unwrap();

    assert_eq!(document.overall.agreements, 2);
    assert_eq!(document.overall.disagreements, 1);
    assert_eq!(document.by_grader_kind.mechanical.agreements, 2);
    assert_eq!(document.by_grader_kind.llm.disagreements, 1);
    assert_eq!(document.disagreements.len(), 1);
    assert_eq!(document.disagreements[0].assertion_id, "case-a:g1");
    assert_eq!(
        document.disagreements[0].human_rationale.as_deref(),
        Some("judge missed the caveat")
    );
    assert_eq!(document.coverage.graded_assertions, 3);
    assert_eq!(document.coverage.labeled_assertions, 3);
    assert!(document.coverage.unlabeled.is_empty());
}

#[test]
fn an_unlabelled_scored_assertion_shows_up_as_coverage_not_agreement() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);

    write_grading(
        &report_dir,
        "run-001",
        vec![
            grade_result("assert a0", true, GraderKind::Mechanical),
            grade_result("assert a1", true, GraderKind::Llm),
        ],
    );

    record_assertion_verdict(&report_dir, "run-001", "case-a:g0", HumanVerdict::Pass, None, Some("h")).unwrap();

    let document = build_grader_agreement_document(&report_dir).unwrap();

    assert_eq!(document.coverage.graded_assertions, 2);
    assert_eq!(document.coverage.labeled_assertions, 1);
    assert_eq!(document.coverage.unlabeled.len(), 1);
    assert_eq!(document.coverage.unlabeled[0].assertion_id, "case-a:g1");
    assert_eq!(document.overall.agreements, 1);
    assert_eq!(document.overall.disagreements, 0);
}

#[test]
fn a_run_with_no_grading_json_contributes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);

    let document = build_grader_agreement_document(&report_dir).unwrap();

    assert_eq!(document.overall.agreements, 0);
    assert_eq!(document.overall.disagreements, 0);
    assert_eq!(document.coverage.graded_assertions, 0);
    assert!(document.overall.agreement_rate.is_none());
}

#[test]
fn a_threshold_outside_zero_to_one_is_refused() {
    assert!(AgreementThreshold::parse(0.0).is_err());
    assert!(AgreementThreshold::parse(-0.1).is_err());
    assert!(AgreementThreshold::parse(1.1).is_err());
    assert!(AgreementThreshold::parse(f64::NAN).is_err());
}

#[test]
fn a_threshold_is_parsed_from_the_command_line_form() {
    assert_eq!("0.9".parse::<AgreementThreshold>().unwrap().value(), 0.9);
    assert!("0".parse::<AgreementThreshold>().is_err());
    assert!("some".parse::<AgreementThreshold>().is_err());
}
