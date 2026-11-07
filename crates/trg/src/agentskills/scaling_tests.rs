use super::*;
use crate::agentskills::grading::{build_grading_file, AssertionGradeResult, GraderInfo, GraderKind};
use crate::agentskills::report::{build_report_bundle, write_report_bundle, BuildReportOptions, WriteReportOptions};
use crate::fs::testutil::MemFS;

fn sample_report_dir(temp: &tempfile::TempDir, report_id: &str, model_config: &str) -> PathBuf {
    let fs = MemFS::new();
    let skill_path = Path::new("demo-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case-a", "prompt": "prompt a long enough", "expected_output": "output a", "graders": [{ "type": "contains", "text": "a" }] },
                    { "id": "case-b", "prompt": "prompt b long enough", "expected_output": "output b", "graders": [{ "type": "contains", "text": "b" }] }
                ]
            }"#,
        );

    let mut bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("demo-skill"),
        "demo-skill",
        model_config,
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some(report_id.to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    // A run only lands in the completed bucket `aggregate_scenarios_for_records` reads
    // assertions from once its status says so; the scaffold otherwise leaves every run
    // "skipped" until a harness actually runs it.
    for run in bundle.document.runs.iter_mut() {
        run.status = "completed".to_string();
    }

    write_report_bundle(
        temp.path(),
        &bundle,
        WriteReportOptions {
            force: true,
            ..WriteReportOptions::default()
        },
    )
    .unwrap()
}

fn grade_run(report_dir: &Path, run_id: &str, passed: bool) {
    let result = AssertionGradeResult {
        assertion: "assertion text".to_string(),
        passed,
        evidence: "the transcript said so".to_string(),
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
fn fewer_than_two_bundles_is_refused_rather_than_compared_against_itself() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = sample_report_dir(&temp, "only-one", "solo");

    let result = build_scaling_document(&[report_dir], FailedRunsMode::Bucket);
    assert!(matches!(result, Err(ScalingError::NotEnoughBundles(1))));
}

#[test]
fn bundles_built_from_different_suites_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = sample_report_dir(&temp, "weaker", "small");

    let fs = MemFS::new();
    let skill_path = Path::new("other-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: other-skill\ndescription: d\n---\n",
    );
    fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "other-skill",
                "evals": [{ "id": "case-c", "prompt": "a different prompt", "expected_output": "o", "graders": [{ "type": "contains", "text": "c" }] }]
            }"#,
        );
    let other_bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("other-skill"),
        "other-skill",
        "large",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("stronger".to_string()),
            generated_at: Some("2026-05-26T12:00:00Z".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();
    let stronger_dir = write_report_bundle(
        temp.path(),
        &other_bundle,
        WriteReportOptions {
            force: true,
            ..WriteReportOptions::default()
        },
    )
    .unwrap();

    let result = build_scaling_document(&[weaker_dir, stronger_dir], FailedRunsMode::Bucket);
    assert!(matches!(result, Err(ScalingError::SuiteHashMismatch { .. })));
}

/// A bundle that fails to parse as the current report schema (here, a `model_configs`
/// entry with a `capture_status` value no variant, not even a retired one accepted for
/// compatibility, recognizes) reports it like any other malformed bundle, but with
/// several bundles on the command line the raw `serde_json::Error` alone does not say
/// which one is at fault. The message has to name the report directory, the same way
/// `SuiteHashMismatch` already names both bundles it compares.
#[test]
fn an_unreadable_bundle_names_its_own_report_dir_in_the_error() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = sample_report_dir(&temp, "weaker", "small");
    let stale_dir = sample_report_dir(&temp, "stronger", "large");

    let report_path = stale_dir.join("report.json");
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    document["dimensions"]["model_configs"][0]["capture_status"] = serde_json::json!("unknown_status");
    std::fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let result = build_scaling_document(&[weaker_dir, stale_dir.clone()], FailedRunsMode::Bucket);

    let error = result.expect_err("a report.json with an unrecognized capture_status must not parse");
    let message = error.to_string();
    assert!(
        message.contains(&stale_dir.display().to_string()),
        "expected the error to name '{}', got: {message}",
        stale_dir.display()
    );
}

/// `capture_status: "partial"` is a retired spelling of `incomplete` that older builds
/// wrote; a bundle carrying it has to compare cleanly rather than being reported as
/// unreadable.
#[test]
fn a_bundle_with_the_retired_partial_capture_status_still_compares() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = sample_report_dir(&temp, "weaker", "small");
    let stronger_dir = sample_report_dir(&temp, "stronger", "large");

    let report_path = stronger_dir.join("report.json");
    let mut document: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    document["dimensions"]["model_configs"][0]["capture_status"] = serde_json::json!("partial");
    std::fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let result = build_scaling_document(&[weaker_dir, stronger_dir], FailedRunsMode::Bucket);

    assert!(
        result.is_ok(),
        "a report.json using the retired 'partial' capture_status must still parse: {:?}",
        result.err()
    );
}

#[test]
fn a_stronger_config_that_clears_the_noise_reads_as_improved_and_names_its_model_config() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
    for (run_id, passed) in [("run-001", false), ("run-002", false)] {
        grade_run(&weaker_dir, run_id, passed);
    }
    let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
    for (run_id, passed) in [("run-001", true), ("run-002", true)] {
        grade_run(&stronger_dir, run_id, passed);
    }

    let document = build_scaling_document(&[weaker_dir.clone(), stronger_dir.clone()], FailedRunsMode::Bucket).unwrap();

    assert_eq!(document.bundles.len(), 2);
    assert_eq!(
        document.bundles[0]
            .model_config
            .as_ref()
            .map(|config| config.id.as_str()),
        Some("small-model")
    );
    assert_eq!(
        document.bundles[1]
            .model_config
            .as_ref()
            .map(|config| config.id.as_str()),
        Some("large-model")
    );
    assert_eq!(document.steps.len(), 1);
    assert!(matches!(
        document.steps[0].comparison.overall,
        ScalingVerdict::Improved(_)
    ));
    assert!(matches!(
        document.end_to_end.comparison.overall,
        ScalingVerdict::Improved(_)
    ));
    assert!(!any_regression(&document));
}

#[test]
fn a_case_the_stronger_config_scored_lower_on_is_surfaced_as_an_outlier() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
    // case-a with_skill, then case-b with_skill (build_runs orders eval case then scenario).
    grade_run(&weaker_dir, "run-001", true);
    grade_run(&weaker_dir, "run-002", true);
    let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
    grade_run(&stronger_dir, "run-001", false);
    grade_run(&stronger_dir, "run-002", true);

    let document = build_scaling_document(&[weaker_dir, stronger_dir], FailedRunsMode::Bucket).unwrap();

    assert_eq!(document.steps[0].outliers.len(), 1);
    assert_eq!(document.steps[0].outliers[0].eval_case_id, "case-a");
}

#[test]
fn a_regression_anywhere_is_reported_by_any_regression() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
    grade_run(&weaker_dir, "run-001", true);
    grade_run(&weaker_dir, "run-002", true);
    let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
    grade_run(&stronger_dir, "run-001", false);
    grade_run(&stronger_dir, "run-002", false);

    let document = build_scaling_document(&[weaker_dir, stronger_dir], FailedRunsMode::Bucket).unwrap();

    assert!(any_regression(&document));
}

#[test]
fn writing_puts_scalingjson_in_the_named_report_dir() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = sample_report_dir(&temp, "weaker", "small-model");
    grade_run(&weaker_dir, "run-001", true);
    grade_run(&weaker_dir, "run-002", true);
    let stronger_dir = sample_report_dir(&temp, "stronger", "large-model");
    grade_run(&stronger_dir, "run-001", true);
    grade_run(&stronger_dir, "run-002", true);

    let document = build_scaling_document(&[weaker_dir, stronger_dir.clone()], FailedRunsMode::Bucket).unwrap();
    let written = write_scaling(&stronger_dir, &document).unwrap();

    assert_eq!(written, stronger_dir.join(SCALING_FILE_NAME));
    assert!(written.is_file());
}
