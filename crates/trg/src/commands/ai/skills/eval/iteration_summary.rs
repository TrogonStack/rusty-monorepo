use std::path::PathBuf;

use crate::agentskills::benchmark::FailedRunsMode;
use crate::agentskills::iteration_summary::{
    build_iteration_summary_document, print_human_summary, write_iteration_summary, IterationSummaryOptions,
};
use crate::fs::FileSystem;
use clap::Args;

use super::print_report_dir;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval iteration-summary ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval iteration-summary ./report --previous ./artifacts/my-skill/prior-report

  $ trg ai skills eval iteration-summary ./report --json --failed-runs exclude
")]
pub struct IterationSummaryArgs {
    #[arg(help = "Path to the report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "DIR",
        help = "Previous iteration report directory for cross-iteration comparison (auto-detected when omitted)"
    )]
    pub previous: Option<PathBuf>,

    #[arg(
        long,
        value_enum,
        default_value_t = FailedRunsMode::Bucket,
        help = "How to treat runner failures when aggregating pass rates"
    )]
    pub failed_runs: FailedRunsMode,

    #[arg(
        long,
        help = "Emit iteration-summary.json to stdout instead of a human-readable table"
    )]
    pub json: bool,
}

impl IterationSummaryArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> i32 {
        let options = IterationSummaryOptions {
            failed_runs: self.failed_runs,
            previous_report_dir: self.previous,
        };

        let document = match build_iteration_summary_document(&self.report_dir, options) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("Failed to build iteration summary: {error}");
                return 1;
            }
        };

        if let Err(error) = write_iteration_summary(&self.report_dir, &document) {
            eprintln!("Failed to write iteration-summary.json: {error}");
            return 1;
        }

        if self.json {
            match serde_json::to_string_pretty(&document) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("Failed to serialize iteration summary: {error}");
                    return 1;
                }
            }
        } else {
            print_human_summary(&document);
            print_report_dir(&self.report_dir);
        }

        0
    }
}
