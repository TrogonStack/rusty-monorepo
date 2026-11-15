mod constants;
use constants::GATE_FAILED;
use std::path::PathBuf;

use crate::agentskills::grader_agreement::{
    build_grader_agreement_document, write_grader_agreement, AgreementBucket, AgreementThreshold,
    GraderAgreementDocument,
};
use crate::agentskills::grading::telemetry::{phase_span, record_error};
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;

use super::print_report_dir;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval grader-agreement --report-dir ./report

  $ trg ai skills eval grader-agreement --report-dir ./report --output-format json

  $ trg ai skills eval grader-agreement --report-dir ./report --min-agreement 0.9
")]
pub struct GraderAgreementArgs {
    #[arg(long, help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "PROPORTION",
        help = "Fail when the LLM-grader agreement interval's Wilson lower bound is below this proportion in (0, 1]"
    )]
    pub min_agreement: Option<AgreementThreshold>,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as the grader-agreement.json document on stdout"
    )]
    pub output_format: OutputFormat,
}

impl GraderAgreementArgs {
    #[cfg_attr(
        dylint_lib = "trogon_dylint_lints",
        expect(
            debug_remnants,
            reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
        )
    )]
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let span = phase_span!("grader agreement");
        let _entered = span.enter();
        let document = match build_grader_agreement_document(&self.report_dir) {
            Ok(document) => document,
            Err(error) => {
                record_error(&span, error.error_type());
                eprintln!("Failed to build grader agreement report: {error}");
                return ExitCode::InfrastructureFailure;
            }
        };

        if let Err(error) = write_grader_agreement(&self.report_dir, &document) {
            record_error(&span, error.error_type());
            eprintln!("Failed to write grader-agreement.json: {error}");
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            match serde_json::to_string_pretty(&document) {
                Ok(json) => println!("{json}"),
                Err(error) => {
                    eprintln!("Failed to serialize grader agreement report: {error}");
                    return ExitCode::InfrastructureFailure;
                }
            }
        } else {
            print_human_summary(&document);
            print_report_dir(&self.report_dir);
        }

        let Some(threshold) = self.min_agreement else {
            return ExitCode::Success;
        };

        match evaluate_gate(&document, threshold) {
            Ok(()) => ExitCode::Success,
            Err(message) => {
                record_error(&span, GATE_FAILED);
                eprintln!("grader-agreement: {message}");
                ExitCode::GateFailed
            }
        }
    }
}

/// Whether the `llm` bucket's agreement floor clears `threshold`.
///
/// Checks the `llm` bucket specifically, not the overall rate: an LLM judge is the one
/// grader kind whose verdicts are not already deterministic given its inputs, so it is
/// the one worth gating a CI run on. A threshold nobody's labels can answer yet, because
/// no llm-graded assertion has a human verdict, is reported as a gate failure rather than
/// a silent pass, so `--min-agreement` never reads as checked when it checked nothing.
fn evaluate_gate(document: &GraderAgreementDocument, threshold: AgreementThreshold) -> Result<(), String> {
    let Some(interval) = document.by_grader_kind.llm.interval else {
        return Err(format!(
            "no llm-graded assertion has a human label yet, so --min-agreement {threshold} could not be checked"
        ));
    };

    if interval.low() < threshold.value() {
        return Err(format!(
            "llm grader agreement's Wilson lower bound {:.3} is below --min-agreement {threshold}",
            interval.low()
        ));
    }

    Ok(())
}

#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
fn print_human_summary(document: &GraderAgreementDocument) {
    println!("Grader agreement:");
    print_bucket_line("  overall", &document.overall);
    print_bucket_line("  mechanical", &document.by_grader_kind.mechanical);
    print_bucket_line("  declarative", &document.by_grader_kind.declarative);
    print_bucket_line("  script", &document.by_grader_kind.script);
    print_bucket_line("  llm", &document.by_grader_kind.llm);

    println!(
        "  coverage: {}/{} graded assertions labelled ({} unlabelled)",
        document.coverage.labeled_assertions,
        document.coverage.graded_assertions,
        document.coverage.unlabeled.len()
    );

    if !document.disagreements.is_empty() {
        println!("  disagreements:");
        for disagreement in &document.disagreements {
            println!(
                "    {} (run {}): grader said {}, human said {}",
                disagreement.assertion_id, disagreement.run_id, disagreement.grader_passed, disagreement.human_verdict
            );
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
fn print_bucket_line(label: &str, bucket: &AgreementBucket) {
    match bucket.interval {
        Some(interval) => println!(
            "{label}: {}/{} agree, 95% CI [{:.3}, {:.3}]",
            bucket.agreements,
            bucket.agreements + bucket.disagreements,
            interval.low(),
            interval.high()
        ),
        None => println!("{label}: no labelled assertions"),
    }
}

#[cfg(test)]
#[path = "grader_agreement_tests.rs"]
mod tests;
