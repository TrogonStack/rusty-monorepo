use super::*;
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::fs::testutil::MemFS;
use std::path::Path;

fn sample_skill(fs: &MemFS, evals_json: &str) -> std::path::PathBuf {
    let skill_path = Path::new("demo-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(skill_path.join("evals/evals.json"), evals_json);
    skill_path.to_path_buf()
}

fn report_from_skill(fs: &MemFS, skill_path: &Path, iteration: u32) -> ReportDocument {
    let bundle = build_report_bundle(
        fs,
        skill_path,
        skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            iteration: Some(iteration),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();
    bundle.document
}

#[test]
fn identical_evals_hash_returns_none() {
    let fs = MemFS::new();
    let evals = r#"{
            "skill_name": "demo-skill",
            "evals": [
                { "id": "case-a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }
            ]
        }"#;
    let skill_path = sample_skill(&fs, evals);
    let current = report_from_skill(&fs, &skill_path, 2);
    let previous = report_from_skill(&fs, &skill_path, 1);
    assert!(detect_eval_suite_drift(&current, &previous).is_none());
}

#[test]
fn differing_evals_hash_reports_added_and_removed_ids() {
    let fs = MemFS::new();
    let previous_evals = r#"{
            "skill_name": "demo-skill",
            "evals": [
                { "id": "case-a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] },
                { "id": "case-b", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "b" }] }
            ]
        }"#;
    let current_evals = r#"{
            "skill_name": "demo-skill",
            "evals": [
                { "id": "case-a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] },
                { "id": "case-c", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "c" }] }
            ]
        }"#;

    let previous_path = Path::new("demo-skill-prev");
    let current_path = Path::new("demo-skill-next");
    fs.insert(
        previous_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(previous_path.join("evals/evals.json"), previous_evals);
    fs.insert(
        current_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(current_path.join("evals/evals.json"), current_evals);
    let previous = report_from_skill(&fs, previous_path, 1);
    let current = report_from_skill(&fs, current_path, 2);

    let drift = detect_eval_suite_drift(&current, &previous).expect("drift");
    assert_ne!(drift.current_hash, drift.previous_hash);
    assert_eq!(drift.added_eval_ids, vec!["case-c".to_string()]);
    assert_eq!(drift.removed_eval_ids, vec!["case-b".to_string()]);
}

fn narrowed_report_from_skill(fs: &MemFS, skill_path: &Path, pattern: &str, iteration: u32) -> ReportDocument {
    let bundle = build_report_bundle(
        fs,
        skill_path,
        skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            iteration: Some(iteration),
            cases: crate::agentskills::case_selection::CaseSelection::parse(&[pattern.to_string()], &[], None).unwrap(),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();
    bundle.document
}

/// Editing a case rewrites the manifest, so the next run's hash differs and the IDs
/// get diffed. A run that covered one case out of two did not take the other one out
/// of the suite.
#[test]
fn a_case_a_narrowed_run_skipped_is_not_reported_as_one_the_suite_gained() {
    let fs = MemFS::new();
    let previous_path = Path::new("demo-skill-prev");
    let current_path = Path::new("demo-skill-next");
    for (path, prompt) in [(previous_path, "p"), (current_path, "p2")] {
        fs.insert(path.join("SKILL.md"), "---\nname: demo-skill\ndescription: d\n---\n");
        fs.insert(
                path.join("evals/evals.json"),
                format!(
                    r#"{{
                        "skill_name": "demo-skill",
                        "evals": [
                            {{ "id": "case-a", "prompt": "{prompt}", "expected_output": "o", "graders": [{{ "type": "contains", "text": "a" }}] }},
                            {{ "id": "case-b", "prompt": "{prompt}", "expected_output": "o", "graders": [{{ "type": "contains", "text": "b" }}] }}
                        ]
                    }}"#
                ),
            );
    }
    let previous = narrowed_report_from_skill(&fs, previous_path, "case-a", 1);
    let current = report_from_skill(&fs, current_path, 2);

    let drift = detect_eval_suite_drift(&current, &previous).expect("the manifest changed");
    assert_eq!(drift.added_eval_ids, Vec::<String>::new());
    assert_eq!(drift.removed_eval_ids, Vec::<String>::new());
}

#[test]
fn a_narrowed_report_is_diffed_against_the_skill_by_the_suite_it_selected_from() {
    let fs = MemFS::new();
    let skill_path = sample_skill(
        &fs,
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case-a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] },
                    { "id": "case-b", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "b" }] }
                ]
            }"#,
    );
    let report = narrowed_report_from_skill(&fs, &skill_path, "case-a", 1);

    let temp = tempfile::tempdir().unwrap();
    let skill_dir = temp.path().join("demo-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
            skill_dir.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case-a", "prompt": "edited", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] },
                    { "id": "case-b", "prompt": "edited", "expected_output": "o", "graders": [{ "type": "contains", "text": "b" }] }
                ]
            }"#,
        )
        .unwrap();

    let drift = detect_eval_suite_drift_vs_skill(&report, &skill_dir, &EvalDirName::default())
        .unwrap()
        .expect("the manifest changed");
    assert_eq!(drift.added_eval_ids, Vec::<String>::new());
    assert_eq!(drift.removed_eval_ids, Vec::<String>::new());
}

#[test]
fn a_snapshot_reads_the_declared_suite_a_narrowed_report_recorded() {
    let temp = tempfile::tempdir().unwrap();
    let report = serde_json::json!({
        "report": { "iteration": 2 },
        "suite": {
            "evals_hash": "sha256:abc",
            "case_selection": { "cases": ["case-a"], "covered": 1, "declared": ["case-a", "case-b"] }
        },
        "dimensions": { "eval_cases": [{ "id": "case-a" }] }
    });
    std::fs::write(temp.path().join("report.json"), serde_json::to_string(&report).unwrap()).unwrap();

    let snapshot = load_report_drift_snapshot(temp.path()).unwrap();
    assert_eq!(
        snapshot.declared_eval_case_ids,
        BTreeSet::from(["case-a".to_string(), "case-b".to_string()])
    );
}

#[test]
fn a_snapshot_reads_declared_case_objects_the_same_as_legacy_declared_strings() {
    let temp = tempfile::tempdir().unwrap();
    let report = serde_json::json!({
        "report": { "iteration": 2 },
        "suite": {
            "evals_hash": "sha256:abc",
            "case_selection": {
                "cases": ["case-a"],
                "covered": 1,
                "declared": [
                    { "id": "case-a", "split": "train" },
                    { "id": "case-b", "split": "test" }
                ]
            }
        },
        "dimensions": { "eval_cases": [{ "id": "case-a" }] }
    });
    std::fs::write(temp.path().join("report.json"), serde_json::to_string(&report).unwrap()).unwrap();

    let snapshot = load_report_drift_snapshot(temp.path()).unwrap();
    assert_eq!(
        snapshot.declared_eval_case_ids,
        BTreeSet::from(["case-a".to_string(), "case-b".to_string()]),
        "an object-shaped declared entry must not be dropped and fall back to only the \
             covered dimensions, which would report the held-out case-b as drift"
    );
}

#[test]
fn parse_report_iteration_accepts_object_iteration_key() {
    let value = serde_json::json!({
        "report": { "iteration": { "id": "iter-4", "iteration": 4 } }
    });
    assert_eq!(parse_report_iteration(&value).unwrap(), 4);
}

#[test]
fn load_report_drift_snapshot_rejects_iteration_overflow() {
    let temp = tempfile::tempdir().unwrap();
    let report = serde_json::json!({
        "report": { "iteration": (u32::MAX as u64) + 1 },
        "suite": { "evals_hash": "sha256:abc" },
        "dimensions": { "eval_cases": [] }
    });
    std::fs::write(temp.path().join("report.json"), serde_json::to_string(&report).unwrap()).unwrap();

    let error = load_report_drift_snapshot(temp.path()).unwrap_err();
    assert!(error.to_string().contains("report.iteration"));
}

#[test]
fn write_report_fixtures_round_trip_for_drift_tests() {
    let temp = tempfile::tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(
        &fs,
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case-a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }
                ]
            }"#,
    );
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
    let loaded = load_report_document(&report_dir).unwrap();
    assert_eq!(loaded.suite.evals_hash, bundle.document.suite.evals_hash);
}

#[test]
fn a_genuinely_absent_suite_reports_no_drift() {
    let fs = MemFS::new();
    let skill_path = sample_skill(
        &fs,
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case-a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }
                ]
            }"#,
    );
    let report = report_from_skill(&fs, &skill_path, 1);

    // A different skill path in the same MemFS has nothing under it at all: no
    // manifest, no case directories. That is the "no suite here" case, not a read
    // failure, and it must report no drift.
    let absent_skill_path = Path::new("no-such-skill");
    let drift =
        detect_eval_suite_drift_vs_skill_with_fs(&report, absent_skill_path, &EvalDirName::default(), &fs).unwrap();

    assert!(drift.is_none());
}

#[test]
fn a_present_but_unreadable_suite_is_an_error_not_a_clean_bill_of_health() {
    let fs = MemFS::new();
    let skill_path = sample_skill(
        &fs,
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "case-a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }
                ]
            }"#,
    );
    let report = report_from_skill(&fs, &skill_path, 1);

    let unreadable_fs = UnreadableEvalsDirFS {
        evals_dir: skill_path.join("evals"),
    };
    let error = detect_eval_suite_drift_vs_skill_with_fs(&report, &skill_path, &EvalDirName::default(), &unreadable_fs)
        .unwrap_err();

    assert!(matches!(error, EvalError::Io(io_error) if io_error.kind() == std::io::ErrorKind::PermissionDenied));
}

/// A filesystem whose `evals` directory exists but cannot be listed, so
/// `resolve_eval_suite` fails with an IO error that is not `NotFound`.
struct UnreadableEvalsDirFS {
    evals_dir: std::path::PathBuf,
}

impl FileSystem for UnreadableEvalsDirFS {
    fn read_to_string(&self, _path: &Path) -> std::io::Result<String> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
    }

    fn read_bytes(&self, _path: &Path) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not found"))
    }

    fn write(&self, _path: &Path, _contents: &str) -> std::io::Result<()> {
        Ok(())
    }

    fn exists(&self, path: &Path) -> bool {
        path == self.evals_dir
    }

    fn is_file(&self, _path: &Path) -> bool {
        false
    }

    fn is_dir(&self, path: &Path) -> bool {
        path == self.evals_dir
    }

    fn is_symlink(&self, _path: &Path) -> bool {
        false
    }

    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
        if path == self.evals_dir {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            ))
        } else {
            Ok(Vec::new())
        }
    }
}
