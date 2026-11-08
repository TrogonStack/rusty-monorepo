use std::path::{Path, PathBuf};

use crate::agentskills::benchmark::{
    build_benchmark, write_benchmark, BenchmarkDocument, BenchmarkOptions, FailedRunsMode,
};
use crate::agentskills::evals::EvalSplit;
use crate::agentskills::grading::telemetry::{eval_error_type, phase_span, record_error};
use crate::agentskills::headroom::{describe_headroom_warning, HeadroomThreshold};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;

use super::print_report_dir;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval benchmark --report-dir ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval benchmark --report-dir ./report --failed-runs exclude

  $ trg ai skills eval benchmark --report-dir ./report --previous-report-dir ./artifacts/my-skill/prior-report --output-format json

  $ trg ai skills eval benchmark --report-dir /absolute/path/to/report
")]
pub struct BenchmarkArgs {
    #[arg(long, help = "Path to the report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "REPORT_DIR",
        help = "Previous iteration report directory for cross-iteration drift detection (auto-detected when omitted)"
    )]
    pub previous_report_dir: Option<PathBuf>,

    #[arg(
        long,
        value_enum,
        default_value_t = FailedRunsMode::Bucket,
        help = "How to treat runner failures when aggregating pass rates"
    )]
    pub failed_runs: FailedRunsMode,

    #[arg(
        long,
        help = "Suppress the warning when the eval suite hash differs from the previous iteration report"
    )]
    pub allow_eval_suite_drift: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as the benchmark.json document on stdout"
    )]
    pub output_format: OutputFormat,

    #[arg(
        long,
        value_name = "PROPORTION",
        default_value_t = HeadroomThreshold::default(),
        help = "The with_skill arm's Wilson lower bound is reported as saturated at or above this proportion in (0, 1]"
    )]
    pub headroom_threshold: HeadroomThreshold,
}

impl BenchmarkArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let options = BenchmarkOptions {
            failed_runs: self.failed_runs,
            allow_eval_suite_drift: self.allow_eval_suite_drift,
            previous_report_dir: self.previous_report_dir,
            headroom_threshold: self.headroom_threshold,
        };

        let (code, document) = benchmark_report_dir_with_document(&self.report_dir, options);
        if !code.is_success() {
            return code;
        }

        if self.output_format.is_json() {
            if let Some(document) = document {
                match serde_json::to_string_pretty(&document) {
                    Ok(json) => println!("{json}"),
                    Err(error) => {
                        eprintln!("Failed to serialize benchmark: {error}");
                        return ExitCode::InfrastructureFailure;
                    }
                }
            }
        } else {
            if let Some(document) = &document {
                print_headroom_warnings(document);
            }
            print_report_dir(&self.report_dir);
        }
        ExitCode::Success
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn print_headroom_warnings(document: &BenchmarkDocument) {
    if let Some(headroom) = &document.headroom {
        println!("{}", describe_headroom_warning("overall", headroom));
    }
    for split in EvalSplit::ALL {
        if let Some(headroom) = document
            .by_split
            .get(&split)
            .and_then(|split_benchmark| split_benchmark.headroom.as_ref())
        {
            println!("{}", describe_headroom_warning(split.as_str(), headroom));
        }
    }
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub(crate) fn benchmark_report_dir_with_document(
    report_dir: &Path,
    options: BenchmarkOptions,
) -> (ExitCode, Option<crate::agentskills::benchmark::BenchmarkDocument>) {
    let span = phase_span!("benchmark");
    let _entered = span.enter();
    let document = match build_benchmark(report_dir, options) {
        Ok(document) => document,
        Err(e) => {
            record_error(&span, eval_error_type(&e));
            eprintln!("Failed to build benchmark: {}", e);
            return (ExitCode::InfrastructureFailure, None);
        }
    };

    if let Err(e) = write_benchmark(report_dir, &document) {
        record_error(&span, eval_error_type(&e));
        eprintln!("Failed to write benchmark.json: {}", e);
        return (ExitCode::InfrastructureFailure, None);
    }

    (ExitCode::Success, Some(document))
}

#[cfg(test)]
#[path = "benchmark_tests.rs"]
mod tests;
