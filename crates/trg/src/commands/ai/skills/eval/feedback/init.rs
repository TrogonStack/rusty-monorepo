use std::path::PathBuf;

use crate::agentskills::feedback::init_feedback;
use crate::agentskills::report::sync_human_feedback;
use crate::commands::ai::skills::eval::print_json;
use crate::exit_code::ExitCode;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback init --report-dir ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval feedback init --report-dir ./report --reviewer reviewer@example.com
")]
pub struct FeedbackInitArgs {
    #[arg(long, help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "EMAIL",
        help = "Reviewer identity recorded in feedback.json (defaults to git user.email)"
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

impl FeedbackInitArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self) -> ExitCode {
        let report = match init_feedback(&self.report_dir, self.reviewer.as_deref()) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Failed to initialize feedback artifacts: {}", e);
                return ExitCode::InfrastructureFailure;
            }
        };

        if let Err(e) = sync_human_feedback(&self.report_dir) {
            eprintln!("Failed to sync feedback summary into report.json: {}", e);
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            #[derive(Serialize)]
            struct FeedbackInitDocument {
                report_dir: String,
                created: usize,
                skipped: usize,
            }
            let document = FeedbackInitDocument {
                report_dir: self.report_dir.display().to_string(),
                created: report.created,
                skipped: report.skipped,
            };
            return print_json(&document, ExitCode::Success);
        }

        println!(
            "Initialized feedback for {} run(s) ({} already existed)",
            report.created, report.skipped
        );
        ExitCode::Success
    }
}

#[cfg(test)]
#[path = "init_tests.rs"]
mod tests;
