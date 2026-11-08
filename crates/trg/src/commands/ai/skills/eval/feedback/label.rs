use std::path::PathBuf;

use crate::agentskills::feedback::{record_assertion_verdict, FeedbackLabelReport, HumanVerdict};
use crate::agentskills::report::sync_human_feedback;
use crate::commands::ai::skills::eval::print_json;
use crate::exit_code::ExitCode;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

/// The same shape `FeedbackLabelReport` reports, plus the report directory the sibling
/// `feedback` subcommands already echo back in their JSON output.
#[derive(Debug, Serialize)]
struct FeedbackLabelOutput<'a> {
    report_dir: String,
    #[serde(flatten)]
    report: &'a FeedbackLabelReport,
}

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback label --report-dir ./report --run run-001 --assertion case-a:g0 --verdict pass

  $ trg ai skills eval feedback label --report-dir ./report --run run-001 --assertion case-a:g0 \\
      --verdict fail --rationale 'evidence quoted the wrong file'
")]
pub struct FeedbackLabelArgs {
    #[arg(long, help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "RUN_ID",
        help = "The run whose grading.json the assertion was scored in"
    )]
    pub run: String,

    #[arg(
        long,
        value_name = "ASSERTION_ID",
        help = "The stable assertion id (<eval-case-id>:g<index>) to label"
    )]
    pub assertion: String,

    #[arg(long, value_enum, help = "The human reviewer's verdict on this assertion")]
    pub verdict: HumanVerdict,

    #[arg(long, help = "Why the reviewer reached this verdict, recorded alongside it")]
    pub rationale: Option<String>,

    #[arg(
        long,
        value_name = "EMAIL",
        help = "Reviewer identity recorded in feedback.json when it does not exist yet (defaults to git user.email)"
    )]
    pub reviewer: Option<String>,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl FeedbackLabelArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self) -> ExitCode {
        let report = match record_assertion_verdict(
            &self.report_dir,
            &self.run,
            &self.assertion,
            self.verdict,
            self.rationale,
            self.reviewer.as_deref(),
        ) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Failed to record assertion verdict: {}", e);
                return ExitCode::InfrastructureFailure;
            }
        };

        if let Err(e) = sync_human_feedback(&self.report_dir) {
            eprintln!("Failed to sync feedback summary into report.json: {}", e);
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            let document = FeedbackLabelOutput {
                report_dir: self.report_dir.display().to_string(),
                report: &report,
            };
            return print_json(&document, ExitCode::Success);
        }

        println!(
            "Recorded verdict '{}' for {} in run {}{}",
            report.verdict,
            report.assertion_id,
            report.run_id,
            if report.replaced { " (replaced)" } else { "" }
        );
        ExitCode::Success
    }
}

#[cfg(test)]
#[path = "label_tests.rs"]
mod tests;
