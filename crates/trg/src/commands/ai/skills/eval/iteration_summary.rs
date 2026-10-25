use std::path::PathBuf;

use crate::agentskills::benchmark::FailedRunsMode;
use crate::agentskills::efficiency::{EfficiencyMetric, EfficiencyTolerances, IncreaseTolerance};
use crate::agentskills::headroom::HeadroomThreshold;
use crate::agentskills::iteration_summary::{
    build_iteration_summary_document, print_human_summary, recommendation_label, write_iteration_summary,
    IterationSummaryOptions, KeepOrRevertSection, Recommendation,
};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::{Args, ValueEnum};

use super::print_report_dir;

/// A keep-or-revert verdict that should fail the command for CI.
///
/// Unset gates on nothing: a first iteration with no `--previous` to compare against has no
/// verdict to match, so it always exits successfully regardless of `--fail-on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum FailOn {
    /// The test split regressed against `--previous`.
    Revert,
    /// The train split improved while the test split did not, which is what a skill that
    /// learned the suite instead of the task looks like.
    Overfitting,
    /// A split's with_skill runs cost more per run than `--max-cost-increase` allows.
    CostRegression,
    /// A split's with_skill runs took longer per run than `--max-duration-increase` allows.
    LatencyRegression,
    /// A split's with_skill runs spent more total tokens per run than
    /// `--max-token-increase` allows.
    TokenRegression,
}

impl FailOn {
    /// Why this gate fired against `section`, or nothing when it did not.
    fn reason(self, section: &KeepOrRevertSection) -> Option<String> {
        let recommendation_gate = |expected: Recommendation| {
            (section.recommendation == expected).then(|| {
                format!(
                    "recommendation is {}, which --fail-on was told to fail on",
                    recommendation_label(section.recommendation)
                )
            })
        };
        let efficiency_gate = |metric: EfficiencyMetric| {
            let splits = section.efficiency.as_ref()?.regressed_splits(metric);
            (!splits.is_empty()).then(|| {
                let names: Vec<&str> = splits.iter().map(|split| split.as_str()).collect();
                format!(
                    "{} regressed on the {} split, which --fail-on was told to fail on",
                    metric.as_str(),
                    names.join(", ")
                )
            })
        };
        match self {
            Self::Revert => recommendation_gate(Recommendation::Revert),
            Self::Overfitting => recommendation_gate(Recommendation::SuspectedOverfitting),
            Self::CostRegression => efficiency_gate(EfficiencyMetric::Cost),
            Self::LatencyRegression => efficiency_gate(EfficiencyMetric::Duration),
            Self::TokenRegression => efficiency_gate(EfficiencyMetric::Tokens),
        }
    }
}

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval iteration-summary ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval iteration-summary ./report --previous ./artifacts/my-skill/prior-report

  $ trg ai skills eval iteration-summary ./report --output-format json --failed-runs exclude

  $ trg ai skills eval iteration-summary ./report --fail-on revert,overfitting

  $ trg ai skills eval iteration-summary ./report --fail-on revert,cost_regression --max-cost-increase 0.1
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
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human-readable table or as the iteration-summary.json document on stdout"
    )]
    pub output_format: OutputFormat,

    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        value_name = "VERDICT",
        help = "Exit with a gate failure on any of these keep-or-revert verdicts: revert, overfitting, cost_regression, latency_regression, token_regression. Comma-separated; unset never gates"
    )]
    pub fail_on: Vec<FailOn>,

    #[arg(
        long,
        value_name = "PROPORTION",
        default_value_t = IncreaseTolerance::default(),
        help = "Per-run cost increase over the previous iteration's median, as a proportion of it, that still does not count as a cost regression"
    )]
    pub max_cost_increase: IncreaseTolerance,

    #[arg(
        long,
        value_name = "PROPORTION",
        default_value_t = IncreaseTolerance::default(),
        help = "Per-run duration increase over the previous iteration's median, as a proportion of it, that still does not count as a latency regression"
    )]
    pub max_duration_increase: IncreaseTolerance,

    #[arg(
        long,
        value_name = "PROPORTION",
        default_value_t = IncreaseTolerance::default(),
        help = "Per-run total token increase over the previous iteration's median, as a proportion of it, that still does not count as a token regression"
    )]
    pub max_token_increase: IncreaseTolerance,

    #[arg(
        long,
        value_name = "PROPORTION",
        default_value_t = HeadroomThreshold::default(),
        help = "The with_skill arm's Wilson lower bound is reported as saturated at or above this proportion in (0, 1]"
    )]
    pub headroom_threshold: HeadroomThreshold,

    #[arg(
        long,
        help = "Drop test-split eval case ids and assertion text from the output, keeping only aggregate counts, deltas and intervals; for a caller whose keep-or-revert decision must not be shaped by the held-out split"
    )]
    pub withhold_test_detail: bool,
}

impl IterationSummaryArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let options = IterationSummaryOptions {
            failed_runs: self.failed_runs,
            previous_report_dir: self.previous,
            headroom_threshold: self.headroom_threshold,
            withhold_test_detail: self.withhold_test_detail,
            efficiency_tolerances: EfficiencyTolerances {
                cost: self.max_cost_increase,
                duration: self.max_duration_increase,
                tokens: self.max_token_increase,
            },
        };

        let document = match build_iteration_summary_document(&self.report_dir, options) {
            Ok(document) => document,
            Err(error) => {
                eprintln!("Failed to build iteration summary: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };

        if let Err(error) = write_iteration_summary(&self.report_dir, &document) {
            eprintln!("Failed to write iteration-summary.json: {error}");
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            match serde_json::to_string_pretty(&document) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("Failed to serialize iteration summary: {error}");
                    return ExitCode::InfrastructureFailure;
                }
            }
        } else {
            print_human_summary(&document);
            print_report_dir(&self.report_dir);
        }

        let reasons: Vec<String> = match &document.keep_or_revert {
            Some(section) => self
                .fail_on
                .iter()
                .filter_map(|fail_on| fail_on.reason(section))
                .collect(),
            None => Vec::new(),
        };
        for reason in &reasons {
            eprintln!("iteration-summary: {reason}");
        }

        ExitCode::from_gate(reasons.is_empty())
    }
}
