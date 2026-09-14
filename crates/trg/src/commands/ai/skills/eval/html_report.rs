use std::path::PathBuf;

use crate::agentskills::exit_code::ExitCode;
use crate::agentskills::html_report::write_html_report;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

use super::{print_json, print_report_dir};

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval html-report ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval html-report /absolute/path/to/report --output-format json
")]
pub struct HtmlReportArgs {
    #[arg(help = "Path to the report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

#[derive(Serialize)]
struct HtmlReportOutcome {
    report_dir: String,
    html_path: String,
}

impl HtmlReportArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        match write_html_report(&self.report_dir) {
            Ok(html_path) => {
                if self.output_format.is_json() {
                    print_json(
                        &HtmlReportOutcome {
                            report_dir: self.report_dir.display().to_string(),
                            html_path: html_path.display().to_string(),
                        },
                        ExitCode::Success,
                    )
                } else {
                    print_report_dir(&self.report_dir);
                    println!("html report: {}", html_path.display());
                    ExitCode::Success
                }
            }
            Err(error) => {
                eprintln!("Failed to write HTML report: {error}");
                ExitCode::InfrastructureFailure
            }
        }
    }
}
