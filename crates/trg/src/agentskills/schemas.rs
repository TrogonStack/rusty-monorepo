use std::path::Path;

use super::evals::{collect_named_files, EvalError, Result};
use super::feedback::FEEDBACK_FILE_NAME;
use super::validation::ValidationError;

pub const REPORT_SCHEMA: &str = include_str!("../../schemas/report.json.schema.json");
pub const GRADING_SCHEMA: &str = include_str!("../../schemas/grading.json.schema.json");
pub const BENCHMARK_SCHEMA: &str = include_str!("../../schemas/benchmark.json.schema.json");
pub const ITERATION_SUMMARY_SCHEMA: &str = include_str!("../../schemas/iteration-summary.json.schema.json");
pub const FEEDBACK_SCHEMA: &str = include_str!("../../schemas/feedback.json.schema.json");
pub const IMPROVEMENT_BUNDLE_SCHEMA: &str = include_str!("../../schemas/improvement-bundle.json.schema.json");
pub const COMPARISON_SCHEMA: &str = include_str!("../../schemas/comparison.json.schema.json");
pub const TIMING_SCHEMA: &str = include_str!("../../schemas/timing.json.schema.json");
pub const EVALS_SCHEMA: &str = include_str!("../../schemas/evals.json.schema.json");

pub fn validate_artifact(schema: &str, json: &serde_json::Value) -> Result<()> {
    #[cfg(any(feature = "schema-validation", test))]
    {
        let schema_value: serde_json::Value = serde_json::from_str(schema)?;
        let validator = jsonschema::validator_for(&schema_value)
            .map_err(|error| EvalError::Validation(ValidationError::for_field("schema", error.to_string()).into()))?;

        let errors: Vec<String> = validator.iter_errors(json).map(|error| error.to_string()).collect();

        if errors.is_empty() {
            return Ok(());
        }

        Err(EvalError::Validation(
            ValidationError::for_field("artifact", errors.join("; ")).into(),
        ))
    }

    #[cfg(not(any(feature = "schema-validation", test)))]
    {
        let _ = (schema, json);
        Ok(())
    }
}

pub fn validate_report_bundle_schemas(report_dir: &Path) -> Result<()> {
    let report_path = report_dir.join("report.json");
    if report_path.is_file() {
        let value = read_json_file(&report_path)?;
        validate_artifact(REPORT_SCHEMA, &value)?;
    }

    validate_named_artifacts(report_dir, "grading.json", GRADING_SCHEMA)?;
    validate_named_artifacts(report_dir, "timing.json", TIMING_SCHEMA)?;
    validate_named_artifacts(report_dir, FEEDBACK_FILE_NAME, FEEDBACK_SCHEMA)?;
    validate_named_artifacts(report_dir, "comparison.json", COMPARISON_SCHEMA)?;
    validate_benchmark_artifacts(report_dir)?;
    validate_iteration_summary_artifacts(report_dir)?;
    Ok(())
}

fn validate_named_artifacts(root: &Path, file_name: &str, schema: &str) -> Result<()> {
    let mut paths = Vec::new();
    collect_named_files(root, file_name, &mut paths)?;

    for path in paths {
        let value = read_json_file(&path)?;
        validate_artifact(schema, &value).map_err(|error| schema_error_for_path(&path, error))?;
    }

    Ok(())
}

fn validate_benchmark_artifacts(report_dir: &Path) -> Result<()> {
    let mut paths = Vec::new();
    collect_named_files(report_dir, "benchmark.json", &mut paths)?;

    for path in paths {
        let value = read_json_file(&path)?;
        if value.get("schema_version").is_none() {
            continue;
        }
        validate_artifact(BENCHMARK_SCHEMA, &value).map_err(|error| schema_error_for_path(&path, error))?;
    }

    Ok(())
}

fn validate_iteration_summary_artifacts(report_dir: &Path) -> Result<()> {
    let mut paths = Vec::new();
    collect_named_files(report_dir, "iteration-summary.json", &mut paths)?;

    for path in paths {
        let value = read_json_file(&path)?;
        if value.get("schema_version").is_none() {
            continue;
        }
        validate_artifact(ITERATION_SUMMARY_SCHEMA, &value).map_err(|error| schema_error_for_path(&path, error))?;
    }

    Ok(())
}

fn read_json_file(path: &Path) -> Result<serde_json::Value> {
    let content = std::fs::read_to_string(path).map_err(|source| {
        EvalError::Io(std::io::Error::new(
            source.kind(),
            format!("read {}: {source}", path.display()),
        ))
    })?;
    serde_json::from_str(&content).map_err(EvalError::from)
}

fn schema_error_for_path(path: &Path, error: EvalError) -> EvalError {
    match error {
        EvalError::Validation(errors) => {
            EvalError::Validation(ValidationError::for_field(path.display().to_string(), errors.to_string()).into())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::benchmark::{build_benchmark, BenchmarkOptions};
    use crate::agentskills::compare::{run_comparisons, CompareOptions, JudgeKind, ScenarioPair};
    use crate::agentskills::evals::{parse_eval_suite, scaffold_eval_suite};
    use crate::agentskills::feedback::{
        init_feedback, FeedbackCategory, FeedbackDocument, FeedbackNote, FeedbackSeverity,
    };
    use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
    use crate::agentskills::improvement_bundle::{
        build_improvement_bundle, testutil::sample_prior_iteration_fixture, NextIterationOptions,
    };
    use crate::agentskills::iteration_summary::{build_iteration_summary_document, IterationSummaryOptions};
    use crate::agentskills::report::{
        build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
    };
    use crate::agentskills::runner::{write_timing_file, EvalRunOutcome, RunStatus};
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
                        "assertions": ["assert a"]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "assertions": ["assert b"]
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
            assertion: "file exists".to_string(),
            passed: true,
            evidence: "outputs/report.md exists".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            rationale: None,
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
                total_tokens: Some(900),
                input_tokens: None,
                output_tokens: None,
                cost_usd: None,
                final_text: String::new(),
                exit_code: None,
                failure_kind: None,
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
        };

        let json = serde_json::to_value(&document).unwrap();
        validate_artifact(FEEDBACK_SCHEMA, &json).unwrap();
    }

    fn write_benchmark_fixture_report(report_dir: &Path) {
        std::fs::create_dir_all(report_dir).unwrap();
        let report = serde_json::json!({
            "schema_version": "trg.skills-eval.report.v1",
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
                "graders": []
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
  "schema_version": "trg.skills-eval.grading.v1",
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
        let mut report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
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
                        "assertions": ["assert a"]
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
}
