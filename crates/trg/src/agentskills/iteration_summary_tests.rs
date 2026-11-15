use super::*;
use crate::agentskills::efficiency::{
    CostVerdict, DurationVerdict, EfficiencyMetric, IncreaseTolerance, TokenVerdict, UnmeasuredReason,
};
use std::fs;

fn write_report(report_dir: &Path, runs: serde_json::Value, iteration: u32, report_id: &str) {
    write_report_with_hash(report_dir, runs, iteration, report_id, "sha256:def");
}

fn write_report_with_hash(
    report_dir: &Path,
    runs: serde_json::Value,
    iteration: u32,
    report_id: &str,
    evals_hash: &str,
) {
    fs::create_dir_all(report_dir).unwrap();
    let report = serde_json::json!({
        "report": {
            "id": report_id,
            "generated_at": "2026-05-26T00:00:00Z",
            "iteration": iteration,
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo",
            "skill_path": "demo",
            "skill_hash": "sha256:abc",
            "evals_path": "demo/evals/evals.json",
            "evals_hash": evals_hash
        },
        "dimensions": {
            "eval_cases": [],
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": runs,
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

fn write_run_artifacts(report_dir: &Path, run_id: &str, grading: Option<&str>, timing: Option<&str>) {
    let run_dir = report_dir.join(format!("runs/{run_id}"));
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    if let Some(grading) = grading {
        fs::write(run_dir.join("grading.json"), grading).unwrap();
    }
    if let Some(timing) = timing {
        fs::write(run_dir.join("timing.json"), timing).unwrap();
    }
}

fn sample_run(id: &str, eval_case_id: &str, scenario: &str, attempt: u32, status: &str) -> serde_json::Value {
    sample_run_with_split(id, eval_case_id, scenario, attempt, status, "train")
}

fn sample_run_with_split(
    id: &str,
    eval_case_id: &str,
    scenario: &str,
    attempt: u32,
    status: &str,
    split: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "eval_case_id": eval_case_id,
        "split": split,
        "scenario_id": scenario,
        "model_config_id": "ci-default",
        "skill_revision_id": "current",
        "attempt": attempt,
        "status": status,
        "paths": { "workspace": format!("runs/{id}/workspace") },
        "artifacts": [],
        "metrics": {}
    })
}

#[test]
fn tallies_tool_usage_per_scenario_from_measured_runs() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    let measured = |id: &str, scenario: &str, calls: u64, bytes: u64| {
        let mut run = sample_run(id, "case-a", scenario, 1, "completed");
        run["metrics"] = serde_json::json!({
            "transcript": {
                "turns": 3,
                "tool_calls": calls,
                "repeated_tool_calls": 1,
                "tools": [{ "tool": "Grep", "calls": calls, "repeated_calls": 1, "largest_result_bytes": bytes }]
            }
        });
        run
    };
    write_report(
        &report_dir,
        serde_json::json!([
            measured("run-001", "with_skill", 2, 155_000),
            measured("run-002", "with_skill", 3, 10),
            sample_run("run-003", "case-a", "without_skill", 1, "completed"),
        ]),
        1,
        "report-a",
    );

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

    assert_eq!(summary.tool_usage.len(), 1);
    let usage = &summary.tool_usage[0];
    assert_eq!(usage.scenario, ScenarioKind::WithSkill);
    assert_eq!(usage.measured_runs, RunCount::new(2));
    assert_eq!(usage.tools[0].tool, "Grep");
    assert_eq!(usage.tools[0].calls.get(), 5);
    assert_eq!(usage.tools[0].repeated_calls.get(), 2);
    assert_eq!(
        usage.tools[0].largest_result_bytes.map(|bytes| bytes.get()),
        Some(155_000)
    );
    assert_eq!(
        tool_usage_row(usage.scenario, &usage.tools[0]),
        "with_skill | Grep: 5 calls in 2 runs, 2 repeated, largest result 155000 bytes"
    );
}

#[test]
fn detects_always_pass_assertions() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "without_skill", 1, "completed"),
        ]),
        1,
        "report-a",
    );
    write_run_artifacts(
        &report_dir,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"stable pass","passed":true}]}"#),
        None,
    );
    write_run_artifacts(
        &report_dir,
        "run-002",
        Some(r#"{"assertion_results":[{"assertion":"stable pass","passed":true}]}"#),
        None,
    );

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

    assert_eq!(summary.always_pass.len(), 1);
    assert_eq!(summary.always_pass[0].eval_case_id, "case-a");
    assert_eq!(summary.always_pass[0].assertion, "stable pass");
    assert_eq!(summary.always_pass[0].attempts_observed, 2);
}

#[test]
fn detects_always_fail_assertions() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "without_skill", 2, "completed"),
        ]),
        1,
        "report-a",
    );
    write_run_artifacts(
        &report_dir,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"always broken","passed":false}]}"#),
        None,
    );
    write_run_artifacts(
        &report_dir,
        "run-002",
        Some(r#"{"assertion_results":[{"assertion":"always broken","passed":false}]}"#),
        None,
    );

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

    assert_eq!(summary.always_fail.len(), 1);
    assert_eq!(summary.always_fail[0].attempts_observed, 2);
}

#[test]
fn detects_helped_by_skill_assertions() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "without_skill", 1, "completed"),
        ]),
        1,
        "report-a",
    );
    write_run_artifacts(
        &report_dir,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"skill helps","passed":true}]}"#),
        None,
    );
    write_run_artifacts(
        &report_dir,
        "run-002",
        Some(r#"{"assertion_results":[{"assertion":"skill helps","passed":false}]}"#),
        None,
    );

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

    assert_eq!(summary.helped_by_skill.len(), 1);
    assert_eq!(summary.helped_by_skill[0].with_skill_pass_rate, 1.0);
    assert_eq!(summary.helped_by_skill[0].without_skill_pass_rate, 0.0);
    assert!((summary.helped_by_skill[0].delta - 1.0).abs() < 0.0001);
    assert_eq!(summary.helped_by_skill[0].with_skill_attempts, 1);
    assert_eq!(summary.helped_by_skill[0].without_skill_attempts, 1);
}

#[test]
fn detects_flaky_assertions_across_attempts() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "with_skill", 2, "completed"),
        ]),
        1,
        "report-a",
    );
    write_run_artifacts(
        &report_dir,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"flaky check","passed":true}]}"#),
        None,
    );
    write_run_artifacts(
        &report_dir,
        "run-002",
        Some(r#"{"assertion_results":[{"assertion":"flaky check","passed":false}]}"#),
        None,
    );

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

    assert_eq!(summary.flaky_assertions.len(), 1);
    assert_eq!(summary.flaky_assertions[0].attempts, 2);
    assert_eq!(summary.flaky_assertions[0].pass_count, 1);
    assert!((summary.flaky_assertions[0].flakiness_ratio.value() - 0.5).abs() < 0.0001);
}

#[test]
fn outlier_detection_requires_at_least_four_samples() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "with_skill", 2, "completed"),
            sample_run("run-003", "case-a", "with_skill", 3, "completed"),
        ]),
        1,
        "report-a",
    );
    for (run_id, duration_ms, tokens) in [("run-001", 1000, 100), ("run-002", 1100, 110), ("run-003", 9000, 900)] {
        write_run_artifacts(
            &report_dir,
            run_id,
            None,
            Some(&format!(
                r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
            )),
        );
    }

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
    assert!(summary.timing_outliers.is_empty());
    assert!(summary.token_outliers.is_empty());

    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "with_skill", 2, "completed"),
            sample_run("run-003", "case-a", "with_skill", 3, "completed"),
            sample_run("run-004", "case-a", "with_skill", 4, "completed"),
            sample_run("run-005", "case-a", "with_skill", 5, "completed"),
            sample_run("run-006", "case-a", "with_skill", 6, "completed"),
            sample_run("run-007", "case-a", "with_skill", 7, "completed"),
            sample_run("run-008", "case-a", "with_skill", 8, "completed"),
        ]),
        1,
        "report-a",
    );
    for (run_id, duration_ms, tokens) in [
        ("run-001", 1000, 1000),
        ("run-002", 1010, 1010),
        ("run-003", 990, 990),
        ("run-004", 1005, 1005),
        ("run-005", 995, 995),
        ("run-006", 1000, 1000),
        ("run-007", 1010, 1010),
        ("run-008", 50000, 50000),
    ] {
        write_run_artifacts(
            &report_dir,
            run_id,
            None,
            Some(&format!(
                r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
            )),
        );
    }

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
    assert_eq!(summary.timing_outliers.len(), 1);
    assert_eq!(summary.timing_outliers[0].attempt, 8);
    assert_eq!(summary.timing_outliers[0].duration_ms, 50000);
    assert_eq!(summary.timing_outliers[0].median_ms, 1002.5);
    assert_eq!(summary.timing_outliers[0].mad_ms, 7.5);
    assert_eq!(summary.token_outliers.len(), 1);
    assert_eq!(summary.token_outliers[0].total_tokens, 50000);
}

/// An arm whose runs mostly landed on the same number has a median absolute
/// deviation of zero, and a band of width zero calls every remaining value unusual.
#[test]
fn an_arm_whose_runs_mostly_agree_exactly_reports_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "with_skill", 2, "completed"),
            sample_run("run-003", "case-a", "with_skill", 3, "completed"),
            sample_run("run-004", "case-a", "with_skill", 4, "completed"),
            sample_run("run-005", "case-a", "with_skill", 5, "completed"),
            sample_run("run-006", "case-a", "with_skill", 6, "completed"),
            sample_run("run-007", "case-a", "with_skill", 7, "completed"),
            sample_run("run-008", "case-a", "with_skill", 8, "completed"),
        ]),
        1,
        "report-a",
    );
    for (run_id, duration_ms, tokens) in [
        ("run-001", 1000, 1000),
        ("run-002", 1000, 1000),
        ("run-003", 1000, 1000),
        ("run-004", 1000, 1000),
        ("run-005", 1000, 1000),
        ("run-006", 1000, 1000),
        ("run-007", 1000, 1000),
        ("run-008", 50000, 50000),
    ] {
        write_run_artifacts(
            &report_dir,
            run_id,
            None,
            Some(&format!(
                r#"{{"duration_ms": {duration_ms}, "total_tokens": {tokens}}}"#
            )),
        );
    }

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
    assert!(summary.timing_outliers.is_empty());
    assert!(summary.token_outliers.is_empty());
}

#[test]
fn an_explicit_previous_that_cannot_be_read_fails_the_summary_instead_of_comparing_against_nothing() {
    let root = tempfile::tempdir().unwrap();
    let current = root.path().join("report-iter-2");
    let missing_previous = root.path().join("report-iter-1");

    write_report(
        &current,
        serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
        2,
        "report-iter-2",
    );
    write_run_artifacts(
        &current,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
        None,
    );

    let result = build_iteration_summary_document(
        &current,
        IterationSummaryOptions {
            previous_report_dir: Some(missing_previous),
            ..IterationSummaryOptions::default()
        },
    );

    assert!(
            result.is_err(),
            "a named --previous-report-dir that cannot be read must fail the summary, not be silently treated as no previous at all"
        );
}

#[test]
fn cross_iteration_deltas_surface_newly_stable_assertions() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous = skill_root.join("report-iter-1");
    let current = skill_root.join("report-iter-2");

    write_report(
        &previous,
        serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
        1,
        "report-iter-1",
    );
    write_run_artifacts(
        &previous,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"now stable","passed":false}]}"#),
        None,
    );

    write_report(
        &current,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "without_skill", 1, "completed"),
        ]),
        2,
        "report-iter-2",
    );
    for run_id in ["run-001", "run-002"] {
        write_run_artifacts(
            &current,
            run_id,
            Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
            None,
        );
    }

    let summary = build_iteration_summary_document(
        &current,
        IterationSummaryOptions {
            previous_report_dir: Some(previous),
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();

    let cross = summary.cross_iteration.as_ref().unwrap();
    assert_eq!(cross.newly_always_pass.len(), 1);
    assert_eq!(
        summary.always_pass[0].cross_iteration_delta,
        Some(CrossIterationDelta::New)
    );
}

#[test]
fn cross_iteration_omitted_when_previous_grading_vanishes_after_detection() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous = skill_root.join("report-iter-1");
    let current = skill_root.join("report-iter-2");

    write_report(
        &previous,
        serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
        1,
        "report-iter-1",
    );
    write_run_artifacts(
        &previous,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"now stable","passed":false}]}"#),
        None,
    );

    write_report(
        &current,
        serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
        2,
        "report-iter-2",
    );
    write_run_artifacts(
        &current,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
        None,
    );

    // Detection reads report.json while the previous iteration's directory still
    // exists, exactly as `resolve_previous_report_for_summary` does.
    let previous_report = load_report(&previous).unwrap();

    // The previous iteration's directory is removed after detection but before the
    // grading artifacts underneath it are read for analysis.
    fs::remove_dir_all(&previous).unwrap();

    let current_report = load_report(&current).unwrap();
    let current_analysis = analyze_report(&current, &current_report, FailedRunsMode::default());

    let cross_iteration =
        build_cross_iteration_section(&previous, previous_report, &current_analysis, FailedRunsMode::default());

    assert!(
        cross_iteration.is_none(),
        "a previous iteration whose grading could not be read must not be compared against"
    );
}

#[test]
fn cross_iteration_present_when_previous_iteration_scored_nothing() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous = skill_root.join("report-iter-1");
    let current = skill_root.join("report-iter-2");

    write_report(
        &previous,
        serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
        1,
        "report-iter-1",
    );
    // The grading artifact is present and reads fine; it simply scored nothing.
    write_run_artifacts(&previous, "run-001", Some(r#"{"assertion_results":[]}"#), None);

    write_report(
        &current,
        serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
        2,
        "report-iter-2",
    );
    write_run_artifacts(
        &current,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"now stable","passed":true}]}"#),
        None,
    );

    let summary = build_iteration_summary_document(
        &current,
        IterationSummaryOptions {
            previous_report_dir: Some(previous),
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();

    let cross = summary
        .cross_iteration
        .as_ref()
        .expect("a readable but empty previous iteration must still be compared against");
    assert_eq!(cross.newly_always_pass.len(), 1);
}

#[test]
fn detect_previous_report_dir_finds_matching_sibling() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    write_report(&previous_dir, serde_json::json!([]), 1, "report-iter-1");
    write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

    let previous = detect_previous_report_dir(&current_dir, 2).expect("a matching sibling exists");

    assert_eq!(previous.dir(), previous_dir.as_path());
    let report = previous.report_for_summary().unwrap();
    assert_eq!(report.report.id, "report-iter-1");
    assert_eq!(report.report.iteration, 1);
}

#[test]
fn detect_previous_report_dir_skips_unparseable_candidates() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let broken_dir = skill_root.join("report-broken");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    std::fs::create_dir_all(&broken_dir).unwrap();
    std::fs::write(broken_dir.join("report.json"), "not json").unwrap();

    write_report(&previous_dir, serde_json::json!([]), 1, "report-iter-1");
    write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

    let previous = detect_previous_report_dir(&current_dir, 2).expect("the valid sibling is still found");
    assert_eq!(previous.dir(), previous_dir.as_path());
}

#[test]
fn detect_previous_report_dir_returns_none_when_no_sibling_matches() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let current_dir = skill_root.join("report-iter-2");

    write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

    assert!(detect_previous_report_dir(&current_dir, 2).is_none());
}

#[test]
fn detect_previous_report_dir_returns_none_for_a_first_iteration() {
    let root = tempfile::tempdir().unwrap();
    let report_dir = root.path().join("report");
    write_report(&report_dir, serde_json::json!([]), 1, "report-a");

    assert!(detect_previous_report_dir(&report_dir, 1).is_none());
}

#[test]
fn previous_report_projection_survives_deletion_of_the_directory() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    write_report(
        &previous_dir,
        serde_json::json!([sample_run("run-001", "case-a", "with_skill", 1, "completed")]),
        1,
        "report-iter-1",
    );
    write_report(&current_dir, serde_json::json!([]), 2, "report-iter-2");

    let previous = detect_previous_report_dir(&current_dir, 2).expect("a matching sibling exists");

    std::fs::remove_dir_all(&previous_dir).unwrap();
    assert!(!previous_dir.exists());

    let report = previous
        .report_for_summary()
        .expect("the report was already read during detection");
    assert_eq!(report.report.id, "report-iter-1");
    assert_eq!(report.report.iteration, 1);

    let snapshot = previous
        .drift_snapshot()
        .expect("the drift snapshot projects from the same in-memory document");
    assert_eq!(snapshot.iteration, 1);
}

#[test]
fn iteration_summary_json_matches_schema() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_report(
        &report_dir,
        serde_json::json!([
            sample_run("run-001", "case-a", "with_skill", 1, "completed"),
            sample_run("run-002", "case-a", "without_skill", 1, "completed"),
        ]),
        1,
        "report-a",
    );
    write_run_artifacts(
        &report_dir,
        "run-001",
        Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
        Some(r#"{ "duration_ms": 1000, "total_tokens": 100 }"#),
    );
    write_run_artifacts(
        &report_dir,
        "run-002",
        Some(r#"{"assertion_results":[{"assertion":"a","passed":false}]}"#),
        Some(r#"{ "duration_ms": 2000, "total_tokens": 200 }"#),
    );

    let mut summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
    summary.generated_at = "2026-05-26T12:00:00Z".to_string();

    let json = serde_json::to_value(&summary).unwrap();
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/iteration-summary.json.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&json).map(|error| error.to_string()).collect();
    assert!(errors.is_empty(), "schema errors: {errors:?}");
}

fn write_with_skill_runs(
    report_dir: &Path,
    id_prefix: &str,
    eval_case_id: &str,
    split: &str,
    passed_count: usize,
    failed_count: usize,
) -> Vec<serde_json::Value> {
    let mut runs = Vec::new();
    let mut attempt = 1u32;
    for i in 0..passed_count {
        let id = format!("{id_prefix}-pass-{i}");
        runs.push(sample_run_with_split(
            &id,
            eval_case_id,
            "with_skill",
            attempt,
            "completed",
            split,
        ));
        write_run_artifacts(
            report_dir,
            &id,
            Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
            None,
        );
        attempt += 1;
    }
    for i in 0..failed_count {
        let id = format!("{id_prefix}-fail-{i}");
        runs.push(sample_run_with_split(
            &id,
            eval_case_id,
            "with_skill",
            attempt,
            "completed",
            split,
        ));
        write_run_artifacts(
            report_dir,
            &id,
            Some(r#"{"assertion_results":[{"assertion":"a","passed":false}]}"#),
            None,
        );
        attempt += 1;
    }
    runs
}

fn build_summary_against_previous(
    previous_dir: &Path,
    previous_runs: Vec<serde_json::Value>,
    current_dir: &Path,
    current_runs: Vec<serde_json::Value>,
) -> IterationSummaryDocument {
    write_report(
        previous_dir,
        serde_json::Value::Array(previous_runs),
        1,
        "report-iter-1",
    );
    write_report(current_dir, serde_json::Value::Array(current_runs), 2, "report-iter-2");

    build_iteration_summary_document(
        current_dir,
        IterationSummaryOptions {
            previous_report_dir: Some(previous_dir.to_path_buf()),
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn a_test_split_improvement_recommends_keep() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 0, 40);
    let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 40, 0);

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert_eq!(keep_or_revert.suites, SuiteComparison::SameSuite);
    assert!(matches!(
        keep_or_revert.by_split.get(&EvalSplit::Test),
        Some(SplitVerdict::Improved(_))
    ));
    assert_eq!(keep_or_revert.recommendation, Recommendation::Keep);
}

#[test]
fn a_test_split_regression_recommends_revert_even_if_train_also_improved() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let mut previous_runs = write_with_skill_runs(&previous_dir, "prev-test", "case-a", "test", 40, 0);
    previous_runs.extend(write_with_skill_runs(
        &previous_dir,
        "prev-train",
        "case-b",
        "train",
        0,
        40,
    ));
    let mut current_runs = write_with_skill_runs(&current_dir, "cur-test", "case-a", "test", 0, 40);
    current_runs.extend(write_with_skill_runs(
        &current_dir,
        "cur-train",
        "case-b",
        "train",
        40,
        0,
    ));

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert!(matches!(
        keep_or_revert.by_split.get(&EvalSplit::Test),
        Some(SplitVerdict::Regressed(_))
    ));
    assert!(matches!(
        keep_or_revert.by_split.get(&EvalSplit::Train),
        Some(SplitVerdict::Improved(_))
    ));
    assert_eq!(
        keep_or_revert.recommendation,
        Recommendation::Revert,
        "a held-out regression is reason enough to revert on its own"
    );
}

fn write_with_skill_attempt(
    report_dir: &Path,
    id: &str,
    eval_case_id: &str,
    split: &str,
    attempt: u32,
    passed: bool,
) -> serde_json::Value {
    let run = sample_run_with_split(id, eval_case_id, "with_skill", attempt, "completed", split);
    write_run_artifacts(
        report_dir,
        id,
        Some(&format!(
            r#"{{"assertion_results":[{{"assertion":"a","passed":{passed}}}]}}"#
        )),
        None,
    );
    run
}

#[test]
fn pass_at_k_and_pass_pow_k_separate_best_of_n_from_the_matched_aggregate() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = vec![
        write_with_skill_attempt(&previous_dir, "prev-a-1", "case-a", "test", 1, false),
        write_with_skill_attempt(&previous_dir, "prev-a-2", "case-a", "test", 2, false),
        write_with_skill_attempt(&previous_dir, "prev-b-1", "case-b", "test", 1, false),
        write_with_skill_attempt(&previous_dir, "prev-b-2", "case-b", "test", 2, false),
    ];
    let current_runs = vec![
        write_with_skill_attempt(&current_dir, "cur-a-1", "case-a", "test", 1, true),
        write_with_skill_attempt(&current_dir, "cur-a-2", "case-a", "test", 2, false),
        write_with_skill_attempt(&current_dir, "cur-b-1", "case-b", "test", 1, true),
        write_with_skill_attempt(&current_dir, "cur-b-2", "case-b", "test", 2, true),
    ];

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    let verdict = keep_or_revert
        .by_split
        .get(&EvalSplit::Test)
        .expect("the test split ran in both iterations");
    let delta = match verdict {
        SplitVerdict::Improved(delta) | SplitVerdict::Regressed(delta) | SplitVerdict::Indistinguishable(delta) => {
            delta
        }
        SplitVerdict::NoRuns => panic!("the test split ran in both iterations"),
    };

    assert_eq!(
        delta.pass_at_k_delta,
        Some(1.0),
        "case-a and case-b both went from no attempt passing to at least one"
    );
    assert_eq!(
        delta.pass_pow_k_delta,
        Some(0.5),
        "only case-b went from no attempt passing to every attempt passing"
    );
    assert!(
        delta.pass_pow_k_delta.unwrap() < delta.pass_at_k_delta.unwrap(),
        "best-of-n must not read as the matched aggregate over the same cases"
    );
}

#[test]
fn capped_attempts_count_as_failures_for_pass_pow_k_rather_than_being_invisible() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = vec![write_with_skill_attempt(
        &previous_dir,
        "prev-a-1",
        "case-a",
        "test",
        1,
        true,
    )];
    let mut current_runs = vec![write_with_skill_attempt(
        &current_dir,
        "cur-a-1",
        "case-a",
        "test",
        1,
        true,
    )];
    current_runs.push(sample_run_with_split(
        "cur-a-2",
        "case-a",
        "with_skill",
        2,
        "budget_exhausted",
        "test",
    ));
    current_runs.push(sample_run_with_split(
        "cur-a-3",
        "case-a",
        "with_skill",
        3,
        "timeout",
        "test",
    ));

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    let verdict = keep_or_revert
        .by_split
        .get(&EvalSplit::Test)
        .expect("the test split ran in both iterations");
    let delta = match verdict {
        SplitVerdict::Improved(delta) | SplitVerdict::Regressed(delta) | SplitVerdict::Indistinguishable(delta) => {
            delta
        }
        SplitVerdict::NoRuns => panic!("the test split ran in both iterations"),
    };

    assert_eq!(
        delta.pass_at_k_delta,
        Some(0.0),
        "case-a still has one passing attempt in both iterations"
    );
    assert_eq!(
        delta.pass_pow_k_delta,
        Some(-1.0),
        "the capped attempts must count as failures, not be left out of case-a's attempts"
    );
}

#[test]
fn train_improving_while_test_stays_flat_is_suspected_overfitting() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let mut previous_runs = write_with_skill_runs(&previous_dir, "prev-test", "case-a", "test", 5, 5);
    previous_runs.extend(write_with_skill_runs(
        &previous_dir,
        "prev-train",
        "case-b",
        "train",
        0,
        40,
    ));
    let mut current_runs = write_with_skill_runs(&current_dir, "cur-test", "case-a", "test", 5, 5);
    current_runs.extend(write_with_skill_runs(
        &current_dir,
        "cur-train",
        "case-b",
        "train",
        40,
        0,
    ));

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert!(matches!(
        keep_or_revert.by_split.get(&EvalSplit::Test),
        Some(SplitVerdict::Indistinguishable(_))
    ));
    assert!(matches!(
        keep_or_revert.by_split.get(&EvalSplit::Train),
        Some(SplitVerdict::Improved(_))
    ));
    assert_eq!(keep_or_revert.recommendation, Recommendation::SuspectedOverfitting);
}

#[test]
fn a_suite_with_no_test_runs_in_either_iteration_reports_no_runs_and_stays_inconclusive() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev-train", "case-b", "train", 5, 5);
    let current_runs = write_with_skill_runs(&current_dir, "cur-train", "case-b", "train", 5, 5);

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert_eq!(
        keep_or_revert.by_split.get(&EvalSplit::Test),
        Some(&SplitVerdict::NoRuns)
    );
    assert_eq!(keep_or_revert.recommendation, Recommendation::Inconclusive);
}

/// A train-only suite (no `test`-split cases declared at all) is silence about
/// generalization, not a finding of overfitting: nothing measured the held-out split
/// because it does not exist, unlike `train_improving_while_test_stays_flat_is_suspected_overfitting`
/// where a measured, flat test split is what makes the train gain suspicious.
#[test]
fn train_improving_with_no_test_runs_at_all_stays_inconclusive_not_overfitting() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev-train", "case-b", "train", 0, 40);
    let current_runs = write_with_skill_runs(&current_dir, "cur-train", "case-b", "train", 40, 0);

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert_eq!(
        keep_or_revert.by_split.get(&EvalSplit::Test),
        Some(&SplitVerdict::NoRuns),
        "no test-split runs exist in either iteration"
    );
    assert!(matches!(
        keep_or_revert.by_split.get(&EvalSplit::Train),
        Some(SplitVerdict::Improved(_))
    ));
    assert_eq!(
        keep_or_revert.recommendation,
        Recommendation::Inconclusive,
        "a suite with no held-out set cannot support suspected_overfitting"
    );
}

#[test]
fn a_changed_eval_suite_reports_different_suites_instead_of_comparing() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev-test", "case-a", "test", 0, 40);
    let current_runs = write_with_skill_runs(&current_dir, "cur-test", "case-a", "test", 40, 0);

    write_report_with_hash(
        &previous_dir,
        serde_json::Value::Array(previous_runs),
        1,
        "report-iter-1",
        "sha256:before",
    );
    write_report_with_hash(
        &current_dir,
        serde_json::Value::Array(current_runs),
        2,
        "report-iter-2",
        "sha256:after",
    );

    let summary = build_iteration_summary_document(
        &current_dir,
        IterationSummaryOptions {
            previous_report_dir: Some(previous_dir),
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert_eq!(
        keep_or_revert.suites,
        SuiteComparison::DifferentSuites {
            current_suite_hash: "sha256:after".to_string(),
            previous_suite_hash: "sha256:before".to_string(),
        }
    );
    assert!(
        keep_or_revert.by_split.is_empty(),
        "a verdict across two different suites is not a verdict about the change"
    );
    assert_eq!(keep_or_revert.recommendation, Recommendation::Inconclusive);
}

fn write_timed_with_skill_runs(
    report_dir: &Path,
    id_prefix: &str,
    split: &str,
    timings: &[String],
) -> Vec<serde_json::Value> {
    timings
        .iter()
        .enumerate()
        .map(|(index, timing)| {
            let id = format!("{id_prefix}-{index}");
            write_run_artifacts(
                report_dir,
                &id,
                Some(r#"{"assertion_results":[{"assertion":"a","passed":true}]}"#),
                Some(timing),
            );
            sample_run_with_split(&id, "case-a", "with_skill", index as u32 + 1, "completed", split)
        })
        .collect()
}

fn timings(duration_ms: &[u64], total_tokens: u64, usd: Option<f64>) -> Vec<String> {
    duration_ms
            .iter()
            .map(|duration| match usd {
                Some(usd) => format!(
                    r#"{{ "duration_ms": {duration}, "total_tokens": {total_tokens}, "cost": {{ "kind": "priced", "usd": {usd} }} }}"#
                ),
                None => format!(r#"{{ "duration_ms": {duration}, "total_tokens": {total_tokens} }}"#),
            })
            .collect()
}

fn summary_with_tolerances(
    previous_dir: &Path,
    previous_runs: Vec<serde_json::Value>,
    current_dir: &Path,
    current_runs: Vec<serde_json::Value>,
    efficiency_tolerances: EfficiencyTolerances,
) -> IterationSummaryDocument {
    write_report(
        previous_dir,
        serde_json::Value::Array(previous_runs),
        1,
        "report-iter-1",
    );
    write_report(current_dir, serde_json::Value::Array(current_runs), 2, "report-iter-2");
    build_iteration_summary_document(
        current_dir,
        IterationSummaryOptions {
            previous_report_dir: Some(previous_dir.to_path_buf()),
            efficiency_tolerances,
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap()
}

#[test]
fn a_change_that_costs_more_per_run_is_costlier_without_moving_the_recommendation() {
    let root = tempfile::tempdir().unwrap();
    let previous_dir = root.path().join("demo-skill/report-iter-1");
    let current_dir = root.path().join("demo-skill/report-iter-2");
    let durations = [1000, 1100, 1200, 1300, 1050];
    let previous_runs =
        write_timed_with_skill_runs(&previous_dir, "prev", "test", &timings(&durations, 500, Some(0.10)));
    let current_runs = write_timed_with_skill_runs(&current_dir, "cur", "test", &timings(&durations, 1500, Some(0.30)));

    let summary = summary_with_tolerances(
        &previous_dir,
        previous_runs,
        &current_dir,
        current_runs,
        EfficiencyTolerances::default(),
    );

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert_eq!(keep_or_revert.recommendation, Recommendation::Inconclusive);
    let efficiency = keep_or_revert.efficiency.as_ref().expect("same suite, so judged");
    let test = &efficiency.by_split[&EvalSplit::Test];
    let CostVerdict::Costlier(comparison) = test.cost else {
        panic!("expected costlier, got {:?}", test.cost);
    };
    assert!((comparison.shift.estimate() - 0.20).abs() < 1e-9);
    assert!((comparison.previous_median - 0.10).abs() < 1e-9);
    assert!(matches!(test.duration, DurationVerdict::Indistinguishable(_)));
    assert!(matches!(test.tokens, TokenVerdict::MoreTokens(_)));
    assert_eq!(efficiency.by_split[&EvalSplit::Train].cost, CostVerdict::NoRuns);
}

#[test]
fn an_increase_inside_the_operator_tolerance_is_not_a_regression() {
    let root = tempfile::tempdir().unwrap();
    let previous_dir = root.path().join("demo-skill/report-iter-1");
    let current_dir = root.path().join("demo-skill/report-iter-2");
    let previous_runs = write_timed_with_skill_runs(
        &previous_dir,
        "prev",
        "test",
        &timings(&[1000, 1010, 990, 1005, 995], 500, None),
    );
    let current_runs = write_timed_with_skill_runs(
        &current_dir,
        "cur",
        "test",
        &timings(&[1100, 1110, 1090, 1105, 1095], 500, None),
    );

    let summary = summary_with_tolerances(
        &previous_dir,
        previous_runs,
        &current_dir,
        current_runs,
        EfficiencyTolerances {
            duration: IncreaseTolerance::parse(0.2).unwrap(),
            ..EfficiencyTolerances::default()
        },
    );

    let efficiency = summary.keep_or_revert.unwrap().efficiency.unwrap();
    assert_eq!(efficiency.tolerances.duration.value(), 0.2);
    let test = &efficiency.by_split[&EvalSplit::Test];
    assert!(matches!(test.duration, DurationVerdict::WithinTolerance(_)), "{test:?}");
    assert!(efficiency.regressed_splits(EfficiencyMetric::Duration).is_empty());
    assert_eq!(
        test.cost,
        CostVerdict::Unmeasured {
            reason: UnmeasuredReason::NotEveryRunPriced
        }
    );
}

#[test]
fn efficiency_is_not_judged_across_two_different_suites() {
    let root = tempfile::tempdir().unwrap();
    let previous_dir = root.path().join("demo-skill/report-iter-1");
    let current_dir = root.path().join("demo-skill/report-iter-2");
    let previous_runs =
        write_timed_with_skill_runs(&previous_dir, "prev", "test", &timings(&[1000; 5], 500, Some(0.10)));
    let current_runs = write_timed_with_skill_runs(&current_dir, "cur", "test", &timings(&[9000; 5], 5000, Some(0.90)));
    write_report_with_hash(
        &previous_dir,
        serde_json::Value::Array(previous_runs),
        1,
        "report-iter-1",
        "sha256:before",
    );
    write_report_with_hash(
        &current_dir,
        serde_json::Value::Array(current_runs),
        2,
        "report-iter-2",
        "sha256:after",
    );

    let summary = build_iteration_summary_document(
        &current_dir,
        IterationSummaryOptions {
            previous_report_dir: Some(previous_dir),
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();

    let efficiency = summary.keep_or_revert.unwrap().efficiency.unwrap();
    assert!(efficiency.by_split.is_empty());
}

#[test]
fn withheld_test_detail_keeps_the_efficiency_aggregates_and_names_no_case() {
    let root = tempfile::tempdir().unwrap();
    let previous_dir = root.path().join("demo-skill/report-iter-1");
    let current_dir = root.path().join("demo-skill/report-iter-2");
    let previous_runs =
        write_timed_with_skill_runs(&previous_dir, "prev", "test", &timings(&[1000; 5], 500, Some(0.10)));
    let current_runs = write_timed_with_skill_runs(&current_dir, "cur", "test", &timings(&[3000; 5], 500, Some(0.10)));
    write_report(
        &previous_dir,
        serde_json::Value::Array(previous_runs),
        1,
        "report-iter-1",
    );
    write_report(&current_dir, serde_json::Value::Array(current_runs), 2, "report-iter-2");

    let summary = build_iteration_summary_document(
        &current_dir,
        IterationSummaryOptions {
            previous_report_dir: Some(previous_dir),
            withhold_test_detail: true,
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();

    let efficiency = summary.keep_or_revert.unwrap().efficiency.unwrap();
    assert!(matches!(
        efficiency.by_split[&EvalSplit::Test].duration,
        DurationVerdict::Slower(_)
    ));
    let published = serde_json::to_string(&efficiency).unwrap();
    assert!(!published.contains("case-a"), "{published}");
}

#[test]
fn keep_or_revert_with_efficiency_verdicts_matches_schema() {
    let root = tempfile::tempdir().unwrap();
    let previous_dir = root.path().join("demo-skill/report-iter-1");
    let current_dir = root.path().join("demo-skill/report-iter-2");
    let previous_runs =
        write_timed_with_skill_runs(&previous_dir, "prev", "test", &timings(&[1000; 5], 500, Some(0.10)));
    let mut current_runs =
        write_timed_with_skill_runs(&current_dir, "cur", "test", &timings(&[3000; 5], 400, Some(0.30)));
    current_runs.extend(write_timed_with_skill_runs(
        &current_dir,
        "cur-train",
        "train",
        &timings(&[1000; 2], 500, None),
    ));

    let summary = summary_with_tolerances(
        &previous_dir,
        previous_runs,
        &current_dir,
        current_runs,
        EfficiencyTolerances {
            cost: IncreaseTolerance::parse(0.05).unwrap(),
            ..EfficiencyTolerances::default()
        },
    );

    let json = serde_json::to_value(&summary).unwrap();
    assert!(json["keep_or_revert"]["efficiency"]["by_split"]["test"]["cost"]["status"] == "costlier");
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/iteration-summary.json.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&json).map(|error| error.to_string()).collect();
    assert!(errors.is_empty(), "schema errors: {errors:?}");

    let mut written_before = json.clone();
    written_before["keep_or_revert"]
        .as_object_mut()
        .unwrap()
        .remove("efficiency");
    let errors: Vec<_> = validator
        .iter_errors(&written_before)
        .map(|error| error.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "a document written before efficiency verdicts: {errors:?}"
    );
    let read_back: IterationSummaryDocument = serde_json::from_value(written_before).unwrap();
    assert!(read_back.keep_or_revert.unwrap().efficiency.is_none());
}

#[test]
fn keep_or_revert_json_matches_schema() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 0, 40);
    let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 40, 0);

    let mut summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);
    summary.generated_at = "2026-05-26T12:00:00Z".to_string();

    let json = serde_json::to_value(&summary).unwrap();
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/iteration-summary.json.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&json).map(|error| error.to_string()).collect();
    assert!(errors.is_empty(), "schema errors: {errors:?}");
}

#[test]
fn build_iteration_summary_reports_headroom_and_names_saturated_cases_per_split() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    let mut runs = write_with_skill_runs(&report_dir, "test", "case-a", "test", 40, 0);
    runs.extend(write_with_skill_runs(&report_dir, "train", "case-b", "train", 40, 0));
    write_report(&report_dir, serde_json::Value::Array(runs), 1, "report-a");

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

    let headroom = summary
        .headroom
        .as_ref()
        .expect("both splits pooled clear the default threshold");
    assert_eq!(
        headroom.saturated_case_ids,
        vec!["case-a".to_string(), "case-b".to_string()]
    );

    let test_headroom = summary.by_split[&EvalSplit::Test]
        .headroom
        .as_ref()
        .expect("the test split alone is also saturated");
    assert_eq!(test_headroom.saturated_case_ids, vec!["case-a".to_string()]);

    let train_headroom = summary.by_split[&EvalSplit::Train]
        .headroom
        .as_ref()
        .expect("the train split alone is also saturated");
    assert_eq!(train_headroom.saturated_case_ids, vec!["case-b".to_string()]);
}

/// A caller that asks for `withhold_test_detail` gets no test-split eval case id or
/// assertion text anywhere in the document, not just in the fields obviously scoped to
/// the test split: the same case saturates both the per-split and the pooled headroom
/// warning, and both have to drop it. The train split and the aggregate verdicts it
/// still needs (counts, deltas, intervals) are untouched either way.
#[test]
fn withhold_test_detail_drops_test_split_case_ids_everywhere_but_leaves_train_and_verdicts_alone() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    let mut runs = write_with_skill_runs(&report_dir, "test", "case-a", "test", 40, 0);
    runs.extend(write_with_skill_runs(&report_dir, "train", "case-b", "train", 40, 0));
    write_report(&report_dir, serde_json::Value::Array(runs), 1, "report-a");

    let leaky = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
    assert!(
        leaky.always_pass.iter().any(|record| record.eval_case_id == "case-a"),
        "sanity check: without withhold_test_detail the test-split case must appear, or this test proves nothing"
    );

    let summary = build_iteration_summary_document(
        &report_dir,
        IterationSummaryOptions {
            withhold_test_detail: true,
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();

    assert!(
        summary.always_pass.iter().all(|record| record.eval_case_id != "case-a"),
        "the test-split case must not appear in the top-level always_pass list"
    );
    assert!(
        summary.always_pass.iter().any(|record| record.eval_case_id == "case-b"),
        "the train-split case must still appear in the top-level always_pass list"
    );

    let test_summary = &summary.by_split[&EvalSplit::Test];
    assert!(test_summary.always_pass.is_empty());
    assert!(test_summary.always_fail.is_empty());
    assert!(test_summary.helped_by_skill.is_empty());
    assert_eq!(
        test_summary
            .headroom
            .as_ref()
            .expect("the test split is still saturated")
            .saturated_case_ids,
        Vec::<String>::new(),
        "the test split's own headroom warning must not name which case saturated it"
    );

    let train_summary = &summary.by_split[&EvalSplit::Train];
    assert!(
        !train_summary.always_pass.is_empty(),
        "the train split keeps its own detail"
    );
    assert_eq!(
        train_summary.headroom.as_ref().unwrap().saturated_case_ids,
        vec!["case-b".to_string()]
    );

    assert_eq!(
        summary.headroom.as_ref().unwrap().saturated_case_ids,
        vec!["case-b".to_string()],
        "the pooled headroom warning must drop the test-split case id but keep the train-split one"
    );
}

/// A case the current run never executed at all, because a narrower selection excluded
/// it, still has to be masked when its stability came from the previous report instead:
/// `cross_iteration.no_longer_always_pass` is populated from runs the previous report
/// made, not the current one, so looking up "is this a test case" only in the current
/// report's runs misses it entirely.
#[test]
fn withhold_test_detail_masks_a_held_out_case_that_only_appears_in_the_previous_reports_cross_iteration_data() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev-test", "case-a", "test", 40, 0);
    let current_runs = write_with_skill_runs(&current_dir, "cur-train", "case-b", "train", 40, 0);
    write_report(
        &previous_dir,
        serde_json::Value::Array(previous_runs),
        1,
        "report-iter-1",
    );
    write_report(&current_dir, serde_json::Value::Array(current_runs), 2, "report-iter-2");

    let leaky = build_iteration_summary_document(
        &current_dir,
        IterationSummaryOptions {
            previous_report_dir: Some(previous_dir.clone()),
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();
    let leaky_cross_iteration = leaky.cross_iteration.as_ref().expect("a previous iteration exists");
    assert!(
        leaky_cross_iteration
            .no_longer_always_pass
            .iter()
            .any(|record| record.eval_case_id == "case-a"),
        "sanity check: without withholding, the never-rerun held-out case must show up as \
             no longer always passing, or this test proves nothing"
    );

    let summary = build_iteration_summary_document(
        &current_dir,
        IterationSummaryOptions {
            previous_report_dir: Some(previous_dir),
            withhold_test_detail: true,
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();

    let cross_iteration = summary.cross_iteration.as_ref().expect("a previous iteration exists");
    assert!(
        cross_iteration
            .no_longer_always_pass
            .iter()
            .all(|record| record.eval_case_id != "case-a"),
        "a test-split case the current run never executed must still be withheld from \
             cross-iteration detail"
    );
}

#[test]
fn build_iteration_summary_reports_no_headroom_warning_when_the_with_skill_arm_still_has_room() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    let runs = write_with_skill_runs(&report_dir, "test", "case-a", "test", 5, 5);
    write_report(&report_dir, serde_json::Value::Array(runs), 1, "report-a");

    let summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();

    assert!(summary.headroom.is_none());
    assert!(summary.by_split[&EvalSplit::Test].headroom.is_none());
}

#[test]
fn build_iteration_summary_headroom_threshold_is_configurable() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    let runs = write_with_skill_runs(&report_dir, "test", "case-a", "test", 5, 0);
    write_report(&report_dir, serde_json::Value::Array(runs), 1, "report-a");

    let default_summary = build_iteration_summary_document(&report_dir, IterationSummaryOptions::default()).unwrap();
    assert!(default_summary.headroom.is_none());

    let lenient_summary = build_iteration_summary_document(
        &report_dir,
        IterationSummaryOptions {
            headroom_threshold: HeadroomThreshold::parse(0.5).unwrap(),
            ..IterationSummaryOptions::default()
        },
    )
    .unwrap();
    assert!(lenient_summary.headroom.is_some());
}

#[test]
fn keep_or_revert_is_capped_by_saturation_when_the_test_split_is_saturated() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 0, 40);
    let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 40, 0);

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert_eq!(
        keep_or_revert.capped_by_saturation,
        Some(SaturationCap::TestSplitSaturated),
        "the test split's with_skill arm passed everything, so the keep verdict cannot be read as ordinary"
    );
}

#[test]
fn keep_or_revert_is_not_capped_when_the_test_split_still_has_room() {
    let root = tempfile::tempdir().unwrap();
    let skill_root = root.path().join("demo-skill");
    let previous_dir = skill_root.join("report-iter-1");
    let current_dir = skill_root.join("report-iter-2");

    let previous_runs = write_with_skill_runs(&previous_dir, "prev", "case-a", "test", 5, 5);
    let current_runs = write_with_skill_runs(&current_dir, "cur", "case-a", "test", 5, 5);

    let summary = build_summary_against_previous(&previous_dir, previous_runs, &current_dir, current_runs);

    let keep_or_revert = summary.keep_or_revert.as_ref().expect("a previous iteration exists");
    assert_eq!(keep_or_revert.capped_by_saturation, None);
}
