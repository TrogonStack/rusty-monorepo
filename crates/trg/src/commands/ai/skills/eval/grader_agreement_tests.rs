use super::*;
use crate::agentskills::feedback::{record_assertion_verdict, HumanVerdict};
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
                        "graders": [{ "type": "llm", "criterion": "assert a" }]
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

fn grade_run(report_dir: &Path, run_id: &str, passed: bool) {
    let result = AssertionGradeResult {
        assertion: "assert a".to_string(),
        passed,
        evidence: "the transcript said so".to_string(),
        grader: GraderInfo {
            kind: GraderKind::Llm,
            model: Some("test-model".to_string()),
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
fn command_writes_the_artifact_and_passes_without_a_gate() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_run(&report_dir, "run-001", true);
    record_assertion_verdict(&report_dir, "run-001", "case-a:g0", HumanVerdict::Pass, None, Some("h")).unwrap();

    let status = GraderAgreementArgs {
        report_dir: report_dir.clone(),
        min_agreement: None,
        output_format: OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::Success);
    assert!(report_dir.join("grader-agreement.json").is_file());
}

#[test]
fn a_gate_with_no_llm_labels_yet_fails_rather_than_passing_silently() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_run(&report_dir, "run-001", true);

    let status = GraderAgreementArgs {
        report_dir,
        min_agreement: Some(AgreementThreshold::parse(0.9).unwrap()),
        output_format: OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::GateFailed);
}

#[test]
fn a_confidently_wrong_judge_fails_the_gate() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_run(&report_dir, "run-001", true);
    record_assertion_verdict(&report_dir, "run-001", "case-a:g0", HumanVerdict::Fail, None, Some("h")).unwrap();

    let status = GraderAgreementArgs {
        report_dir,
        min_agreement: Some(AgreementThreshold::parse(0.5).unwrap()),
        output_format: OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::GateFailed);
}

#[test]
fn grader_agreement_nests_loading_collecting_and_writing_under_one_span() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_run(&report_dir, "run-001", true);
    record_assertion_verdict(&report_dir, "run-001", "case-a:g0", HumanVerdict::Pass, None, Some("h")).unwrap();

    let (status, trace) = crate::telemetry::testing::capture(|| {
        GraderAgreementArgs {
            report_dir: report_dir.clone(),
            min_agreement: None,
            output_format: OutputFormat::Json,
        }
        .handle(&crate::fs::RealFS)
    });

    assert_eq!(status, ExitCode::Success);
    let root = trace.span("grader agreement").expect("the command has its own span");
    let phases: Vec<&str> = trace.children_of(root).iter().map(|span| span.name.as_ref()).collect();
    assert_eq!(
        phases,
        vec!["load report", "collect verdicts", "write grader agreement"]
    );
    assert_eq!(root.status, opentelemetry::trace::Status::Unset);
}

#[test]
fn a_failed_gate_marks_the_grader_agreement_span_failed() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);
    grade_run(&report_dir, "run-001", true);

    let (status, trace) = crate::telemetry::testing::capture(|| {
        GraderAgreementArgs {
            report_dir,
            min_agreement: Some(AgreementThreshold::parse(0.9).unwrap()),
            output_format: OutputFormat::Json,
        }
        .handle(&crate::fs::RealFS)
    });

    assert_eq!(status, ExitCode::GateFailed);
    assert_eq!(
        trace.attribute(
            "grader agreement",
            opentelemetry_semantic_conventions::attribute::ERROR_TYPE
        ),
        Some(opentelemetry::Value::from(GATE_FAILED))
    );
    assert_eq!(
        trace.attribute(
            "write grader agreement",
            opentelemetry_semantic_conventions::attribute::ERROR_TYPE
        ),
        None,
        "the artifact was written before the gate was checked"
    );
}
