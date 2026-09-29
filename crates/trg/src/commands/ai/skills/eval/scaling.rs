use std::path::PathBuf;

use crate::agentskills::benchmark::FailedRunsMode;
use crate::agentskills::exit_code::ExitCode;
use crate::agentskills::scaling::{
    any_regression, build_scaling_document, write_scaling, ScalingDocument, ScalingStep, ScalingVerdict,
};
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;

use super::print_report_dir;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval scaling ./report-small ./report-medium ./report-large

  $ trg ai skills eval scaling ./report-small ./report-large --output-format json

  $ trg ai skills eval scaling ./report-small ./report-large --fail-on-regression
")]
pub struct ScalingArgs {
    #[arg(
        required = true,
        num_args = 2..,
        help = "Report bundle directories, ordered weakest to strongest configuration. This command never ranks models itself; the order is the operator's claim"
    )]
    pub report_dirs: Vec<PathBuf>,

    #[arg(
        long,
        value_enum,
        default_value_t = FailedRunsMode::Bucket,
        help = "How to treat runner failures when aggregating pass rates"
    )]
    pub failed_runs: FailedRunsMode,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as the scaling.json document on stdout"
    )]
    pub output_format: OutputFormat,

    #[arg(
        long,
        help = "Exit with the gate-failed code when any step, on any split, or the end-to-end comparison regressed"
    )]
    pub fail_on_regression: bool,
}

impl ScalingArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let document = match build_scaling_document(&self.report_dirs, self.failed_runs) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("Failed to build model scaling report: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };

        let write_dir = self.report_dirs.last().expect("clap requires at least two report dirs");
        if let Err(error) = write_scaling(write_dir, &document) {
            eprintln!("Failed to write scaling.json: {error}");
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            match serde_json::to_string_pretty(&document) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("Failed to serialize model scaling report: {error}");
                    return ExitCode::InfrastructureFailure;
                }
            }
        } else {
            print_human_summary(&document);
            print_report_dir(write_dir);
        }

        if self.fail_on_regression && any_regression(&document) {
            eprintln!("scaling: a stronger configuration scored lower somewhere in the comparison");
            return ExitCode::GateFailed;
        }

        ExitCode::Success
    }
}

fn print_human_summary(document: &ScalingDocument) {
    println!("Model scaling across {} bundles:", document.bundles.len());
    for step in &document.steps {
        print_step(step);
    }
    println!("End to end:");
    print_step(&document.end_to_end);
}

fn print_step(step: &ScalingStep) {
    println!(
        "  {} -> {}: {}",
        step.weaker.report_id,
        step.stronger.report_id,
        describe_verdict(&step.comparison.overall)
    );
    for (split, verdict) in &step.comparison.by_split {
        println!("    {}: {}", split.as_str(), describe_verdict(verdict));
    }
    for outlier in &step.outliers {
        println!(
            "    outlier: {} scored {:.3} -> {:.3}",
            outlier.eval_case_id, outlier.weaker_pass_rate, outlier.stronger_pass_rate
        );
    }
}

fn describe_verdict(verdict: &ScalingVerdict) -> String {
    match verdict {
        ScalingVerdict::Improved(delta) => {
            format!(
                "improved ({:.3} -> {:.3})",
                delta.weaker_pass_rate, delta.stronger_pass_rate
            )
        }
        ScalingVerdict::Regressed(delta) => {
            format!(
                "regressed ({:.3} -> {:.3})",
                delta.weaker_pass_rate, delta.stronger_pass_rate
            )
        }
        ScalingVerdict::Flat(delta) => {
            format!(
                "flat ({:.3} -> {:.3})",
                delta.weaker_pass_rate, delta.stronger_pass_rate
            )
        }
        ScalingVerdict::NoRuns => "no with_skill runs on either side".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_includes_examples() {
        let help = ScalingArgs::augment_args(clap::Command::new("scaling").about("scaling"))
            .render_long_help()
            .to_string();
        assert!(help.contains("Examples:"), "missing Examples section:\n{help}");
        assert!(help.contains("--fail-on-regression"));
        assert!(help.contains("--failed-runs"));
    }
}
