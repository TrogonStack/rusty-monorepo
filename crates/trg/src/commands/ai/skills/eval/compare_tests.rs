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
fn compare_warns_on_eval_suite_drift() {
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
            "compare",
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
    assert!(stderr.contains("removed eval IDs: case-b"));
}

#[test]
fn compare_allow_eval_suite_drift_suppresses_warning() {
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
            "compare",
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
    let warning = &json["warnings"][0];
    assert_eq!(warning["kind"], "eval_suite_drift");
    assert_eq!(warning["added_eval_ids"], serde_json::json!(["case-b"]));
    assert_eq!(warning["removed_eval_ids"], serde_json::json!(["case-a"]));
}

#[test]
fn compare_help_documents_allow_eval_suite_drift() {
    let help = CompareArgs::augment_args(clap::Command::new("compare").about("compare"))
        .render_long_help()
        .to_string();
    assert!(help.contains("--allow-eval-suite-drift"));
}

#[test]
fn collect_eval_suite_drift_warnings_empty_without_a_previous_report() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let report_dir = skill_root.join("report-iter-2");
    write_minimal_report(&report_dir, 2, "sha256:current", &["case-a"]);

    let warnings = collect_eval_suite_drift_warnings(&report_dir, None, false).unwrap();

    assert!(warnings.is_empty());
}

#[test]
fn collect_eval_suite_drift_warnings_detected_from_sibling_previous_report() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous = skill_root.join("report-iter-1");
    let current = skill_root.join("report-iter-2");
    write_minimal_report(&previous, 1, "sha256:1111", &["case-a", "case-b"]);
    write_minimal_report(&current, 2, "sha256:2222", &["case-a", "case-c"]);

    let warnings = collect_eval_suite_drift_warnings(&current, None, false).unwrap();

    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].added_eval_ids, vec!["case-c".to_string()]);
    assert_eq!(warnings[0].removed_eval_ids, vec!["case-b".to_string()]);
}
