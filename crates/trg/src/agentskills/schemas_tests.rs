use super::*;
use crate::agentskills::benchmark::{build_benchmark, BenchmarkOptions};
use crate::agentskills::compare::{run_comparisons, CompareOptions, JudgeKind, ScenarioPair};
use crate::agentskills::evals::{parse_eval_suite, scaffold_eval_suite, SkillDisclosure};
use crate::agentskills::feedback::{init_feedback, FeedbackCategory, FeedbackDocument, FeedbackNote, FeedbackSeverity};
use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
use crate::agentskills::improvement_bundle::testutil::sample_prior_iteration_fixture;
use crate::agentskills::improvement_bundle::{build_improvement_bundle, NextIterationOptions};
use crate::agentskills::iteration_summary::{build_iteration_summary_document, IterationSummaryOptions};
use crate::agentskills::prompt::{SkillName, StagedSkillDir};
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::agentskills::runner::usage::HarnessTokenUsage;
use crate::agentskills::runner::{write_timing_file, EvalRunOutcome, RunStatus};
use crate::agentskills::transcript::{
    NormalizedTranscript, StagedSkill, StagedSkillName, ToolName, ToolVisibility, TranscriptEvent, WorkspaceEscape,
};
use crate::fs::testutil::MemFS;
use chrono::{SecondsFormat, Utc};
use std::path::Path;
use tempfile::tempdir;

const ALL_SCHEMAS: &[&str] = &[
    REPORT_SCHEMA,
    GRADING_SCHEMA,
    BENCHMARK_SCHEMA,
    ITERATION_SUMMARY_SCHEMA,
    FEEDBACK_SCHEMA,
    IMPROVEMENT_BUNDLE_SCHEMA,
    COMPARISON_SCHEMA,
    TIMING_SCHEMA,
    EVALS_SCHEMA,
    ENV_SCHEMA,
    EVENTS_SCHEMA,
];

#[test]
fn embedded_schemas_are_valid_json_schemas() {
    for schema in ALL_SCHEMAS {
        let schema_value: serde_json::Value = serde_json::from_str(schema).expect("schema JSON");
        jsonschema::validator_for(&schema_value).expect("schema meta-validation");
    }
}

fn sample_skill(fs: &MemFS) -> std::path::PathBuf {
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
    skill_path.to_path_buf()
}

#[test]
fn report_json_round_trip_validates() {
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions {
            report_id: Some("schema-test".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            iteration: Some(1),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let json = serde_json::to_value(&bundle.document).unwrap();
    validate_artifact(REPORT_SCHEMA, &json).unwrap();
}

#[test]
fn grading_json_round_trip_validates() {
    let grading = build_grading_file(vec![AssertionGradeResult {
        name: None,
        assertion: "file exists".to_string(),
        passed: true,
        evidence: "outputs/report.md exists".to_string(),
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
    }])
    .unwrap();

    let json = serde_json::to_value(&grading).unwrap();
    validate_artifact(GRADING_SCHEMA, &json).unwrap();
}

#[test]
fn timing_json_round_trip_validates() {
    let temp = tempdir().unwrap();
    let timing_path = temp.path().join("timing.json");
    write_timing_file(
        &timing_path,
        &EvalRunOutcome {
            status: RunStatus::Completed,
            duration_ms: 1500,
            tokens: HarnessTokenUsage::reported(Some(900), None, None),
            cost: None,
            final_text: String::new(),
            exit_code: None,
            failure_kind: None,
            read_only_fixture_violations: Vec::new(),
        },
    )
    .unwrap();

    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(timing_path).unwrap()).unwrap();
    validate_artifact(TIMING_SCHEMA, &json).unwrap();
}

#[test]
fn feedback_json_round_trip_validates() {
    let document = FeedbackDocument {
        reviewer: "reviewer@example.com".to_string(),
        reviewed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        notes: vec![FeedbackNote {
            severity: FeedbackSeverity::Warning,
            category: FeedbackCategory::Completeness,
            text: "Missing summary section".to_string(),
        }],
        assertion_verdicts: std::collections::BTreeMap::new(),
    };

    let json = serde_json::to_value(&document).unwrap();
    validate_artifact(FEEDBACK_SCHEMA, &json).unwrap();
}

#[test]
fn events_json_round_trip_validates() {
    let directory = StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Announced, "demo-skill")
        .expect("with-skill scenario stages a directory");
    let mut transcript = NormalizedTranscript::new(
        "claude-code",
        ToolVisibility::Observed,
        vec![
            TranscriptEvent::ToolCall {
                tool: ToolName::new("Read").unwrap(),
                paths: vec![".skill/SKILL.md".to_string()],
            },
            TranscriptEvent::Terminal { ok: true },
        ],
    )
    .staged_at(StagedSkill::At {
        directory,
        name: StagedSkillName::known(SkillName::parse("demo-skill").unwrap()),
    });
    transcript.workspace_escapes.push(WorkspaceEscape {
        tool: ToolName::new("Read").unwrap(),
        path: "/somewhere/outside/notes.md".to_string(),
    });

    let json = serde_json::to_value(&transcript).unwrap();
    validate_artifact(EVENTS_SCHEMA, &json).unwrap();
}

#[test]
fn events_json_bundle_validates() {
    let temp = tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

    let run = &bundle.document.runs[0];
    let run_dir = report_dir.join(&run.paths.workspace).parent().unwrap().to_path_buf();
    let transcript = NormalizedTranscript::new(
        "claude-code",
        ToolVisibility::Observed,
        vec![TranscriptEvent::Terminal { ok: true }],
    );
    std::fs::write(
        run_dir.join("events.json"),
        serde_json::to_string_pretty(&transcript).unwrap(),
    )
    .unwrap();

    validate_report_bundle_schemas(&report_dir).unwrap();
}

fn write_benchmark_fixture_report(report_dir: &Path) {
    std::fs::create_dir_all(report_dir).unwrap();
    let report = serde_json::json!({
        "report": {
            "id": "report-test",
            "generated_at": "2026-05-26T00:00:00Z",
            "iteration": 1,
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo",
            "skill_path": "demo",
            "skill_hash": "sha256:abc",
            "evals_path": "demo/evals/evals.json",
            "evals_hash": "sha256:def"
        },
        "dimensions": {
            "eval_cases": [],
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": [{
            "id": "run-001",
            "eval_case_id": "case-a",
            "eval_slug": "case-a",
            "scenario_id": "with_skill",
            "iteration": 1,
            "model_config_id": "ci-default",
            "skill_revision_id": "current",
            "attempt": 1,
            "status": "completed",
            "paths": {
                "workspace": "runs/run-001/workspace",
                "outputs": "runs/run-001/workspace/outputs"
            },
            "mirror_path": "iteration-1/case-a/with_skill/",
            "artifacts": [],
            "metrics": {}
        }],
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    std::fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();

    let run_dir = report_dir.join("runs/run-001");
    std::fs::create_dir_all(run_dir.join("workspace")).unwrap();
    std::fs::write(
        run_dir.join("grading.json"),
        r#"{
  "assertion_results": [
    { "assertion": "a", "passed": true, "evidence": "ok", "grader": { "kind": "mechanical" } }
  ],
  "summary": { "passed": 1, "failed": 0, "total": 1, "pass_rate": 1.0 }
}"#,
    )
    .unwrap();
    std::fs::write(
        run_dir.join("timing.json"),
        r#"{ "duration_ms": 1000, "total_tokens": 300 }"#,
    )
    .unwrap();
}

#[test]
fn iteration_summary_json_round_trip_validates() {
    let temp = tempdir().unwrap();
    write_benchmark_fixture_report(temp.path());

    let mut summary = build_iteration_summary_document(temp.path(), IterationSummaryOptions::default()).unwrap();
    summary.generated_at = "2026-05-26T12:00:00Z".to_string();
    let json = serde_json::to_value(&summary).unwrap();
    validate_artifact(ITERATION_SUMMARY_SCHEMA, &json).unwrap();
}

#[test]
fn benchmark_json_round_trip_validates() {
    let temp = tempdir().unwrap();
    write_benchmark_fixture_report(temp.path());

    let mut benchmark = build_benchmark(temp.path(), BenchmarkOptions::default()).unwrap();
    benchmark.generated_at = "2026-05-26T12:00:00Z".to_string();
    let json = serde_json::to_value(&benchmark).unwrap();
    validate_artifact(BENCHMARK_SCHEMA, &json).unwrap();
}

#[test]
fn comparison_json_round_trip_validates() {
    let temp = tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

    for run in &bundle.document.runs {
        let output_path = report_dir.join(&run.paths.workspace).join("output.md");
        std::fs::create_dir_all(output_path.parent().unwrap()).unwrap();
        std::fs::write(output_path, format!("{}-output", run.scenario_id.as_str())).unwrap();
    }

    let report_path = report_dir.join("report.json");
    let mut report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    for run in report["runs"].as_array_mut().unwrap() {
        run["status"] = serde_json::json!("completed");
    }
    std::fs::write(report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    std::fs::create_dir_all(report_dir.join("iteration-1").join("case-a")).unwrap();

    let judge_script = temp.path().join("judge.py");
    std::fs::write(
        &judge_script,
        r#"import json, sys
payload = json.load(sys.stdin)
print(json.dumps({"winner": "A", "evidence": "A is clearer"}))
"#,
    )
    .unwrap();

    run_comparisons(
        &report_dir,
        CompareOptions {
            pairs: vec![ScenarioPair {
                a: ScenarioKind::WithSkill,
                b: ScenarioKind::WithoutSkill,
            }],
            judge: JudgeKind::Script,
            judge_provider: crate::agentskills::judge::JudgeProvider::default(),
            judge_model: None,
            judge_command: Some(format!("python3 {}", judge_script.display())),
            emit_comparison_json: true,
        },
    )
    .unwrap();

    let comparison_path = report_dir.join("iteration-1").join("case-a").join("comparison.json");
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(comparison_path).unwrap()).unwrap();
    validate_artifact(COMPARISON_SCHEMA, &json).unwrap();
}

#[test]
fn validate_report_bundle_skips_placeholder_benchmark() {
    let temp = tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

    validate_report_bundle_schemas(&report_dir).unwrap();
}

#[test]
fn init_feedback_artifacts_validate() {
    let temp = tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

    init_feedback(&report_dir, Some("reviewer@example.com")).unwrap();
    validate_report_bundle_schemas(&report_dir).unwrap();
}

#[test]
fn evals_json_v2_manifest_validates_against_schema() {
    let suite = scaffold_eval_suite("demo-skill");
    let json = serde_json::to_value(&suite).unwrap();
    validate_artifact(EVALS_SCHEMA, &json).unwrap();

    let v1 = parse_eval_suite(
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "prompt": "prompt a long enough",
                        "expected_output": "output a long",
                        "graders": [{ "type": "contains", "text": "assert a" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    validate_artifact(EVALS_SCHEMA, &serde_json::to_value(v1).unwrap()).unwrap();
}

#[test]
fn improvement_bundle_json_round_trip_validates() {
    let temp = tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    let document = build_improvement_bundle(
        &report_dir,
        NextIterationOptions {
            skill_dir: Some(skill_root),
            ..NextIterationOptions::default()
        },
    )
    .unwrap();

    let json = serde_json::to_value(&document).unwrap();
    validate_artifact(IMPROVEMENT_BUNDLE_SCHEMA, &json).unwrap();
}
