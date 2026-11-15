use super::*;
use crate::agentskills::feedback::{
    FeedbackCategory, FeedbackDocument, FeedbackNote, FeedbackSeverity, FEEDBACK_FILE_NAME,
};
use crate::agentskills::grading::{AssertionGradeResult, GraderInfo, GraderKind, GradingSummary};
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::fs::testutil::MemFS;
use crate::fs::FileSystem;
use std::path::Path;

fn write_grading(run_dir: &Path, assertion: &str, passed: bool, evidence: &str) {
    std::fs::create_dir_all(run_dir).unwrap();
    let grading = GradingFile {
        assertion_results: vec![AssertionGradeResult {
            name: None,
            assertion: assertion.to_string(),
            passed,
            evidence: evidence.to_string(),
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
            passed: usize::from(passed),
            failed: usize::from(!passed),
            total: 1,
            unsupported: 0,
            excluded: 0,
            ungraded: 0,
            pass_rate: Some(if passed { 1.0 } else { 0.0 }),
        },
    };
    std::fs::write(
        run_dir.join("grading.json"),
        serde_json::to_string_pretty(&grading).unwrap(),
    )
    .unwrap();
}

pub fn write_feedback(run_dir: &Path, text: &str) {
    let document = FeedbackDocument {
        reviewer: "reviewer@example.com".to_string(),
        reviewed_at: "2026-05-26T12:00:00Z".to_string(),
        notes: vec![FeedbackNote {
            severity: FeedbackSeverity::Warning,
            category: FeedbackCategory::Correctness,
            text: text.to_string(),
        }],
        assertion_verdicts: std::collections::BTreeMap::new(),
    };
    std::fs::write(
        run_dir.join(FEEDBACK_FILE_NAME),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();
}

pub fn write_empty_feedback(run_dir: &Path) {
    let document = FeedbackDocument {
        reviewer: "reviewer@example.com".to_string(),
        reviewed_at: "2026-05-26T12:00:00Z".to_string(),
        notes: Vec::new(),
        assertion_verdicts: std::collections::BTreeMap::new(),
    };
    std::fs::write(
        run_dir.join(FEEDBACK_FILE_NAME),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();
}

fn write_transcript(run_dir: &Path, lines: usize) {
    let content: String = (0..lines).map(|index| format!("line-{index}\n")).collect();
    std::fs::write(run_dir.join("transcript.jsonl"), content).unwrap();
}

pub fn sample_prior_iteration_fixture(temp: &tempfile::TempDir, skill_root: &Path) -> PathBuf {
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
                    },
                    {
                        "id": "case-c-held-out",
                        "prompt": "TESTLEAK-PROMPT-C",
                        "expected_output": "output c",
                        "graders": [{ "type": "contains", "text": "assert c" }],
                        "split": "test"
                    }
                ]
            }"#,
    );

    let bundle = build_report_bundle(
        &fs,
        skill_path,
        skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions {
            report_id: Some("report-iter-1".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            iteration: Some(1),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

    write_grading(
        &report_dir.join("runs/run-001"),
        "assert a",
        false,
        "missing file out.json",
    );
    write_grading(
        &report_dir.join("runs/run-002"),
        "assert a",
        true,
        "file exists at outputs/out.json",
    );
    write_grading(
        &report_dir.join("runs/run-003"),
        "assert b",
        false,
        "expected summary section",
    );
    write_grading(
        &report_dir.join("runs/run-004"),
        "assert b",
        true,
        "found summary section in outputs/report.md",
    );

    write_feedback(
        &report_dir.join("runs/run-001"),
        "Add explicit output path guidance for case-a",
    );
    write_transcript(&report_dir.join("runs/run-001"), 450);
    write_transcript(&report_dir.join("runs/run-003"), 10);

    write_grading(
        &report_dir.join("runs/run-005"),
        "assert c",
        false,
        "TESTLEAK-EVIDENCE-C",
    );
    write_grading(&report_dir.join("runs/run-006"), "assert c", true, "found assert c");
    write_feedback(&report_dir.join("runs/run-005"), "TESTLEAK-FEEDBACK-NOTE");
    std::fs::create_dir_all(report_dir.join("runs/run-005")).unwrap();
    std::fs::write(
        report_dir.join("runs/run-005/transcript.jsonl"),
        "TESTLEAK-TRANSCRIPT-MARKER\n",
    )
    .unwrap();

    std::fs::create_dir_all(skill_root.join("evals")).unwrap();
    std::fs::write(
        skill_root.join("evals/evals.json"),
        fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        skill_root.join("SKILL.md"),
        fs.read_to_string(&skill_path.join("SKILL.md")).unwrap(),
    )
    .unwrap();

    report_dir
}
