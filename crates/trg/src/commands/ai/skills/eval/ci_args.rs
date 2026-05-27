use std::path::PathBuf;

use crate::agentskills::ci::{CiPolicy, ThresholdConfig};
use clap::Args;

#[derive(Args, Default)]
pub struct EvalCiArgs {
    #[arg(
        long = "strict-ci",
        help = "Fail CI on runner failures, failed assertions, missing grading, and baseline regressions"
    )]
    pub strict_ci: bool,

    #[arg(long, help = "Fail when any runner invocation fails")]
    pub fail_on_runner_failure: bool,

    #[arg(long, help = "Fail when any assertion result failed")]
    pub fail_on_failed_assertions: bool,

    #[arg(long, help = "Fail when a completed run workspace has no grading.json")]
    pub fail_on_missing_grading: bool,

    #[arg(long, help = "Fail when pass rate regresses below the baseline report")]
    pub fail_on_pass_rate_regression: bool,

    #[arg(long, help = "Fail when total tokens exceed the baseline report")]
    pub fail_on_token_regression: bool,

    #[arg(long, help = "Fail when max duration exceeds the baseline report")]
    pub fail_on_duration_regression: bool,

    #[arg(long, value_name = "RATE", help = "Minimum required pass rate between 0.0 and 1.0")]
    pub min_pass_rate: Option<f64>,

    #[arg(long, value_name = "N", help = "Maximum allowed total tokens across all runs")]
    pub max_tokens: Option<u64>,

    #[arg(long, value_name = "N", help = "Maximum allowed input tokens across all runs")]
    pub max_input_tokens: Option<u64>,

    #[arg(long, value_name = "N", help = "Maximum allowed output tokens across all runs")]
    pub max_output_tokens: Option<u64>,

    #[arg(long, value_name = "MS", help = "Maximum allowed duration in milliseconds for any run")]
    pub max_duration_ms: Option<u64>,

    #[arg(
        long,
        value_name = "REPORT_DIR",
        help = "Baseline report directory for regression comparisons"
    )]
    pub baseline: Option<PathBuf>,
}

impl EvalCiArgs {
    pub fn policy(&self) -> CiPolicy {
        if self.strict_ci {
            CiPolicy::strict_ci()
        } else {
            CiPolicy {
                fail_on_runner_failure: self.fail_on_runner_failure,
                fail_on_failed_assertions: self.fail_on_failed_assertions,
                fail_on_missing_grading: self.fail_on_missing_grading,
                fail_on_pass_rate_regression: self.fail_on_pass_rate_regression,
                fail_on_token_regression: self.fail_on_token_regression,
                fail_on_duration_regression: self.fail_on_duration_regression,
            }
        }
    }

    pub fn thresholds(&self) -> ThresholdConfig {
        ThresholdConfig {
            min_pass_rate: self.min_pass_rate,
            max_tokens: self.max_tokens,
            max_input_tokens: self.max_input_tokens,
            max_output_tokens: self.max_output_tokens,
            max_duration_ms: self.max_duration_ms,
            baseline: self.baseline.clone(),
        }
    }
}
