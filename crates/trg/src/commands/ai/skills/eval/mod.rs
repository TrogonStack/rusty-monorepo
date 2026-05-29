mod benchmark;
mod ci_args;
mod compare;
mod feedback;
mod grade;
mod init;
mod iteration_summary;
mod next_iteration;
mod output;
mod run;
mod verify;

pub(crate) use output::print_report_dir;

use std::path::Path;

use crate::agentskills::ci::{
    collect_failed_assertions, collect_missing_grading_workspaces, collect_report_metrics, emit_github_annotations,
    print_human_summary, run_ci_checks, EvalCommandJsonOutput,
};
use crate::agentskills::evals::WorkspaceCheckReport;
use crate::fs::FileSystem;
use clap::{Args, Subcommand};

pub use benchmark::BenchmarkArgs;
pub use compare::CompareArgs;
pub use feedback::FeedbackArgs;
pub use grade::GradeArgs;
pub use init::InitArgs;
pub use iteration_summary::IterationSummaryArgs;
pub use next_iteration::NextIterationArgs;
pub use run::RunArgs;
pub use verify::VerifyArgs;

#[derive(Args)]
pub struct EvalArgs {
    #[command(subcommand)]
    pub command: EvalCommands,
}

#[derive(Subcommand)]
pub enum EvalCommands {
    /// Run skill evals and write an artifact bundle
    Run(RunArgs),
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
    /// Blindly compare scenario outputs within a report directory
    Compare(CompareArgs),
    /// Build an improvement bundle from a prior iteration for skill revision
    NextIteration(NextIterationArgs),
}

impl EvalArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        match self.command {
            EvalCommands::Run(args) => args.handle(fs),
            EvalCommands::Grade(args) => args.handle(fs),
            EvalCommands::Verify(args) => args.handle(fs),
            EvalCommands::Init(args) => args.handle(fs),
            EvalCommands::Benchmark(args) => args.handle(fs),
            EvalCommands::IterationSummary(args) => args.handle(fs),
            EvalCommands::Feedback(args) => args.handle(fs),
            EvalCommands::Compare(args) => args.handle(fs),
            EvalCommands::NextIteration(args) => args.handle(fs),
        }
    }
}

pub(crate) fn finish_eval_output(
    report_dir: &Path,
    json: bool,
    policy: crate::agentskills::ci::CiPolicy,
    thresholds: &crate::agentskills::ci::ThresholdConfig,
    workspace: Option<WorkspaceCheckReport>,
) -> i32 {
    let metrics = match collect_report_metrics(report_dir) {
        Ok(metrics) => metrics,
        Err(error) => {
            eprintln!("Failed to collect report metrics: {error}");
            return 1;
        }
    };

    let failed_assertions = collect_failed_assertions(report_dir).unwrap_or_default();
    let missing_grading = collect_missing_grading_workspaces(report_dir).unwrap_or_default();
    let check = run_ci_checks(&metrics, policy, thresholds, &failed_assertions, &missing_grading);
    emit_github_annotations(&check.violations);

    let exit_code = if check.passed { 0 } else { 1 };
    if json {
        let output = EvalCommandJsonOutput {
            report_dir: report_dir.display().to_string(),
            exit_code,
            check,
            workspace,
        };
        match serde_json::to_string_pretty(&output) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("Failed to serialize eval output: {error}");
                return 1;
            }
        }
    } else {
        print_report_dir(report_dir);
        print_human_summary(&check);
    }

    exit_code
}

#[cfg(test)]
mod help_tests {
    use super::*;
    use crate::commands::ai::skills::eval::feedback::{FeedbackInitArgs, FeedbackListArgs, FeedbackValidateArgs};
    use clap::{Args, Command};

    fn long_help<T: Args>(name: &'static str, about: &'static str) -> String {
        T::augment_args(Command::new(name).about(about))
            .render_long_help()
            .to_string()
    }

    #[test]
    fn eval_run_help_includes_examples() {
        let help = long_help::<RunArgs>("run", "Run skill evals and write an artifact bundle");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--skill-dir"));
        assert!(help.contains("--out-dir"));
        assert!(help.contains("--grade"), "missing --grade:\n{help}");
        assert!(help.contains("--benchmark"), "missing --benchmark:\n{help}");
        assert!(
            help.contains("run --skill-dir ./my-skill --runner codex --grade --benchmark"),
            "missing one-shot pipeline example:\n{help}"
        );
    }

    #[test]
    fn eval_grade_help_includes_examples() {
        let help = long_help::<GradeArgs>("grade", "Grade a completed eval report bundle");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--grader"));
    }

    #[test]
    fn eval_benchmark_help_includes_examples() {
        let help = long_help::<BenchmarkArgs>("benchmark", "Aggregate grading and timing artifacts");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--failed-runs"));
        assert!(help.contains("--allow-eval-suite-drift"));
    }

    #[test]
    fn eval_iteration_summary_help_includes_examples() {
        let help = long_help::<IterationSummaryArgs>("iteration-summary", "Summarize assertion stability and outliers");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--previous"));
        assert!(help.contains("--json"));
    }

    #[test]
    fn eval_verify_help_includes_examples() {
        let help = long_help::<VerifyArgs>("verify", "Verify a generated eval bundle");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--mode"));
    }

    #[test]
    fn eval_compare_help_includes_examples() {
        let help = long_help::<CompareArgs>("compare", "Blindly compare scenario outputs");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--pair"));
        assert!(help.contains("--allow-eval-suite-drift"));
    }

    #[test]
    fn eval_feedback_init_help_includes_examples() {
        let help = long_help::<FeedbackInitArgs>("init", "Scaffold empty feedback.json");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--reviewer"));
    }

    #[test]
    fn eval_feedback_list_help_includes_examples() {
        let help = long_help::<FeedbackListArgs>("list", "List runs needing review");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    }

    #[test]
    fn eval_init_help_mentions_optional_metadata_fields() {
        let help = long_help::<InitArgs>("init", "Scaffold evals/evals.json for a skill directory");
        assert!(help.contains("timeout_secs"), "missing timeout_secs:\n{help}");
        assert!(
            help.contains("expected_output_files"),
            "missing expected_output_files:\n{help}"
        );
        assert!(help.contains("grader_hints"), "missing grader_hints:\n{help}");
    }

    #[test]
    fn eval_feedback_validate_help_includes_examples() {
        let help = long_help::<FeedbackValidateArgs>("validate", "Validate feedback.json files");
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    }

    #[test]
    fn eval_next_iteration_help_includes_examples() {
        let help = long_help::<NextIterationArgs>(
            "next-iteration",
            "Build an improvement bundle from a prior iteration for skill revision",
        );
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--from"));
        assert!(help.contains("--allow-eval-suite-drift"));
    }
}
