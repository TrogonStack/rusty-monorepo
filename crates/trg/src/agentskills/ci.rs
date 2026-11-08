use std::path::{Path, PathBuf};

use serde::Serialize;

use serde::Deserialize;

use super::evals::{check_workspace, EvalError, WorkspaceCheckOptions, WorkspaceCheckReport};
use super::grading::{case_score, describe_pass_rate, GradingFile};
use super::report::RunRecord;
use crate::exit_code::ExitCode;

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
    pub min_case_score: Option<f64>,
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
    pub unsupported_assertions: usize,
    pub excluded_assertions: usize,
    pub ungraded_assertions: usize,
    pub pass_rate: Option<f64>,
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
    NothingScored,
    Ungraded,
    CaseScoreBelowMinimum,
    CaseNothingScored,
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
    pub exit_code: ExitCode,
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
            "failed" | "timeout" | "budget_exhausted" => metrics.failed_runs += 1,
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
    case_scores: &[CaseScoreRecord],
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

    // A threshold nothing was measured against is the one case where silence is the
    // worst answer: the gate an operator asked for reports green over a pass that
    // scored nothing at all, which is how a suite stops being run without anyone
    // noticing. Excluding unsupported results from the rate is still right; having no
    // rate to exclude them from is not the same thing.
    match (thresholds.min_pass_rate, metrics.pass_rate) {
        (Some(minimum), Some(pass_rate)) if pass_rate + f64::EPSILON < minimum => {
            violations.push(CiViolation {
                kind: CiViolationKind::PassRateBelowMinimum,
                message: format!("pass rate {pass_rate:.4} is below minimum {minimum:.4}"),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
        (Some(minimum), None) => {
            violations.push(CiViolation {
                kind: CiViolationKind::NothingScored,
                message: format!(
                    "a minimum pass rate of {minimum:.4} was required, but nothing was scored: {} run(s) completed, {} skipped, {} grading file(s) read",
                    metrics.completed_runs, metrics.skipped_runs, metrics.grading_files
                ),
                run_id: None,
                workspace: None,
                file: None,
                line: None,
            });
        }
        _ => {}
    }

    if metrics.ungraded_assertions > 0 {
        violations.push(CiViolation {
            kind: CiViolationKind::Ungraded,
            message: format!(
                "{} assertion(s) had no grader to attempt them, so the suite measured less than it declared",
                metrics.ungraded_assertions
            ),
            run_id: None,
            workspace: None,
            file: None,
            line: None,
        });
    }

    // A suite's average can stay healthy while one case fails everything it was
    // asked, so this gate is evaluated per case rather than against the metrics
    // above: a single offender must trip it even when nothing else did. A case
    // with no score is not a case that scored zero, so it is its own violation
    // kind rather than being silently read as either a pass or a failure.
    if let Some(minimum) = thresholds.min_case_score {
        for record in case_scores {
            match record.case_score {
                Some(score) if score + f64::EPSILON < minimum => {
                    violations.push(CiViolation {
                        kind: CiViolationKind::CaseScoreBelowMinimum,
                        message: format!(
                            "case score {score:.4} in '{}' is below minimum {minimum:.4}",
                            record.workspace
                        ),
                        run_id: record.run_id.clone(),
                        workspace: Some(record.workspace.clone()),
                        file: Some(record.file.clone()),
                        line: None,
                    });
                }
                Some(_) => {}
                None => {
                    violations.push(CiViolation {
                        kind: CiViolationKind::CaseNothingScored,
                        message: format!(
                            "a minimum case score of {minimum:.4} was required for '{}', but nothing was scored there",
                            record.workspace
                        ),
                        run_id: record.run_id.clone(),
                        workspace: Some(record.workspace.clone()),
                        file: Some(record.file.clone()),
                        line: None,
                    });
                }
            }
        }
    }

    if let Some(maximum) = thresholds.max_tokens {
        if metrics.total_tokens > maximum {
            violations.push(CiViolation {
                kind: CiViolationKind::TokenBudgetExceeded,
                message: format!("total tokens {} exceeds maximum {}", metrics.total_tokens, maximum),
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
                message: format!("input tokens {} exceeds maximum {}", metrics.input_tokens, maximum),
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
                message: format!("output tokens {} exceeds maximum {}", metrics.output_tokens, maximum),
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
        let regressed = match (metrics.pass_rate, baseline.pass_rate) {
            (Some(current), Some(previous)) => current + f64::EPSILON < previous,
            _ => false,
        };
        if policy.fail_on_pass_rate_regression && regressed {
            violations.push(CiViolation {
                kind: CiViolationKind::PassRateRegression,
                message: format!(
                    "pass rate {} regressed from baseline {}",
                    describe_pass_rate(metrics.pass_rate),
                    describe_pass_rate(baseline.pass_rate)
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
            if result.passed || !result.is_scored() {
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

/// One case's own score, read back from the grading.json it was written to
/// rather than from any cached field, for the same reason the rest of this
/// module recomputes metrics from disk: a report.json that outlived the
/// grading.json it once matched would otherwise report a gate result nothing
/// on disk still backs.
#[derive(Debug, Clone)]
pub struct CaseScoreRecord {
    pub run_id: Option<String>,
    pub workspace: String,
    pub file: String,
    pub case_score: Option<f64>,
}

pub fn collect_case_scores(report_dir: &Path) -> Result<Vec<CaseScoreRecord>, EvalError> {
    let runs = load_report_runs(report_dir)?;
    let mut records = Vec::new();

    for run in &runs {
        let workspace = report_dir.join(&run.paths.workspace);
        collect_case_scores_in_workspace(
            &workspace,
            Some(run.id.clone()),
            workspace.display().to_string(),
            &mut records,
        )?;
    }

    Ok(records)
}

pub fn collect_case_scores_in_workspace(
    workspace: &Path,
    run_id: Option<String>,
    workspace_label: String,
    records: &mut Vec<CaseScoreRecord>,
) -> Result<(), EvalError> {
    if !workspace.is_dir() {
        return Ok(());
    }

    let mut grading_files = Vec::new();
    super::evals::collect_named_files(workspace, "grading.json", &mut grading_files)?;

    for grading_path in grading_files {
        let content = std::fs::read_to_string(&grading_path)?;
        let grading: GradingFile = serde_json::from_str(&content)?;
        records.push(CaseScoreRecord {
            run_id: run_id.clone(),
            workspace: workspace_label.clone(),
            file: grading_path.display().to_string(),
            case_score: case_score(&grading.assertion_results),
        });
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

fn github_annotations_for(violations: &[CiViolation], environment: &impl Fn(&str) -> Option<String>) -> Vec<String> {
    if environment("GITHUB_ACTIONS").as_deref() == Some("true") {
        format_github_annotations(violations)
    } else {
        Vec::new()
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "These lines are the CLI report and GitHub annotation protocol consumed from standard output."
    )
)]
pub fn emit_github_annotations(violations: &[CiViolation]) {
    for line in github_annotations_for(violations, &|name| std::env::var(name).ok()) {
        println!("{line}");
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "These lines are the CLI report and GitHub annotation protocol consumed from standard output."
    )
)]
pub fn print_human_summary(check: &CiCheckResult) {
    match check.metrics.pass_rate {
        Some(pass_rate) => println!(
            "assertions: {}/{} passed ({:.2}%)",
            check.metrics.passed_assertions,
            check.metrics.assertion_results,
            pass_rate * 100.0
        ),
        None => println!("assertions: none scored"),
    }
    if check.metrics.unsupported_assertions > 0 {
        println!(
            "unsupported: {} (not graded on this runner, excluded from the pass rate)",
            check.metrics.unsupported_assertions
        );
    }
    if check.metrics.excluded_assertions > 0 {
        println!(
            "excluded: {} (presuppose the skill, so they score in neither arm)",
            check.metrics.excluded_assertions
        );
    }
    if check.metrics.ungraded_assertions > 0 {
        println!(
            "ungraded: {} (no grader could attempt these, so the suite measured less than it declared)",
            check.metrics.ungraded_assertions
        );
    }
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
    metrics.unsupported_assertions += workspace.unsupported_assertions;
    metrics.excluded_assertions += workspace.excluded_assertions;
    metrics.ungraded_assertions += workspace.ungraded_assertions;
}

fn compute_pass_rate(passed: usize, total: usize) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some(passed as f64 / total as f64)
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
    assertion: String,
    passed: bool,
    #[serde(default)]
    unsupported: Option<String>,
    #[serde(default)]
    excluded: Option<String>,
    #[serde(default)]
    ungraded: Option<String>,
}

impl AssertionForAnnotations {
    fn is_scored(&self) -> bool {
        self.unsupported.is_none() && self.excluded.is_none() && self.ungraded.is_none()
    }
}

#[cfg(test)]
#[path = "ci_tests.rs"]
mod tests;
