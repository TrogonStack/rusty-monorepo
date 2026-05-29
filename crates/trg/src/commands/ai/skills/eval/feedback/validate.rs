use std::path::PathBuf;

use crate::agentskills::feedback::validate_feedback;
use crate::agentskills::report::sync_human_feedback;
use clap::Args;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback validate ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval feedback validate ./report
")]
pub struct FeedbackValidateArgs {
    #[arg(help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,
}

impl FeedbackValidateArgs {
    pub fn handle(self) -> i32 {
        let report = match validate_feedback(&self.report_dir) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Failed to validate feedback artifacts: {}", e);
                return 1;
            }
        };

        if !report.errors.is_empty() {
            eprintln!("Feedback validation failed:");
            for error in &report.errors {
                eprintln!("  {error}");
            }
            return 1;
        }

        if let Err(e) = sync_human_feedback(&self.report_dir) {
            eprintln!("Failed to sync feedback summary into report.json: {}", e);
            return 1;
        }

        println!("Validated {} feedback file(s)", report.validated);
        0
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

        let status = FeedbackValidateArgs { report_dir }.handle();
        assert_eq!(status, 0);
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

        let status = FeedbackValidateArgs { report_dir }.handle();
        assert_eq!(status, 1);
    }
}
