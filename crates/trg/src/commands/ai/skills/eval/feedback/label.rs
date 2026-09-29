use std::path::PathBuf;

use crate::agentskills::exit_code::ExitCode;
use crate::agentskills::feedback::{record_assertion_verdict, FeedbackLabelReport, HumanVerdict};
use crate::agentskills::report::sync_human_feedback;
use crate::commands::ai::skills::eval::print_json;
use crate::output::OutputFormat;
use clap::Args;
use serde::Serialize;

/// The same shape `FeedbackLabelReport` reports, plus the report directory the sibling
/// `feedback` subcommands already echo back in their JSON output.
#[derive(Debug, Serialize)]
struct FeedbackLabelOutput<'a> {
    report_dir: String,
    #[serde(flatten)]
    report: &'a FeedbackLabelReport,
}

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback label ./report --run run-001 --assertion case-a:g0 --verdict pass

  $ trg ai skills eval feedback label ./report --run run-001 --assertion case-a:g0 \\
      --verdict fail --rationale 'evidence quoted the wrong file'
")]
pub struct FeedbackLabelArgs {
    #[arg(help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "RUN_ID",
        help = "The run whose grading.json the assertion was scored in"
    )]
    pub run: String,

    #[arg(
        long,
        value_name = "ASSERTION_ID",
        help = "The stable assertion id (<eval-case-id>:g<index>) to label"
    )]
    pub assertion: String,

    #[arg(long, value_enum, help = "The human reviewer's verdict on this assertion")]
    pub verdict: HumanVerdict,

    #[arg(long, help = "Why the reviewer reached this verdict, recorded alongside it")]
    pub rationale: Option<String>,

    #[arg(
        long,
        value_name = "EMAIL",
        help = "Reviewer identity recorded in feedback.json when it does not exist yet (defaults to git user.email)"
    )]
    pub reviewer: Option<String>,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl FeedbackLabelArgs {
    pub fn handle(self) -> ExitCode {
        let report = match record_assertion_verdict(
            &self.report_dir,
            &self.run,
            &self.assertion,
            self.verdict,
            self.rationale,
            self.reviewer.as_deref(),
        ) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Failed to record assertion verdict: {}", e);
                return ExitCode::InfrastructureFailure;
            }
        };

        if let Err(e) = sync_human_feedback(&self.report_dir) {
            eprintln!("Failed to sync feedback summary into report.json: {}", e);
            return ExitCode::InfrastructureFailure;
        }

        if self.output_format.is_json() {
            let document = FeedbackLabelOutput {
                report_dir: self.report_dir.display().to_string(),
                report: &report,
            };
            return print_json(&document, ExitCode::Success);
        }

        println!(
            "Recorded verdict '{}' for {} in run {}{}",
            report.verdict,
            report.assertion_id,
            report.run_id,
            if report.replaced { " (replaced)" } else { "" }
        );
        ExitCode::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::feedback::FeedbackDocument;
    use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
    use crate::commands::ai::skills::eval::feedback::testutil::sample_report_dir;

    fn read_feedback(report_dir: &std::path::Path, run_id: &str) -> FeedbackDocument {
        let content = std::fs::read_to_string(report_dir.join(format!("runs/{run_id}/feedback.json"))).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    fn grade_run(report_dir: &std::path::Path, run_id: &str, assertion: &str) {
        let result = AssertionGradeResult {
            assertion: assertion.to_string(),
            passed: true,
            evidence: "matched".to_string(),
            grader: GraderInfo {
                kind: GraderKind::Mechanical,
                model: None,
                command: None,
            },
            name: None,
            rationale: None,
            unsupported: None,
            excluded: None,
            ungraded: None,
            votes: None,
            weight: None,
        };
        let grading = build_grading_file(vec![result]).unwrap();
        std::fs::write(
            report_dir.join(format!("runs/{run_id}/grading.json")),
            serde_json::to_string_pretty(&grading).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn label_command_records_a_verdict() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_run(&report_dir, "run-001", "assert a");

        let status = FeedbackLabelArgs {
            report_dir: report_dir.clone(),
            run: "run-001".to_string(),
            assertion: "case-a:g0".to_string(),
            verdict: HumanVerdict::Pass,
            rationale: Some("matches expected output".to_string()),
            reviewer: Some("reviewer@example.com".to_string()),
            output_format: OutputFormat::Text,
        }
        .handle();
        assert_eq!(status, ExitCode::Success);

        let document = read_feedback(&report_dir, "run-001");
        let verdict = &document.assertion_verdicts["case-a:g0"];
        assert_eq!(verdict.verdict, HumanVerdict::Pass);
    }

    #[test]
    fn label_command_rejects_an_unknown_assertion() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        grade_run(&report_dir, "run-001", "assert a");

        let status = FeedbackLabelArgs {
            report_dir,
            run: "run-001".to_string(),
            assertion: "case-a:g9".to_string(),
            verdict: HumanVerdict::Pass,
            rationale: None,
            reviewer: Some("reviewer@example.com".to_string()),
            output_format: OutputFormat::Text,
        }
        .handle();
        assert_eq!(status, ExitCode::InfrastructureFailure);
    }
}
