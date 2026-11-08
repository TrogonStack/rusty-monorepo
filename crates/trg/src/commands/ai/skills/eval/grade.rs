use std::path::{Path, PathBuf};

use crate::agentskills::grading::{grade_report_bundle, GradeOptions, GradeReport, GraderMode};
use crate::agentskills::judge::JudgeProvider;
use crate::agentskills::judge_votes::JudgeVotes;
use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

use super::{print_json, print_report_dir};

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval grade --report-dir ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval grade --report-dir ./report --grader auto --strict

  $ trg ai skills eval grade --report-dir /absolute/path/to/report --grader script --grader-command ./grade.sh
")]
pub struct GradeArgs {
    #[arg(long, help = "Path to a report bundle directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(long, value_enum, default_value_t = GraderMode::Auto, help = "Grading strategy")]
    pub grader: GraderMode,

    #[arg(
        long,
        value_enum,
        default_value_t = JudgeProvider::default(),
        help = "Judge backend for LLM grading. 'compatible' addresses any OpenAI-compatible endpoint through TRG_JUDGE_BASE_URL and TRG_JUDGE_API_KEY."
    )]
    pub grader_provider: JudgeProvider,

    #[arg(
        long,
        value_name = "MODEL",
        help = "Model identifier for LLM grading. Defaults to TRG_JUDGE_MODEL."
    )]
    pub grader_model: Option<String>,

    #[arg(
        long,
        value_name = "COMMAND",
        help = "External grader script. Reads JSON from stdin: {assertion, workspace, outputs, transcript}. Writes {passed, evidence, rationale?} to stdout."
    )]
    pub grader_command: Option<String>,

    #[arg(
        long,
        value_name = "N",
        default_value_t = JudgeVotes::single(),
        help = "Opinions to take from the LLM judge on each assertion, decided by majority. Must be odd, so the panel cannot tie. Costs one judge request per vote per assertion"
    )]
    pub grader_votes: JudgeVotes,

    #[arg(long, help = "Fail when evidence is missing or assertions require LLM grading")]
    pub strict: bool,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl GradeArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        let options = GradeOptions {
            grader: self.grader,
            grader_provider: self.grader_provider,
            grader_model: self.grader_model,
            grader_command: self.grader_command,
            grader_votes: self.grader_votes,
            strict: self.strict,
        };
        let (code, report) = grade_report_dir_with_report(&self.report_dir, options, self.output_format);
        if self.output_format.is_json() {
            return print_json(&GradeJsonOutput::new(&self.report_dir, code, report.as_ref()), code);
        }
        code
    }
}

/// What `grade` reports, and what `run --grade` reports for the stage it just
/// ran. One shape, because a caller reading the grade out of a chained run and
/// out of a standalone `grade` is reading the same thing.
#[derive(Serialize)]
pub(crate) struct GradeJsonOutput<'a> {
    report_dir: String,
    exit_code: ExitCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    grade: Option<&'a GradeReport>,
}

impl<'a> GradeJsonOutput<'a> {
    pub(crate) fn new(report_dir: &Path, exit_code: ExitCode, grade: Option<&'a GradeReport>) -> Self {
        Self {
            report_dir: report_dir.display().to_string(),
            exit_code,
            grade,
        }
    }
}

/// Grades, and prints the human summary only when that is what was asked for.
///
/// Under `json` it prints nothing at all: the caller owns the one document on
/// stdout, and a chained `run` has a later stage that may own it instead.
#[cfg_attr(
    dylint_lib = "trogon_dylint_lints",
    expect(
        debug_remnants,
        reason = "This function renders the CLI response or an operator action on its designated stream; tracing must not replace that output"
    )
)]
pub(crate) fn grade_report_dir_with_report(
    report_dir: &Path,
    options: GradeOptions,
    format: OutputFormat,
) -> (ExitCode, Option<GradeReport>) {
    match grade_report_bundle(report_dir, options) {
        Ok(report) => {
            if !format.is_json() {
                print_report_dir(report_dir);
                println!("Graded {} run(s)", report.runs_graded);
                println!(
                    "  assertions: {}/{} passed",
                    report.passed,
                    report.assertions_graded - report.unsupported - report.excluded - report.ungraded
                );
                if report.unsupported > 0 {
                    println!(
                        "  unsupported: {} (not gradable on this runner, excluded from the tally above)",
                        report.unsupported
                    );
                }
                if report.excluded > 0 {
                    println!(
                        "  arm scoped: {} (presuppose the skill, so they score in neither arm)",
                        report.excluded
                    );
                }
                if let Some(detail) = report.run_statuses.describe_not_completed() {
                    println!(
                        "  runs not completed: {} ({}), so an empty workspace grades as a failure",
                        report.run_statuses.not_completed(),
                        detail
                    );
                }
                if report.ungraded > 0 {
                    println!(
                        "  ungraded: {} (no grader could attempt these, so the suite measured less than it declared)",
                        report.ungraded
                    );
                    const CAP: usize = 5;
                    for text in report.ungraded_assertions.iter().take(CAP) {
                        println!("    - {text}");
                    }
                    if report.ungraded_assertions.len() > CAP {
                        println!("    ... and {} more", report.ungraded_assertions.len() - CAP);
                    }
                }
                if let Some(warning) = &report.judge_independence {
                    println!("{}", warning.describe());
                }
            }
            let exit_code = ExitCode::from_gate(report.failed == 0 && report.ungraded == 0);
            (exit_code, Some(report))
        }
        Err(e) => {
            eprintln!("Grading failed: {}", e);
            (ExitCode::InfrastructureFailure, None)
        }
    }
}

#[cfg(test)]
#[path = "grade_tests.rs"]
mod tests;
