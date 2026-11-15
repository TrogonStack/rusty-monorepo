use super::*;
use assert_cmd::Command;
use std::fs;

fn write_minimal_report(dir: &std::path::Path, iteration: u32, evals_hash: &str, eval_ids: &[&str]) {
    fs::create_dir_all(dir).unwrap();
    let eval_cases: Vec<serde_json::Value> = eval_ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "slug": id,
                "prompt": "p",
                "expected_output": "o",
                "files": [],
                "assertion_ids": []
            })
        })
        .collect();
    let report = serde_json::json!({
        "report": {
            "id": format!("report-iter-{iteration}"),
            "generated_at": "2026-05-26T12:00:00Z",
            "iteration": iteration,
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo-skill",
            "skill_path": "demo-skill",
            "skill_hash": "sha256:abc",
            "evals_path": "demo-skill/evals/evals.json",
            "evals_hash": evals_hash
        },
        "dimensions": {
            "eval_cases": eval_cases,
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": [],
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    fs::write(dir.join("report.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
}

#[test]
fn benchmark_warns_on_eval_suite_drift() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous = skill_root.join("report-iter-1");
    let current = skill_root.join("report-iter-2");
    write_minimal_report(&previous, 1, "sha256:1111", &["case-a", "case-b"]);
    write_minimal_report(&current, 2, "sha256:2222", &["case-a", "case-c"]);

    let output = Command::cargo_bin("trg")
        .unwrap()
        .args([
            "ai",
            "skills",
            "eval",
            "benchmark",
            "--report-dir",
            current.to_str().unwrap(),
            "--previous-report-dir",
            previous.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WARN: eval suite changed between iterations"));
    assert!(stderr.contains("added eval IDs: case-c"));
}

#[test]
fn benchmark_allow_eval_suite_drift_suppresses_warning() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous = skill_root.join("report-iter-1");
    let current = skill_root.join("report-iter-2");
    write_minimal_report(&previous, 1, "sha256:1111", &["case-a"]);
    write_minimal_report(&current, 2, "sha256:2222", &["case-b"]);

    let output = Command::cargo_bin("trg")
        .unwrap()
        .args([
            "ai",
            "skills",
            "eval",
            "benchmark",
            "--report-dir",
            current.to_str().unwrap(),
            "--previous-report-dir",
            previous.to_str().unwrap(),
            "--allow-eval-suite-drift",
            "--output-format",
            "json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("WARN: eval suite changed between iterations"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["warnings"][0]["kind"], "eval_suite_drift");
}

#[test]
fn benchmark_help_documents_allow_eval_suite_drift() {
    let help = BenchmarkArgs::augment_args(clap::Command::new("benchmark").about("benchmark"))
        .render_long_help()
        .to_string();
    assert!(help.contains("--allow-eval-suite-drift"));
}

fn phase_names<'a>(trace: &'a crate::telemetry::testing::CapturedTrace, parent: &str) -> Vec<&'a str> {
    let parent = trace.span(parent).unwrap_or_else(|| panic!("no span named {parent:?}"));
    trace
        .children_of(parent)
        .iter()
        .map(|span| span.name.as_ref())
        .collect()
}

#[test]
fn benchmarking_a_report_nests_every_phase_under_the_benchmark_span() {
    let root = tempfile::tempdir().unwrap();
    let report_dir = root.path().join("demo-skill").join("report-iter-1");
    write_minimal_report(&report_dir, 1, "sha256:1111", &["case-a"]);

    let ((code, _), trace) = crate::telemetry::testing::capture(|| {
        benchmark_report_dir_with_document(&report_dir, BenchmarkOptions::default())
    });

    assert!(code.is_success());
    assert_eq!(
        phase_names(&trace, "benchmark"),
        vec![
            "read report",
            "aggregate scenarios",
            "check eval suite drift",
            "build headroom",
            "write benchmark"
        ]
    );
    let benchmark = trace.span("benchmark").unwrap();
    assert_eq!(benchmark.status, opentelemetry::trace::Status::Unset);
}

#[test]
fn a_report_that_cannot_be_read_fails_the_read_phase_and_the_benchmark() {
    let root = tempfile::tempdir().unwrap();

    let ((code, _), trace) = crate::telemetry::testing::capture(|| {
        benchmark_report_dir_with_document(root.path(), BenchmarkOptions::default())
    });

    assert_eq!(code, ExitCode::InfrastructureFailure);
    assert_eq!(phase_names(&trace, "benchmark"), vec!["read report"]);
    for span in ["read report", "benchmark"] {
        assert_eq!(
            trace.attribute(span, opentelemetry_semantic_conventions::attribute::ERROR_TYPE),
            Some(opentelemetry::Value::from("io")),
            "{span}"
        );
        assert!(
            matches!(
                trace.span(span).unwrap().status,
                opentelemetry::trace::Status::Error { .. }
            ),
            "{span}"
        );
    }
}
