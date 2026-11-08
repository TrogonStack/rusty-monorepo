mod benchmark;
mod ci_args;
mod compare;
mod export;
mod feedback;
mod grade;
mod grader_agreement;
mod html_report;
mod init;
mod iteration_summary;
mod mock_server;
mod next_iteration;
mod output;
mod record_mcp;
mod run;
mod scaling;
mod verify;

pub(crate) use output::{print_json, print_report_dir};

use std::path::Path;

use crate::agentskills::ci::{
    collect_case_scores, collect_failed_assertions, collect_missing_grading_workspaces, collect_report_metrics,
    emit_github_annotations, print_human_summary, run_ci_checks, EvalCommandJsonOutput,
};
use crate::agentskills::eval_telemetry::{record_failure, step_span};
use crate::agentskills::evals::WorkspaceCheckReport;
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::{Args, Subcommand};

pub use benchmark::BenchmarkArgs;
pub use compare::CompareArgs;
pub use export::ExportArgs;
pub use feedback::FeedbackArgs;
pub use grade::GradeArgs;
pub use grader_agreement::GraderAgreementArgs;
pub use html_report::HtmlReportArgs;
pub use init::InitArgs;
pub use iteration_summary::IterationSummaryArgs;
pub use mock_server::MockServerArgs;
pub use next_iteration::NextIterationArgs;
pub use record_mcp::RecordMcpArgs;
pub use run::RunArgs;
pub use scaling::ScalingArgs;
pub use verify::VerifyArgs;

#[derive(Args)]
pub struct EvalArgs {
    #[command(subcommand)]
    pub command: EvalCommands,
}

#[derive(Subcommand)]
pub enum EvalCommands {
    /// Run skill evals and write an artifact bundle
    Run(Box<RunArgs>),
    /// Grade a completed eval report bundle
    Grade(GradeArgs),
    /// Verify a generated eval bundle
    Verify(VerifyArgs),
    /// Scaffold evals/evals.json for a skill directory
    Init(InitArgs),
    /// Aggregate grading and timing artifacts into benchmark.json
    Benchmark(BenchmarkArgs),
    /// Summarize assertion stability, skill impact, flakiness, and metric outliers
    IterationSummary(IterationSummaryArgs),
    /// Manage human review feedback artifacts
    Feedback(FeedbackArgs),
    /// Check graders against human-labelled verdicts
    GraderAgreement(GraderAgreementArgs),
    /// Blindly compare scenario outputs within a report directory
    Compare(CompareArgs),
    /// Build an improvement bundle from a prior iteration for skill revision
    NextIteration(NextIterationArgs),
    /// Serve one mocked mcp server's tools over stdio from a resolved mock set
    ///
    /// Not meant to be typed by hand: a run's generated `--mcp-config` is the only caller,
    /// so this is hidden from `--help`.
    #[command(hide = true)]
    MockServer(MockServerArgs),
    /// Render a local-only, self-contained HTML report over a report bundle
    HtmlReport(HtmlReportArgs),
    /// Start a real, operator-declared MCP server and record its answers as fixed mocks
    RecordMcp(RecordMcpArgs),
    /// Check whether a stronger configuration scored higher across ordered report bundles
    Scaling(ScalingArgs),
    /// Replay a finished report bundle as OpenTelemetry traces and evaluation events
    Export(ExportArgs),
}

impl EvalArgs {
    pub fn handle(self, fs: &impl FileSystem) -> ExitCode {
        match self.command {
            EvalCommands::Run(args) => args.handle(fs),
            EvalCommands::Grade(args) => args.handle(fs),
            EvalCommands::Verify(args) => args.handle(fs),
            EvalCommands::Init(args) => args.handle(fs),
            EvalCommands::Benchmark(args) => args.handle(fs),
            EvalCommands::IterationSummary(args) => args.handle(fs),
            EvalCommands::Feedback(args) => args.handle(fs),
            EvalCommands::GraderAgreement(args) => args.handle(fs),
            EvalCommands::Compare(args) => args.handle(fs),
            EvalCommands::NextIteration(args) => args.handle(fs),
            EvalCommands::MockServer(args) => args.handle(),
            EvalCommands::HtmlReport(args) => args.handle(fs),
            EvalCommands::RecordMcp(args) => args.handle(fs),
            EvalCommands::Scaling(args) => args.handle(fs),
            EvalCommands::Export(args) => args.handle(fs),
        }
    }
}

/// What the pass decided, before anything is said about it.
///
/// Kept apart from the printing so the code the process exits with and the code the
/// JSON reports are one value rather than two that agree by habit.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub(crate) fn eval_output(
    report_dir: &Path,
    policy: crate::agentskills::ci::CiPolicy,
    thresholds: &crate::agentskills::ci::ThresholdConfig,
    workspace: Option<WorkspaceCheckReport>,
    budget_exhausted: bool,
) -> Result<EvalCommandJsonOutput, ExitCode> {
    let collecting = step_span("collect report metrics");
    let collected = collecting.in_scope(|| {
        collect_report_metrics(report_dir).map(|metrics| {
            (
                metrics,
                collect_failed_assertions(report_dir).unwrap_or_default(),
                collect_missing_grading_workspaces(report_dir).unwrap_or_default(),
                collect_case_scores(report_dir).unwrap_or_default(),
            )
        })
    });
    let (metrics, failed_assertions, missing_grading, case_scores) = match collected {
        Ok(collected) => collected,
        Err(error) => {
            record_failure(&collecting, "report_unreadable");
            eprintln!("Failed to collect report metrics: {error}");
            return Err(ExitCode::InfrastructureFailure);
        }
    };
    drop(collecting);

    let check = step_span("check ci gates").in_scope(|| {
        run_ci_checks(
            &metrics,
            policy,
            thresholds,
            &failed_assertions,
            &missing_grading,
            &case_scores,
        )
    });

    Ok(EvalCommandJsonOutput {
        report_dir: report_dir.display().to_string(),
        exit_code: ExitCode::from_gate(check.passed).or_budget_exhausted(budget_exhausted),
        check,
        workspace,
    })
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub(crate) fn finish_eval_output(
    report_dir: &Path,
    format: OutputFormat,
    policy: crate::agentskills::ci::CiPolicy,
    thresholds: &crate::agentskills::ci::ThresholdConfig,
    workspace: Option<WorkspaceCheckReport>,
    budget_exhausted: bool,
) -> ExitCode {
    let output = match eval_output(report_dir, policy, thresholds, workspace, budget_exhausted) {
        Ok(output) => output,
        Err(code) => return code,
    };
    step_span("emit github annotations").in_scope(|| emit_github_annotations(&output.check.violations));

    if format.is_json() {
        match serde_json::to_string_pretty(&output) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("Failed to serialize eval output: {error}");
                return ExitCode::InfrastructureFailure;
            }
        }
    } else {
        print_report_dir(report_dir);
        print_human_summary(&output.check);
    }

    output.exit_code
}

#[cfg(test)]
mod tests;
