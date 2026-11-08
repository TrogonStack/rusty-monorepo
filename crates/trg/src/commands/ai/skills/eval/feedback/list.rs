use std::path::PathBuf;

use crate::agentskills::feedback::list_runs_needing_review;
use crate::commands::ai::skills::eval::print_json;
use crate::exit_code::ExitCode;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback list --report-dir ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval feedback list --report-dir /absolute/path/to/report
")]
pub struct FeedbackListArgs {
    #[arg(long, help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl FeedbackListArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self) -> ExitCode {
        let pending = match list_runs_needing_review(&self.report_dir) {
            Ok(pending) => pending,
            Err(e) => {
                eprintln!("Failed to list runs needing review: {}", e);
                return ExitCode::InfrastructureFailure;
            }
        };

        if self.output_format.is_json() {
            #[derive(Serialize)]
            struct FeedbackListDocument<'a> {
                report_dir: String,
                pending: &'a [String],
            }
            let document = FeedbackListDocument {
                report_dir: self.report_dir.display().to_string(),
                pending: &pending,
            };
            return print_json(&document, ExitCode::Success);
        }

        if pending.is_empty() {
            println!("All runs have feedback.json");
        } else {
            println!("Runs needing review:");
            for run_id in pending {
                println!("  {run_id}");
            }
        }

        ExitCode::Success
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
