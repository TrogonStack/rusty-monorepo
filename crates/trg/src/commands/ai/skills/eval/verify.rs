use std::path::PathBuf;

use crate::agentskills::evals::{check_workspace, WorkspaceCheckOptions};
use crate::fs::FileSystem;
use clap::{Args, ValueEnum};

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum VerifyMode {
    /// Tolerate missing grading files and surface failed assertions without erroring.
    Lenient,
    /// Require at least one grading.json and fail on failed assertions.
    Strict,
}

impl VerifyMode {
    fn into_options(self) -> WorkspaceCheckOptions {
        match self {
            Self::Lenient => WorkspaceCheckOptions {
                require_grading: false,
                fail_on_failed_assertions: false,
            },
            Self::Strict => WorkspaceCheckOptions {
                require_grading: true,
                fail_on_failed_assertions: true,
            },
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
pub enum ReportFormat {
    Text,
    Json,
}

#[derive(Args)]
pub struct VerifyArgs {
    #[arg(help = "Path to the workspace directory containing grading.json / timing.json")]
    pub workspace: PathBuf,

    #[arg(long, value_enum, default_value_t = VerifyMode::Lenient)]
    pub mode: VerifyMode,

    #[arg(long, value_enum, default_value_t = ReportFormat::Text)]
    pub format: ReportFormat,
}

impl VerifyArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> i32 {
        let report = match check_workspace(&self.workspace, self.mode.into_options()) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Bundle verification failed: {}", e);
                return 1;
            }
        };

        match self.format {
            ReportFormat::Json => match serde_json::to_string_pretty(&report) {
                Ok(json) => println!("{}", json),
                Err(e) => {
                    eprintln!("Failed to serialize report: {}", e);
                    return 1;
                }
            },
            ReportFormat::Text => {
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
        }

        0
    }
}
