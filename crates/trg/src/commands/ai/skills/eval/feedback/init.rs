use std::path::PathBuf;

use crate::agentskills::feedback::init_feedback;
use crate::agentskills::report::sync_human_feedback;
use clap::Args;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback init ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval feedback init ./report --reviewer reviewer@example.com
")]
pub struct FeedbackInitArgs {
    #[arg(help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,

    #[arg(
        long,
        value_name = "EMAIL",
        help = "Reviewer identity recorded in feedback.json (defaults to git user.email)"
    )]
    pub reviewer: Option<String>,
}

impl FeedbackInitArgs {
    pub fn handle(self) -> i32 {
        let report = match init_feedback(&self.report_dir, self.reviewer.as_deref()) {
            Ok(report) => report,
            Err(e) => {
                eprintln!("Failed to initialize feedback artifacts: {}", e);
                return 1;
            }
        };

        if let Err(e) = sync_human_feedback(&self.report_dir) {
            eprintln!("Failed to sync feedback summary into report.json: {}", e);
            return 1;
        }

        println!(
            "Initialized feedback for {} run(s) ({} already existed)",
            report.created, report.skipped
        );
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::feedback::list_runs_needing_review;
    use crate::commands::ai::skills::eval::feedback::testutil::sample_report_dir;

    #[test]
    fn init_command_scaffolds_feedback_and_syncs_report() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);

        let status = FeedbackInitArgs {
            report_dir: report_dir.clone(),
            reviewer: Some("reviewer@example.com".to_string()),
        }
        .handle();
        assert_eq!(status, 0);

        assert!(list_runs_needing_review(&report_dir).unwrap().is_empty());

        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
        assert!(report.pointer("/summaries/human_feedback/reviewed_runs").is_some());
        assert_eq!(
            report
                .pointer("/runs/0/artifacts/0/kind")
                .and_then(|value| value.as_str()),
            Some("human_feedback")
        );
    }
}
