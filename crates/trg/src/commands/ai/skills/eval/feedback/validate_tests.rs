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
