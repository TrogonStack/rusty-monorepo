use super::*;
use crate::agentskills::feedback::init_feedback;
use crate::commands::ai::skills::eval::feedback::testutil::sample_report_dir;

#[test]
fn list_command_reports_pending_runs() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp);

    let status = FeedbackListArgs {
        report_dir: report_dir.clone(),
        output_format: OutputFormat::Text,
    }
    .handle();
    assert_eq!(status, ExitCode::Success);

    init_feedback(&report_dir, Some("reviewer@example.com")).unwrap();
    let status = FeedbackListArgs {
        report_dir,
        output_format: OutputFormat::Text,
    }
    .handle();
    assert_eq!(status, ExitCode::Success);
}
