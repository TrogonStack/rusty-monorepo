use std::path::PathBuf;

use crate::agentskills::evals::{check_workspace, WorkspaceCheckOptions};
use crate::fs::FileSystem;
use clap::Args;

#[derive(Args)]
pub struct VerifyArgs {
    #[arg(help = "Path to the workspace directory containing grading.json / timing.json")]
    pub workspace: PathBuf,

    #[arg(
        long,
        help = "Require at least one grading.json in the workspace and fail on failed assertions"
    )]
    pub require_grades: bool,

    #[arg(long, help = "Print a machine-readable JSON report")]
    pub json: bool,
}

impl VerifyArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> i32 {
        let report = match check_workspace(
            &self.workspace,
            WorkspaceCheckOptions {
                require_grading: self.require_grades,
                fail_on_failed_assertions: self.require_grades,
            },
        ) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Bundle verification failed: {}", e);
                return 1;
            }
        };

        if self.json {
            match serde_json::to_string_pretty(&report) {
                Ok(json) => println!("{}", json),
                Err(e) => {
                    eprintln!("Failed to serialize report: {}", e);
                    return 1;
                }
            }
        } else {
            println!("Bundle verified");
            println!("  grading files: {}", report.grading_files);
            println!("  timing files: {}", report.timing_files);
            println!(
                "  assertion results: {}/{} passed ({:.2}%)",
                report.passed_assertions,
                report.assertion_results,
                report.pass_rate * 100.0
            );
        }

        0
    }
}
