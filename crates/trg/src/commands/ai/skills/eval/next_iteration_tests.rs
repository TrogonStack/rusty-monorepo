use super::*;
use crate::agentskills::improvement_bundle::testutil::sample_prior_iteration_fixture;
use crate::agentskills::improvement_bundle::{BUNDLE_JSON_NAME, BUNDLE_MD_NAME, NEXT_ITERATION_DIR};

#[test]
fn next_iteration_command_writes_bundle_from_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("current-skill");
    let report_dir = sample_prior_iteration_fixture(&temp, &skill_root);

    let status = NextIterationArgs {
        report_dir: report_dir.clone(),
        skill_dir: Some(skill_root),
        eval_dir: None,
        allow_eval_suite_drift: false,
        output_format: OutputFormat::Text,
    }
    .handle(&crate::fs::testutil::MemFS::new());
    assert_eq!(status, ExitCode::Success);

    let bundle_dir = report_dir.parent().unwrap().join(NEXT_ITERATION_DIR);
    assert!(bundle_dir.join(BUNDLE_MD_NAME).is_file());
    assert!(bundle_dir.join(BUNDLE_JSON_NAME).is_file());
}
