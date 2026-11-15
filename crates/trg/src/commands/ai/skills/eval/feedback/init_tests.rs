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
        output_format: OutputFormat::Text,
    }
    .handle();
    assert_eq!(status, ExitCode::Success);

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
