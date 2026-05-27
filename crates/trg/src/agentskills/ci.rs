use std::path::{Path, PathBuf};

use serde::Serialize;

use serde::Deserialize;

use super::evals::{check_workspace, EvalError, WorkspaceCheckOptions, WorkspaceCheckReport};
use super::report::RunRecord;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CiPolicy {
    pub fail_on_runner_failure: bool,
    pub fail_on_failed_assertions: bool,
    pub fail_on_missing_grading: bool,
    pub fail_on_pass_rate_regression: bool,
    pub fail_on_token_regression: bool,
    pub fail_on_duration_regression: bool,
}

impl CiPolicy {
    pub fn strict_ci() -> Self {
        Self {
            fail_on_runner_failure: true,
            fail_on_failed_assertions: true,
            fail_on_missing_grading: true,
            fail_on_pass_rate_regression: true,
            fail_on_token_regression: true,
            fail_on_duration_regression: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ThresholdConfig {
    pub min_pass_rate: Option<f64>,
    pub max_tokens: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_duration_ms: Option<u64>,
    pub baseline: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReportMetrics {
    pub total_runs: usize,
    pub failed_runs: usize,
    pub skipped_runs: usize,
    pub completed_runs: usize,
    pub grading_files: usize,
    pub assertion_results: usize,
    pub passed_assertions: usize,
    pub failed_assertions: usize,
    pub pass_rate: f64,
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub max_duration_ms: u64,
    pub total_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CiViolationKind {
    RunnerFailure,
    FailedAssertion,
    MissingGrading,
    PassRateBelowMinimum,
    PassRateRegression,
    TokenBudgetExceeded,
    InputTokenBudgetExceeded,
    OutputTokenBudgetExceeded,
    TokenRegression,
    DurationExceeded,
    DurationRegression,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CiViolation {
    pub kind: CiViolationKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CiCheckResult {
    pub passed: bool,
    pub violations: Vec<CiViolation>,
    pub metrics: ReportMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_metrics: Option<ReportMetrics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalCommandJsonOutput {
    pub report_dir: String,
    pub exit_code: i32,
    pub check: CiCheckResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceCheckReport>,
}

pub fn find_report_dir(workspace_or_child: &Path) -> Option<PathBuf> {
    let mut current = workspace_or_child.to_path_buf();
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }
    loop {
        if current.join("report.json").is_file() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReportRunsDocument {
    runs: Vec<RunRecord>,
}

pub fn load_report_runs(report_dir: &Path) -> Result<Vec<RunRecord>, EvalError> {
    let report_path = report_dir.join("report.json");
    let content = std::fs::read_to_string(&report_path)?;
    Ok(serde_json::from_str::<ReportRunsDocument>(&content)?.runs)
}

pub fn collect_report_metrics(report_dir: &Path) -> Result<ReportMetrics, EvalError> {
    let runs = load_report_runs(report_dir)?;
    let mut metrics = ReportMetrics::default();

    for run in &runs {
        metrics.total_runs += 1;
        match run.status.as_str() {
            "failed" => metrics.failed_runs += 1,
            "skipped" => metrics.skipped_runs += 1,
            "completed" => metrics.completed_runs += 1,
            _ => {}
        }

        if let Some(tokens) = run.metrics.total_tokens {
            metrics.total_tokens = metrics.total_tokens.saturating_add(tokens);
        }
        if let Some(tokens) = run.metrics.input_tokens {
            metrics.input_tokens = metrics.input_tokens.saturating_add(tokens);
        }
        if let Some(tokens) = run.metrics.output_tokens {
            metrics.output_tokens = metrics.output_tokens.saturating_add(tokens);
        }
        if let Some(duration) = run.metrics.duration_ms {
            metrics.total_duration_ms = metrics.total_duration_ms.saturating_add(duration);
            metrics.max_duration_ms = metrics.max_duration_ms.max(duration);
        }

        let workspace = report_dir.join(&run.paths.workspace);
        if !workspace.is_dir() {
            continue;
        }

        let workspace_report = check_workspace(&workspace, WorkspaceCheckOptions::default())?;
        merge_workspace_metrics(&mut metrics, &workspace_report);
    }

    metrics.pass_rate = compute_pass_rate(metrics.passed_assertions, metrics.assertion_results);
    Ok(metrics)
}

pub fn collect_workspace_metrics(workspace: &Path) -> Result<ReportMetrics, EvalError> {
    let workspace_report = check_workspace(workspace, WorkspaceCheckOptions::default())?;
    let mut metrics = ReportMetrics::default();
    merge_workspace_metrics(&mut metrics, &workspace_report);
    metrics.pass_rate = compute_pass_rate(metrics.passed_assertions, metrics.assertion_results);
    Ok(metrics)
}

pub fn run_ci_checks(
    metrics: &ReportMetrics,
    policy: CiPolicy,
    thresholds: &ThresholdConfig,
    failed_assertions: &[FailedAssertionDetail],
    missing_grading_workspaces: &[String],
) -> CiCheckResult {
    let baseline_metrics = thresholds
        .baseline
        .as_ref()
        .and_then(|path| collect_report_metrics(path).ok());

    let mut violations = Vec::new();

    if policy.fail_on_runner_failure && metrics.failed_runs > 0 {
        violations.push(CiViolation {
            kind: CiViolationKind::RunnerFailure,
            message: format!("{} run(s) failed", metrics.failed_runs),
            run_id: None,
            workspace: None,
            file: None,
            line: None,
        });
    }

    if policy.fail_on_missing_grading {
        for workspace in missing_grading_workspaces {
            violations.push(CiViolation {
                kind: CiViolationKind::MissingGrading,
                message: format!("workspace '{}' has no grading.json", workspace),
                run_id: None,
                workspace: Some(workspace.clone()),
                file: None,
                line: None,
            });
        }
    }

    if policy.fail_on_failed_assertions {
        for detail in failed_assertions {
            violations.push(CiViolation {
                kind: CiViolationKind::FailedAssertion,
                message: format!("assertion failed: {}", detail.text),
                run_id: detail.run_id.clone(),
                workspace: Some(detail.workspace.clone()),
                file: Some(detail.file.clone()),
                line: Some(detail.line),
            });
        }
    }

    if let Some(minimum) = thresholds.min_pass_rate {
        if metrics.assertion_results > 0 && metrics.pass_rate + f64::EPSILON < minimum {
            violations.push(CiViolation {
                kind: CiViolationKind::PassRateBelowMinimum,
                message: format!(
                    "pass rate {:.4} is below minimum {:.4}",
                    metrics.pass_rate, minimum
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
    }

    if let Some(maximum) = thresholds.max_tokens {
        if metrics.total_tokens > maximum {
            violations.push(CiViolation {
                kind: CiViolationKind::TokenBudgetExceeded,
                message: format!(
                    "total tokens {} exceeds maximum {}",
                    metrics.total_tokens, maximum
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
    }

    if let Some(maximum) = thresholds.max_input_tokens {
        if metrics.input_tokens > maximum {
            violations.push(CiViolation {
                kind: CiViolationKind::InputTokenBudgetExceeded,
                message: format!(
                    "input tokens {} exceeds maximum {}",
                    metrics.input_tokens, maximum
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
    }

    if let Some(maximum) = thresholds.max_output_tokens {
        if metrics.output_tokens > maximum {
            violations.push(CiViolation {
                kind: CiViolationKind::OutputTokenBudgetExceeded,
                message: format!(
                    "output tokens {} exceeds maximum {}",
                    metrics.output_tokens, maximum
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
    }

    if let Some(maximum) = thresholds.max_duration_ms {
        if metrics.max_duration_ms > maximum {
            violations.push(CiViolation {
                kind: CiViolationKind::DurationExceeded,
                message: format!(
                    "max duration {}ms exceeds maximum {}ms",
                    metrics.max_duration_ms, maximum
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
    }

    if let Some(baseline) = &baseline_metrics {
        if policy.fail_on_pass_rate_regression
            && baseline.assertion_results > 0
            && metrics.assertion_results > 0
            && metrics.pass_rate + f64::EPSILON < baseline.pass_rate
        {
            violations.push(CiViolation {
                kind: CiViolationKind::PassRateRegression,
                message: format!(
                    "pass rate {:.4} regressed from baseline {:.4}",
                    metrics.pass_rate, baseline.pass_rate
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }

        if policy.fail_on_token_regression && metrics.total_tokens > baseline.total_tokens {
            violations.push(CiViolation {
                kind: CiViolationKind::TokenRegression,
                message: format!(
                    "total tokens {} regressed above baseline {}",
                    metrics.total_tokens, baseline.total_tokens
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }

        if policy.fail_on_duration_regression && metrics.max_duration_ms > baseline.max_duration_ms {
            violations.push(CiViolation {
                kind: CiViolationKind::DurationRegression,
                message: format!(
                    "max duration {}ms regressed above baseline {}ms",
                    metrics.max_duration_ms, baseline.max_duration_ms
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
    }

    CiCheckResult {
        passed: violations.is_empty(),
        violations,
        metrics: metrics.clone(),
        baseline_metrics,
    }
}

#[derive(Debug, Clone)]
pub struct FailedAssertionDetail {
    pub run_id: Option<String>,
    pub workspace: String,
    pub file: String,
    pub line: u32,
    pub text: String,
}

pub fn collect_failed_assertions(report_dir: &Path) -> Result<Vec<FailedAssertionDetail>, EvalError> {
    let runs = load_report_runs(report_dir)?;
    let mut details = Vec::new();

    for run in &runs {
        let workspace = report_dir.join(&run.paths.workspace);
        collect_failed_assertions_in_workspace(
            &workspace,
            Some(run.id.clone()),
            workspace.display().to_string(),
            &mut details,
        )?;
    }

    Ok(details)
}

pub fn collect_failed_assertions_in_workspace(
    workspace: &Path,
    run_id: Option<String>,
    workspace_label: String,
    details: &mut Vec<FailedAssertionDetail>,
) -> Result<(), EvalError> {
    if !workspace.is_dir() {
        return Ok(());
    }

    let mut grading_files = Vec::new();
    super::evals::collect_named_files(workspace, "grading.json", &mut grading_files)?;

    for grading_path in grading_files {
        let content = std::fs::read_to_string(&grading_path)?;
        let grading: GradingForAnnotations = serde_json::from_str(&content)?;
        for (index, result) in grading.assertion_results.iter().enumerate() {
            if result.passed {
                continue;
            }
            details.push(FailedAssertionDetail {
                run_id: run_id.clone(),
                workspace: workspace_label.clone(),
                file: grading_path.display().to_string(),
                line: index as u32 + 1,
                text: result.assertion.clone(),
            });
        }
    }

    Ok(())
}

pub fn collect_missing_grading_workspaces(report_dir: &Path) -> Result<Vec<String>, EvalError> {
    let runs = load_report_runs(report_dir)?;
    let mut missing = Vec::new();

    for run in &runs {
        if run.status != "completed" {
            continue;
        }
        let workspace = report_dir.join(&run.paths.workspace);
        let mut grading_files = Vec::new();
        super::evals::collect_named_files(&workspace, "grading.json", &mut grading_files)?;
        if grading_files.is_empty() {
            missing.push(workspace.display().to_string());
        }
    }

    Ok(missing)
}

pub fn format_github_annotations(violations: &[CiViolation]) -> Vec<String> {
    violations
        .iter()
        .filter(|violation| violation.kind == CiViolationKind::FailedAssertion)
        .map(|violation| {
            let file = violation.file.as_deref().unwrap_or("grading.json");
            let line = violation.line.unwrap_or(1);
            let message = escape_github_message(&violation.message);
            format!("::error file={file},line={line}::{message}")
        })
        .collect()
}

pub fn emit_github_annotations(violations: &[CiViolation]) {
    if std::env::var("GITHUB_ACTIONS").ok().as_deref() != Some("true") {
        return;
    }

    for line in format_github_annotations(violations) {
        println!("{line}");
    }
}

pub fn print_human_summary(check: &CiCheckResult) {
    println!(
        "assertions: {}/{} passed ({:.2}%)",
        check.metrics.passed_assertions,
        check.metrics.assertion_results,
        check.metrics.pass_rate * 100.0
    );
    if !check.violations.is_empty() {
        println!("ci checks: failed ({} violation(s))", check.violations.len());
        for violation in &check.violations {
            println!("  - {}", violation.message);
        }
    } else {
        println!("ci checks: passed");
    }
}

fn merge_workspace_metrics(metrics: &mut ReportMetrics, workspace: &WorkspaceCheckReport) {
    metrics.grading_files += workspace.grading_files;
    metrics.assertion_results += workspace.assertion_results;
    metrics.passed_assertions += workspace.passed_assertions;
    metrics.failed_assertions += workspace.failed_assertions;
}

fn compute_pass_rate(passed: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        passed as f64 / total as f64
    }
}

fn escape_github_message(message: &str) -> String {
    message.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

#[derive(Debug, serde::Deserialize)]
struct GradingForAnnotations {
    assertion_results: Vec<AssertionForAnnotations>,
}

#[derive(Debug, serde::Deserialize)]
struct AssertionForAnnotations {
    #[serde(alias = "text")]
    assertion: String,
    passed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::report::{
        ReportDocument, ReportSection, ProducerSection, RunMetrics, RunPaths, RunRecord, SCHEMA_VERSION,
        ScenarioKind, SuiteSection, SummariesSection,
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
            pass_rate,
            total_tokens: tokens,
            input_tokens: tokens / 2,
            output_tokens: tokens / 2,
            max_duration_ms: duration,
            total_duration_ms: duration,
        }
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
        let result = run_ci_checks(&metrics, CiPolicy::strict_ci(), &ThresholdConfig::default(), &[], &[]);
        assert!(!result.passed);
        assert!(result.violations.iter().any(|v| v.kind == CiViolationKind::RunnerFailure));
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
        );
        assert!(!result.passed);
        assert!(result
            .violations
            .iter()
            .any(|v| v.kind == CiViolationKind::PassRateRegression));
    }

    #[test]
    fn json_output_is_stable() {
        let output = EvalCommandJsonOutput {
            report_dir: "/tmp/report".to_string(),
            exit_code: 0,
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
            r#"{"report_dir":"/tmp/report","exit_code":0,"check":{"passed":true,"violations":[],"metrics":{"total_runs":1,"failed_runs":0,"skipped_runs":0,"completed_runs":1,"grading_files":1,"assertion_results":10,"passed_assertions":10,"failed_assertions":0,"pass_rate":1.0,"total_tokens":10,"input_tokens":5,"output_tokens":5,"max_duration_ms":20,"total_duration_ms":20}}}"#
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
            vec![
                "::error file=runs/run-001/workspace/grading.json,line=2::assertion failed: missing chart"
                    .to_string()
            ]
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

        std::env::remove_var("GITHUB_ACTIONS");
        assert!(format_github_annotations(&violations).len() == 1);
        emit_github_annotations(&violations);

        std::env::set_var("GITHUB_ACTIONS", "true");
        emit_github_annotations(&violations);
        std::env::remove_var("GITHUB_ACTIONS");
    }

    fn write_metrics_report(dir: &Path, metrics: ReportMetrics) {
        fs::create_dir_all(dir.join("runs/run-001/workspace")).unwrap();
        let pass_rate = metrics.pass_rate;
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
                "{{\n  \"schema_version\": \"1.0\",\n  \"assertion_results\": [\n{}\n  ],\n  \"summary\": {{ \"passed\": {passed}, \"failed\": {failed}, \"total\": {}, \"pass_rate\": {pass_rate} }}\n}}",
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
                schema_version: SCHEMA_VERSION.to_string(),
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
                    ci: None,
                    iteration: 1,
                },
                suite: SuiteSection {
                    skill_name: "demo".to_string(),
                    skill_path: "demo".to_string(),
                    skill_hash: "sha256:abc".to_string(),
                    evals_path: "demo/evals/evals.json".to_string(),
                    evals_hash: "sha256:def".to_string(),
                    old_skill_path: None,
                    old_skill_hash: None,
                },
                dimensions: crate::agentskills::report::DimensionsSection {
                    eval_cases: Vec::new(),
                    assertions: Vec::new(),
                    skill_revisions: Vec::new(),
                    model_configs: Vec::new(),
                    scenarios: Vec::new(),
                    graders: Vec::new(),
                },
                runs: vec![RunRecord {
                    id: "run-001".to_string(),
                    eval_case_id: "case-a".to_string(),
                    eval_slug: "case-a".to_string(),
                    iteration: 1,
                    mirror_path: "iteration-1/eval-case-a/with_skill/".to_string(),
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
                        cost_usd: None,
                    },
                    cache: None,
                    skill_integrity: None,
                    warnings: Vec::new(),
                }],
                assertion_results: Vec::new(),
                summaries: SummariesSection {
                    by_scenario: Vec::new(),
                    human_feedback: Default::default(),
                },
                comparisons: Vec::new(),
                improvement_feedback: Default::default(),
                iteration_summary: None,
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
        assert!((metrics.pass_rate - 0.5).abs() < 0.0001);
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
}
