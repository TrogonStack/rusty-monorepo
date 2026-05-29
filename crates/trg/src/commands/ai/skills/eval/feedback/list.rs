use std::path::PathBuf;

use crate::agentskills::feedback::list_runs_needing_review;
use clap::Args;

#[derive(Args)]
#[command(after_help = "\
Examples:

  $ trg ai skills eval feedback list ./artifacts/my-skill/20260526T120000Z-abc

  $ trg ai skills eval feedback list /absolute/path/to/report
")]
pub struct FeedbackListArgs {
    #[arg(help = "Path to a generated eval report directory containing report.json")]
    pub report_dir: PathBuf,
}

impl FeedbackListArgs {
    pub fn handle(self) -> i32 {
        let pending = match list_runs_needing_review(&self.report_dir) {
            Ok(pending) => pending,
            Err(e) => {
                eprintln!("Failed to list runs needing review: {}", e);
                return 1;
            }
        };

        if pending.is_empty() {
            println!("All runs have feedback.json");
        } else {
            println!("Runs needing review:");
            for run_id in pending {
                println!("  {run_id}");
            }
        }

        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agentskills::feedback::init_feedback;
    use crate::commands::ai::skills::eval::feedback::testutil::sample_report_dir;

    #[test]
    fn list_command_reports_pending_runs() {
        let temp = tempfile::tempdir().unwrap();
        let report_dir = sample_report_dir(&temp);

        let status = FeedbackListArgs {
            report_dir: report_dir.clone(),
        }
        .handle();
        assert_eq!(status, 0);

        init_feedback(&report_dir, Some("reviewer@example.com")).unwrap();
        let status = FeedbackListArgs { report_dir }.handle();
        assert_eq!(status, 0);
    }
}
