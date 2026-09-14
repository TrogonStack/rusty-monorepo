use std::path::PathBuf;

use crate::agentskills::exit_code::ExitCode;
use crate::agentskills::feedback::validate_feedback;
use crate::agentskills::report::sync_human_feedback;
use crate::commands::ai::skills::eval::print_json;
use crate::output::OutputFormat;
use clap::Args;
use serde_json::json;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback validate ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval feedback validate ./report
")]
pub struct FeedbackValidateArgs {
    #[arg(help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_enum,
        default_value_t = OutputFormat::Text,
        help = "Render the result as a human summary or as a machine-readable document"
    )]
    pub output_format: OutputFormat,
}

impl FeedbackValidateArgs {
    pub fn handle(self) -> ExitCode {
        let report = match validate_feedback(&self.report_dir) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Failed to validate feedback artifacts: {}", e);
                return ExitCode::InfrastructureFailure;
            }
        };

        let failed = !report.errors.is_empty();

        if !failed {
            if let Err(e) = sync_human_feedback(&self.report_dir) {
                eprintln!("Failed to sync feedback summary into report.json: {}", e);
                return ExitCode::InfrastructureFailure;
            }
        }

        // A feedback file that does not validate is the verdict, not a
        // breakdown, so under `json` it rides in the document rather than on
        // stderr where a parser will not see it.
        if self.output_format.is_json() {
            let document = json!({
                "report_dir": self.report_dir.display().to_string(),
                "validated": report.validated,
                "errors": report.errors,
            });
            return print_json(&document, ExitCode::from_gate(!failed));
        }

        if failed {
            eprintln!("Feedback validation failed:");
            for error in &report.errors {
                eprintln!("  {error}");
            }
            return ExitCode::GateFailed;
        }

        println!("Validated {} feedback file(s)", report.validated);
        ExitCode::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::feedback::init_feedback;
    use crate::commands::ai::skills::eval::feedback::testutil::sample_report_dir;

    #[test]
    fn validate_command_accepts_valid_feedback() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        init_feedback(&report_dir, Some("reviewer@example.com")).unwrap();

        let status = FeedbackValidateArgs {
            report_dir,
            output_format: OutputFormat::Text,
        }
        .handle();
        assert_eq!(status, ExitCode::Success);
    }

    #[test]
    fn validate_command_rejects_invalid_feedback() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);
        init_feedback(&report_dir, Some("reviewer@example.com")).unwrap();

        std::fs::write(
            report_dir.join("runs/run-001/feedback.json"),
            r#"{
                "reviewer": "reviewer@example.com",
                "reviewed_at": "not-a-timestamp",
                "notes": []
            }"#,
        )
        .unwrap();

        let status = FeedbackValidateArgs {
            report_dir,
            output_format: OutputFormat::Text,
        }
        .handle();
        assert_eq!(status, ExitCode::GateFailed);
    }
}
