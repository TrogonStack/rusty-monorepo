use std::path::PathBuf;

use crate::agentskills::evals::EvalDirName;
use crate::agentskills::improvement_bundle::{write_improvement_bundle, NextIterationOptions};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

use super::print_json;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval next-iteration --report-dir ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval next-iteration --report-dir ./report --allow-eval-suite-drift
")]
pub struct NextIterationArgs {
    #[arg(
        long,
        help = "Path to the previous iteration report directory containing report.json"
    )]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "DIR",
        help = "Skill directory used to detect eval suite drift (defaults to skill_path from report.json)"
    )]
    pub skill_dir: Option<PathBuf>,

    #[arg(
        long,
        value_name = "NAME",
        help = "Directory under --skill-dir the current suite is resolved from, for drift detection (default: evals)"
    )]
    pub eval_dir: Option<EvalDirName>,

    #[arg(
        long,
        help = "Suppress the warning when the current evals/evals.json hash differs from the prior iteration"
    )]
    pub allow_eval_suite_drift: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl NextIterationArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let options = NextIterationOptions {
            allow_eval_suite_drift: self.allow_eval_suite_drift,
            skill_dir: self.skill_dir,
            eval_dir: self.eval_dir,
            ..NextIterationOptions::default()
        };

        let output = match write_improvement_bundle(&self.report_dir, options) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("Failed to build improvement bundle: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };

        if self.output_format.is_json() {
            #[derive(Serialize)]
            struct NextIterationDocument {
                output_dir: String,
                markdown_path: String,
                json_path: String,
            }
            let document = NextIterationDocument {
                output_dir: output.output_dir.display().to_string(),
                markdown_path: output.markdown_path.display().to_string(),
                json_path: output.json_path.display().to_string(),
            };
            return print_json(&document, ExitCode::Success);
        }

        println!("Improvement bundle written to {}", output.output_dir.display());
        println!("  {}", output.markdown_path.display());
        println!("  {}", output.json_path.display());
        ExitCode::Success
    }
}

#[cfg(test)]
#[path = "next_iteration_tests.rs"]
mod tests;
