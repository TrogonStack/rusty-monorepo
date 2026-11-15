use super::super::*;
use crate::commands::ai::skills::eval::feedback::{
    FeedbackInitArgs, FeedbackLabelArgs, FeedbackListArgs, FeedbackValidateArgs,
};
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
    assert!(help.contains("--previous-report-dir"));
    assert!(help.contains("--output-format"));
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
fn eval_feedback_label_help_includes_examples() {
    let help = long_help::<FeedbackLabelArgs>("label", "Record a human verdict on one graded assertion");
    assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    assert!(help.contains("--verdict"));
    assert!(help.contains("--assertion"));
}

#[test]
fn eval_grader_agreement_help_includes_examples() {
    let help = long_help::<GraderAgreementArgs>("grader-agreement", "Check graders against human-labelled verdicts");
    assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    assert!(help.contains("--min-agreement"));
}

#[test]
fn eval_html_report_help_includes_examples() {
    let help = long_help::<HtmlReportArgs>(
        "html-report",
        "Render a local-only, self-contained HTML report over a report bundle",
    );
    assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    assert!(help.contains("--output-format"));
}

#[test]
fn eval_next_iteration_help_includes_examples() {
    let help = long_help::<NextIterationArgs>(
        "next-iteration",
        "Build an improvement bundle from a prior iteration for skill revision",
    );
    assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    assert!(help.contains("--report-dir"));
    assert!(help.contains("--allow-eval-suite-drift"));
}

#[test]
fn eval_scaling_help_includes_examples() {
    let help = long_help::<ScalingArgs>(
        "scaling",
        "Check whether a stronger configuration scored higher across ordered report bundles",
    );
    assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
    assert!(help.contains("--fail-on-regression"));
    assert!(help.contains("--failed-runs"));
}
