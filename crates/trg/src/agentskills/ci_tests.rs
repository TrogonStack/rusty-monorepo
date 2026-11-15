use super::*;
use crate::agentskills::evals::{EvalDirName, EvalSplit};
use crate::agentskills::report::{
    ProducerSection, ReportDocument, ReportSection, RunMetrics, RunPaths, RunRecord, ScenarioKind, SuiteSection,
    SummariesSection,
};
use std::fs;
use std::path::Path;

fn sample_metrics(pass_rate: f64, tokens: u64, duration: u64) -> ReportMetrics {
    let total = 10;
    let passed = (pass_rate * total as f64).round() as usize;
    ReportMetrics {
        total_runs: 1,
        failed_runs: 0,
        skipped_runs: 0,
        completed_runs: 1,
        grading_files: 1,
        assertion_results: total,
        passed_assertions: passed,
        failed_assertions: total - passed,
        unsupported_assertions: 0,
        excluded_assertions: 0,
        ungraded_assertions: 0,
        pass_rate: Some(pass_rate),
        total_tokens: tokens,
        input_tokens: tokens / 2,
        output_tokens: tokens / 2,
        max_duration_ms: duration,
        total_duration_ms: duration,
    }
}

fn unscored_metrics(skipped_runs: usize) -> ReportMetrics {
    ReportMetrics {
        total_runs: skipped_runs,
        skipped_runs,
        completed_runs: 0,
        pass_rate: None,
        ..sample_metrics(0.0, 0, 0)
    }
}

/// A gate that cannot be evaluated has to say so. Reporting it as met is how a
/// suite that stopped running keeps its green check.
#[test]
fn a_minimum_pass_rate_is_not_met_by_a_pass_that_scored_nothing() {
    let result = run_ci_checks(
        &unscored_metrics(6),
        CiPolicy::default(),
        &ThresholdConfig {
            min_pass_rate: Some(1.0),
            ..Default::default()
        },
        &[],
        &[],
        &[],
    );

    assert!(!result.passed);
    let violation = result
        .violations
        .iter()
        .find(|v| v.kind == CiViolationKind::NothingScored)
        .expect("the unmet gate is reported");
    assert!(
        violation.message.contains("nothing was scored"),
        "{}",
        violation.message
    );
    assert!(violation.message.contains("6 skipped"), "{}", violation.message);
}

/// Asking for no minimum is not asking for a gate, so a scaffolded bundle stays
/// the useful thing it is.
#[test]
fn a_pass_that_scored_nothing_is_not_a_violation_on_its_own() {
    let result = run_ci_checks(
        &unscored_metrics(6),
        CiPolicy::strict_ci(),
        &ThresholdConfig::default(),
        &[],
        &[],
        &[],
    );

    assert!(result.passed, "{:?}", result.violations);
}

fn case_record(workspace: &str, case_score: Option<f64>) -> CaseScoreRecord {
    CaseScoreRecord {
        run_id: None,
        workspace: workspace.to_string(),
        file: format!("{workspace}/grading.json"),
        case_score,
    }
}

/// A suite's aggregate pass rate can stay healthy while one case failed
/// everything it was asked, so the gate has to look at every case rather
/// than trust the average nothing here is wrong.
#[test]
fn case_score_gate_trips_on_one_wholly_failing_case_despite_a_healthy_aggregate_pass_rate() {
    let metrics = sample_metrics(0.95, 100, 100);
    let case_scores = vec![
        case_record("runs/run-001/workspace", Some(1.0)),
        case_record("runs/run-002/workspace", Some(0.0)),
    ];
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig {
            min_case_score: Some(0.5),
            ..Default::default()
        },
        &[],
        &[],
        &case_scores,
    );

    assert!(!result.passed);
    let violation = result
        .violations
        .iter()
        .find(|v| v.kind == CiViolationKind::CaseScoreBelowMinimum)
        .expect("the wholly failing case must trip the gate on its own");
    assert!(violation.workspace.as_deref() == Some("runs/run-002/workspace"));
}

/// A case scored on nothing is not a case that scored zero, so it cannot be
/// let through a minimum as if a zero had cleared it, and it cannot be read
/// as meeting even a minimum of zero.
#[test]
fn a_case_with_nothing_scored_does_not_pass_the_gate_and_is_not_read_as_zero() {
    let metrics = sample_metrics(1.0, 100, 100);
    let case_scores = vec![case_record("runs/run-001/workspace", None)];
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig {
            min_case_score: Some(0.0),
            ..Default::default()
        },
        &[],
        &[],
        &case_scores,
    );

    assert!(!result.passed, "a case with no score must not silently clear the gate");
    assert!(!result
        .violations
        .iter()
        .any(|v| v.kind == CiViolationKind::CaseScoreBelowMinimum));
    let violation = result
        .violations
        .iter()
        .find(|v| v.kind == CiViolationKind::CaseNothingScored)
        .expect("nothing scored must be its own violation kind");
    assert!(
        violation.message.contains("nothing was scored"),
        "{}",
        violation.message
    );
}

#[test]
fn case_scores_without_a_minimum_declared_produce_no_violation() {
    let metrics = sample_metrics(1.0, 100, 100);
    let case_scores = vec![case_record("runs/run-001/workspace", None)];
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig::default(),
        &[],
        &[],
        &case_scores,
    );

    assert!(result.passed, "{:?}", result.violations);
}

#[test]
fn pass_rate_minimum_boundary_passes_at_threshold() {
    let metrics = sample_metrics(0.8, 100, 1000);
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig {
            min_pass_rate: Some(0.8),
            ..Default::default()
        },
        &[],
        &[],
        &[],
    );
    assert!(result.passed);
}

#[test]
fn pass_rate_minimum_boundary_fails_below_threshold() {
    let metrics = sample_metrics(0.799, 100, 1000);
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig {
            min_pass_rate: Some(0.8),
            ..Default::default()
        },
        &[],
        &[],
        &[],
    );
    assert!(!result.passed);
    assert!(result
        .violations
        .iter()
        .any(|v| v.kind == CiViolationKind::PassRateBelowMinimum));
}

#[test]
fn token_budget_boundary_passes_at_limit() {
    let metrics = sample_metrics(1.0, 1000, 500);
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig {
            max_tokens: Some(1000),
            ..Default::default()
        },
        &[],
        &[],
        &[],
    );
    assert!(result.passed);
}

#[test]
fn token_budget_boundary_fails_above_limit() {
    let metrics = sample_metrics(1.0, 1001, 500);
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig {
            max_tokens: Some(1000),
            ..Default::default()
        },
        &[],
        &[],
        &[],
    );
    assert!(!result.passed);
    assert!(result
        .violations
        .iter()
        .any(|v| v.kind == CiViolationKind::TokenBudgetExceeded));
}

#[test]
fn duration_boundary_fails_above_limit() {
    let metrics = sample_metrics(1.0, 100, 1001);
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig {
            max_duration_ms: Some(1000),
            ..Default::default()
        },
        &[],
        &[],
        &[],
    );
    assert!(!result.passed);
    assert!(result
        .violations
        .iter()
        .any(|v| v.kind == CiViolationKind::DurationExceeded));
}

#[test]
fn strict_policy_flags_runner_failure() {
    let mut metrics = sample_metrics(1.0, 100, 100);
    metrics.failed_runs = 1;
    let result = run_ci_checks(
        &metrics,
        CiPolicy::strict_ci(),
        &ThresholdConfig::default(),
        &[],
        &[],
        &[],
    );
    assert!(!result.passed);
    assert!(result
        .violations
        .iter()
        .any(|v| v.kind == CiViolationKind::RunnerFailure));
}

#[test]
fn baseline_pass_rate_regression_detected() {
    let temp = tempfile::tempdir().unwrap();
    let baseline_dir = temp.path().join("baseline");
    let current_dir = temp.path().join("current");
    write_metrics_report(&baseline_dir, sample_metrics(0.9, 100, 100));
    write_metrics_report(&current_dir, sample_metrics(0.7, 100, 100));

    let current = collect_report_metrics(&current_dir).unwrap();
    let result = run_ci_checks(
        &current,
        CiPolicy {
            fail_on_pass_rate_regression: true,
            ..Default::default()
        },
        &ThresholdConfig {
            baseline: Some(baseline_dir),
            ..Default::default()
        },
        &[],
        &[],
        &[],
    );
    assert!(!result.passed);
    assert!(result
        .violations
        .iter()
        .any(|v| v.kind == CiViolationKind::PassRateRegression));
}

#[test]
fn unsupported_results_are_not_annotated_as_failed_assertions() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("grading.json"),
        r#"{
        "assertion_results": [
            {
                "assertion": "the skill was engaged",
                "passed": false,
                "evidence": "runner 'codex' does not expose tool calls",
                "grader": {"kind": "declarative"},
                "unsupported": "runner 'codex' does not expose tool calls"
            },
            {
                "assertion": "final text mentions the budget",
                "passed": false,
                "evidence": "final text (12 bytes) does not contain 'budget'",
                "grader": {"kind": "declarative"}
            }
        ],
        "summary": {"passed": 0, "failed": 1, "unsupported": 1, "total": 2, "pass_rate": 0.0}
    }"#,
    )
    .unwrap();

    let mut details = Vec::new();
    collect_failed_assertions_in_workspace(&workspace, None, "workspace".to_string(), &mut details).unwrap();

    assert_eq!(details.len(), 1);
    assert_eq!(details[0].text, "final text mentions the budget");
}

#[test]
fn ungraded_results_are_not_annotated_as_failed_assertions() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("grading.json"),
        r#"{
        "assertion_results": [
            {
                "assertion": "the report reads as encouraging",
                "passed": false,
                "evidence": "no mechanical pattern matched and no judge was consulted",
                "grader": {"kind": "needs_llm"},
                "ungraded": "no mechanical pattern matched and no judge was consulted"
            },
            {
                "assertion": "final text mentions the budget",
                "passed": false,
                "evidence": "final text (12 bytes) does not contain 'budget'",
                "grader": {"kind": "declarative"}
            }
        ],
        "summary": {"passed": 0, "failed": 1, "ungraded": 1, "total": 2, "pass_rate": 0.0}
    }"#,
    )
    .unwrap();

    let mut details = Vec::new();
    collect_failed_assertions_in_workspace(&workspace, None, "workspace".to_string(), &mut details).unwrap();

    assert_eq!(details.len(), 1);
    assert_eq!(details[0].text, "final text mentions the budget");
}

/// A gate that reports green over a suite it did not fully measure is worse than
/// one that reports red, because a passing gate is the one nobody looks behind.
#[test]
fn a_gate_refuses_to_pass_when_anything_went_ungraded() {
    let metrics = ReportMetrics {
        ungraded_assertions: 1,
        ..sample_metrics(1.0, 10, 20)
    };
    let result = run_ci_checks(
        &metrics,
        CiPolicy::default(),
        &ThresholdConfig::default(),
        &[],
        &[],
        &[],
    );

    assert!(!result.passed);
    assert!(result.violations.iter().any(|v| v.kind == CiViolationKind::Ungraded));
}

#[test]
fn json_output_is_stable() {
    let output = EvalCommandJsonOutput {
        report_dir: "/tmp/report".to_string(),
        exit_code: ExitCode::Success,
        check: CiCheckResult {
            passed: true,
            violations: Vec::new(),
            metrics: sample_metrics(1.0, 10, 20),
            baseline_metrics: None,
        },
        workspace: None,
    };
    let json = serde_json::to_string(&output).unwrap();
    assert_eq!(
        json,
        r#"{"report_dir":"/tmp/report","exit_code":0,"check":{"passed":true,"violations":[],"metrics":{"total_runs":1,"failed_runs":0,"skipped_runs":0,"completed_runs":1,"grading_files":1,"assertion_results":10,"passed_assertions":10,"failed_assertions":0,"unsupported_assertions":0,"excluded_assertions":0,"ungraded_assertions":0,"pass_rate":1.0,"total_tokens":10,"input_tokens":5,"output_tokens":5,"max_duration_ms":20,"total_duration_ms":20}}}"#
    );
}

#[test]
fn github_annotation_format_is_stable() {
    let violations = vec![CiViolation {
        kind: CiViolationKind::FailedAssertion,
        message: "assertion failed: missing chart".to_string(),
        run_id: Some("run-001".to_string()),
        workspace: Some("runs/run-001/workspace".to_string()),
        file: Some("runs/run-001/workspace/grading.json".to_string()),
        line: Some(2),
    }];

    let lines = format_github_annotations(&violations);
    assert_eq!(
        lines,
        vec!["::error file=runs/run-001/workspace/grading.json,line=2::assertion failed: missing chart".to_string()]
    );
}

#[test]
fn github_annotations_gated_on_env_var() {
    let violations = vec![CiViolation {
        kind: CiViolationKind::FailedAssertion,
        message: "assertion failed: x".to_string(),
        run_id: None,
        workspace: None,
        file: Some("grading.json".to_string()),
        line: Some(1),
    }];

    assert_eq!(format_github_annotations(&violations).len(), 1);
    assert!(github_annotations_for(&violations, &|_| None).is_empty());
    assert!(github_annotations_for(&violations, &|_| Some("false".to_string())).is_empty());
    assert_eq!(
        github_annotations_for(&violations, &|name| (name == "GITHUB_ACTIONS")
            .then(|| "true".to_string())),
        format_github_annotations(&violations)
    );
}

fn write_metrics_report(dir: &Path, metrics: ReportMetrics) {
    fs::create_dir_all(dir.join("runs/run-001/workspace")).unwrap();
    let pass_rate = metrics.pass_rate.unwrap_or(0.0);
    let passed = metrics.passed_assertions;
    let failed = metrics.failed_assertions;
    let assertion_rows: Vec<String> = (0..metrics.assertion_results)
    .map(|index| {
        let is_passed = index < passed;
        format!(
            r#"    {{ "assertion": "assertion-{index}", "passed": {is_passed}, "evidence": "evidence-{index}", "grader": {{ "kind": "mechanical" }} }}"#,
            is_passed = if is_passed { "true" } else { "false" }
        )
    })
    .collect();
    fs::write(
    dir.join("runs/run-001/workspace/grading.json"),
    format!(
        "{{\n  \"assertion_results\": [\n{}\n  ],\n  \"summary\": {{ \"passed\": {passed}, \"failed\": {failed}, \"total\": {}, \"pass_rate\": {pass_rate} }}\n}}",
        assertion_rows.join(",\n"),
        metrics.assertion_results
    ),
)
.unwrap();
    fs::write(
        dir.join("runs/run-001/workspace/timing.json"),
        format!(
            r#"{{ "total_tokens": {}, "duration_ms": {} }}"#,
            metrics.total_tokens, metrics.max_duration_ms
        ),
    )
    .unwrap();

    let document = ReportDocument {
        report: ReportSection {
            id: "report-test".to_string(),
            generated_at: "2026-05-26T00:00:00Z".to_string(),
            producer: ProducerSection {
                name: "trg".to_string(),
                version: "0.0.0".to_string(),
            },
            runner: None,
            runner_binary: None,
            runner_version: None,
            environment: crate::agentskills::report::EnvironmentPolicy::default(),
            permission: crate::agentskills::permission_outcome::PermissionOutcome::default(),
            allowed_tools: None,
            telemetry_forwarding: crate::agentskills::runner::TelemetryForwarding::Off,
            ci: None,
            trace: None,
            iteration: 1,
        },
        suite: SuiteSection {
            skill_name: "demo".to_string(),
            skill_path: "demo".to_string(),
            skill_hash: "sha256:abc".to_string(),
            evals_path: "demo/evals/evals.json".to_string(),
            evals_hash: "sha256:def".to_string(),
            eval_dir: EvalDirName::default(),
            old_skill_path: None,
            old_skill_hash: None,
            case_selection: None,
        },
        dimensions: crate::agentskills::report::DimensionsSection {
            eval_cases: Vec::new(),
            assertions: Vec::new(),
            skill_revisions: Vec::new(),
            model_configs: Vec::new(),
            scenarios: Vec::new(),
            grading_strategies: Vec::new(),
        },
        runs: vec![RunRecord {
            runner_model: None,
            runner_model_source: None,
            id: "run-001".to_string(),
            eval_case_id: "case-a".to_string(),
            eval_slug: "case-a".to_string(),
            split: EvalSplit::Train,
            iteration: 1,
            mirror_path: "iteration-1/eval-case-a/with_skill/".to_string(),
            tool_grant: None,
            scenario_id: ScenarioKind::WithSkill,
            model_config_id: "ci-default".to_string(),
            skill_revision_id: "current".to_string(),
            attempt: 1,
            status: "completed".to_string(),
            runner_invocations: 1,
            failure_kind: None,
            paths: RunPaths {
                workspace: "runs/run-001/workspace".to_string(),
                outputs: "runs/run-001/workspace/outputs".to_string(),
            },
            artifacts: Vec::new(),
            metrics: RunMetrics {
                duration_ms: Some(metrics.max_duration_ms),
                exit_code: None,
                total_tokens: Some(metrics.total_tokens),
                input_tokens: Some(metrics.input_tokens),
                output_tokens: Some(metrics.output_tokens),
                cached_tokens: None,
                cost: None,
                transcript: None,
            },
            cache: None,
            skill_integrity: None,
            read_only_fixture_violations: Vec::new(),
            warnings: Vec::new(),
            mock_violations: Vec::new(),
            case_score: None,
            trace: None,
            grade_trace: None,
        }],
        assertion_results: Vec::new(),
        summaries: SummariesSection {
            by_scenario: Vec::new(),
            human_feedback: Default::default(),
        },
        comparisons: Vec::new(),
        improvement_feedback: Default::default(),
        iteration_summary: None,
        budget: None,
    };
    let report_json = serde_json::to_string_pretty(&document).unwrap();
    fs::write(dir.join("report.json"), report_json).unwrap();
}

#[test]
fn collect_report_metrics_from_fixture_layout() {
    let temp = tempfile::tempdir().unwrap();
    write_metrics_report(temp.path(), sample_metrics(0.5, 1000, 2500));
    let metrics = collect_report_metrics(temp.path()).unwrap();
    assert_eq!(metrics.assertion_results, 10);
    assert_eq!(metrics.pass_rate, Some(0.5));
    assert_eq!(metrics.total_tokens, 1000);
    assert_eq!(metrics.max_duration_ms, 2500);
}

#[test]
fn find_report_dir_from_nested_workspace() {
    let temp = tempfile::tempdir().unwrap();
    write_metrics_report(temp.path(), sample_metrics(1.0, 1, 1));
    let workspace = temp.path().join("runs/run-001/workspace");
    assert_eq!(find_report_dir(&workspace), Some(temp.path().to_path_buf()));
}
