use super::*;
use crate::agentskills::evals::{EvalCase, EvalSuite};
use crate::agentskills::report::{BuildReportOptions, ScenarioKind};
use crate::agentskills::sampling::AttemptCount;
use crate::fs::testutil::MemFS;
use crate::fs::FileSystem;
use std::path::Path;

fn sample_evals(ids: &[&str]) -> Vec<EvalCase> {
    ids.iter()
        .map(|id| {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "prompt": "prompt",
                "expected_output": "output",
            }))
            .unwrap()
        })
        .collect()
}

#[test]
fn eval_slug_normalizes_path_separators_and_whitespace() {
    assert_eq!(eval_slug("foo/bar"), "foo-bar");
    assert_eq!(eval_slug("foo//bar"), "foo-bar");
    assert_eq!(eval_slug("My Eval Case"), "my-eval-case");
    assert_eq!(eval_slug("  spaced  "), "spaced");
    assert_eq!(eval_slug("@#$start"), "start");
}

#[test]
fn eval_slug_preserves_unicode_letters() {
    assert_eq!(eval_slug("Café Résumé"), "café-résumé");
    assert_eq!(eval_slug("日本語"), "日本語");
}

#[test]
fn eval_slug_disambiguates_collisions() {
    let evals = sample_evals(&["a/b", "a b", "a-b"]);
    let slugs = assign_eval_slugs(&evals);
    assert_eq!(slugs.get("a/b").map(String::as_str), Some("a-b"));
    assert_eq!(slugs.get("a b").map(String::as_str), Some("a-b-2"));
    assert_eq!(slugs.get("a-b").map(String::as_str), Some("a-b-3"));
}

#[test]
fn detect_next_iteration_scans_report_bundles() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("demo-skill");
    std::fs::create_dir_all(skill_root.join("report-a").join("iteration-1")).unwrap();
    std::fs::create_dir_all(skill_root.join("report-b").join("iteration-3")).unwrap();

    assert_eq!(detect_next_iteration(temp.path(), "demo-skill"), 4);
}

#[test]
fn detect_next_iteration_defaults_to_one() {
    let temp = tempfile::tempdir().unwrap();
    assert_eq!(detect_next_iteration(temp.path(), "missing-skill"), 1);
}

#[test]
fn ensure_iteration_available_rejects_existing_iteration() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("demo-skill");
    let report_dir = skill_root.join("report-a");
    std::fs::create_dir_all(report_dir.join("iteration-2")).unwrap();

    let err = ensure_iteration_available(temp.path(), "demo-skill", 2, false, None).unwrap_err();
    assert!(err.to_string().contains("iteration 2 already exists"));
}

#[test]
fn ensure_iteration_available_allows_force() {
    let temp = tempfile::tempdir().unwrap();
    let skill_root = temp.path().join("demo-skill");
    std::fs::create_dir_all(skill_root.join("report-a").join("iteration-2")).unwrap();

    ensure_iteration_available(temp.path(), "demo-skill", 2, true, None).unwrap();
}

#[test]
fn alias_index_round_trips_and_references_existing_runs() {
    let temp = tempfile::tempdir().unwrap();
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
                    { "id": "case/a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }
                ]
            }"#,
        );

    let bundle = crate::agentskills::report::build_report_bundle(
        &fs,
        skill_path,
        skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions {
            report_id: Some("alias-report".to_string()),
            iteration: Some(1),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let slugs = slugs_for_suite(
        &serde_json::from_str::<EvalSuite>(&fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap()).unwrap(),
    );

    let report_dir = temp.path().join("demo-skill/alias-report");
    std::fs::create_dir_all(&report_dir).unwrap();
    for workspace in &bundle.workspace_dirs {
        std::fs::create_dir_all(report_dir.join(workspace)).unwrap();
    }

    write_docs_mirror_layout(&report_dir, &bundle, 1, &slugs).unwrap();

    let index_path = report_dir.join("iteration-1/alias-index.json");
    assert!(index_path.is_file());
    let raw = std::fs::read_to_string(&index_path).unwrap();
    let index: AliasIndex = serde_json::from_str(&raw).unwrap();
    let round_trip: AliasIndex = serde_json::from_str(&serde_json::to_string(&index).unwrap()).unwrap();
    assert_eq!(index, round_trip);

    let entry = index.evals.get("eval-case-a").expect("eval-case-a entry");
    match entry.scenarios.get("with_skill").unwrap() {
        ScenarioAlias::Single(path) => {
            assert_eq!(path, "runs/run-001");
            assert!(report_dir.join(path).join("workspace").is_dir());
        }
        ScenarioAlias::Multi(_) => panic!("expected single attempt mapping"),
    }
    match entry.scenarios.get("without_skill").unwrap() {
        ScenarioAlias::Single(path) => {
            assert_eq!(path, "runs/run-002");
            assert!(report_dir.join(path).join("workspace").is_dir());
        }
        ScenarioAlias::Multi(_) => panic!("expected single attempt mapping"),
    }
}

/// Taken as the highest attempt number anywhere in the pass, a case drawn once would be
/// filed under `attempt-1` because some other case was drawn five times, and a reader
/// following the unsuffixed path it was promised finds nothing.
#[test]
fn a_cell_drawn_once_is_not_suffixed_because_another_cell_was_drawn_more() {
    let run = |case: &str, attempt: u32| -> RunRecord {
        serde_json::from_value(serde_json::json!({
            "id": format!("run-{case}-{attempt}"),
            "eval_case_id": case,
            "eval_slug": case,
            "scenario_id": "with_skill",
            "iteration": 1,
            "model_config_id": "ci-default",
            "skill_revision_id": "current",
            "attempt": attempt,
            "status": "completed",
            "paths": { "workspace": "w", "outputs": "o" },
            "mirror_path": "",
            "artifacts": [],
            "metrics": {}
        }))
        .unwrap()
    };
    let draws = DrawsPerCell::of(&[run("steady", 1), run("flaky", 1), run("flaky", 2), run("flaky", 3)]);

    assert_eq!(draws.of_run(&run("steady", 1)), 1);
    assert_eq!(draws.of_run(&run("flaky", 1)), 3);
}

#[test]
fn mirror_layout_disambiguates_multiple_attempts() {
    let temp = tempfile::tempdir().unwrap();
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
                    { "id": "one", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }
                ]
            }"#,
        );

    let bundle = crate::agentskills::report::build_report_bundle(
        &fs,
        skill_path,
        skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("attempts-report".to_string()),
            iteration: Some(1),
            attempts: Some(AttemptCount::parse(2).unwrap()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let slugs = slugs_for_suite(
        &serde_json::from_str::<EvalSuite>(&fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap()).unwrap(),
    );

    let report_dir = temp.path().join("demo-skill/attempts-report");
    std::fs::create_dir_all(&report_dir).unwrap();
    for workspace in &bundle.workspace_dirs {
        std::fs::create_dir_all(report_dir.join(workspace)).unwrap();
    }

    write_docs_mirror_layout(&report_dir, &bundle, 1, &slugs).unwrap();

    let attempt_one = report_dir.join("iteration-1/eval-one/with_skill/attempt-1");
    let attempt_two = report_dir.join("iteration-1/eval-one/with_skill/attempt-2");
    assert!(attempt_one.symlink_metadata().unwrap().file_type().is_symlink());
    assert!(attempt_two.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(&attempt_one).unwrap(),
        PathBuf::from("../../../runs/run-001/workspace")
    );
    assert_eq!(
        std::fs::read_link(&attempt_two).unwrap(),
        PathBuf::from("../../../runs/run-002/workspace")
    );

    let index: AliasIndex =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("iteration-1/alias-index.json")).unwrap())
            .unwrap();
    match index
        .evals
        .get("eval-one")
        .unwrap()
        .scenarios
        .get("with_skill")
        .unwrap()
    {
        ScenarioAlias::Multi(map) => {
            assert_eq!(map.get("attempt-1").map(String::as_str), Some("runs/run-001"));
            assert_eq!(map.get("attempt-2").map(String::as_str), Some("runs/run-002"));
        }
        ScenarioAlias::Single(_) => panic!("expected multi attempt mapping"),
    }
}

#[test]
fn mirror_layout_snapshot() {
    let temp = tempfile::tempdir().unwrap();
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
                    { "id": "case/a", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] },
                    { "id": "2", "prompt": "p2", "expected_output": "o2", "graders": [{ "type": "contains", "text": "b" }] }
                ]
            }"#,
        );

    let bundle = crate::agentskills::report::build_report_bundle(
        &fs,
        skill_path,
        skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions {
            report_id: Some("snapshot-report".to_string()),
            generated_at: Some("2026-05-26T00:00:00Z".to_string()),
            iteration: Some(1),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let slugs = slugs_for_suite(
        &serde_json::from_str::<EvalSuite>(&fs.read_to_string(&skill_path.join("evals/evals.json")).unwrap()).unwrap(),
    );

    let report_dir = temp.path().join("demo-skill/snapshot-report");
    std::fs::create_dir_all(&report_dir).unwrap();
    for workspace in &bundle.workspace_dirs {
        std::fs::create_dir_all(report_dir.join(workspace)).unwrap();
    }

    write_docs_mirror_layout(&report_dir, &bundle, 1, &slugs).unwrap();

    assert!(report_dir.join("iteration-1/benchmark.json").is_file());

    let with_skill = report_dir.join("iteration-1/eval-case-a/with_skill");
    let without_skill = report_dir.join("iteration-1/eval-case-a/without_skill");
    let eval_two_with = report_dir.join("iteration-1/eval-2/with_skill");
    let eval_two_without = report_dir.join("iteration-1/eval-2/without_skill");

    for link in [with_skill, without_skill, eval_two_with, eval_two_without] {
        assert!(
            link.symlink_metadata().unwrap().file_type().is_symlink(),
            "expected symlink at {}",
            link.display()
        );
    }

    assert_eq!(
        std::fs::read_link(report_dir.join("iteration-1/eval-case-a/with_skill")).unwrap(),
        PathBuf::from("../../runs/run-001/workspace")
    );
    assert_eq!(
        std::fs::read_link(report_dir.join("iteration-1/eval-case-a/without_skill")).unwrap(),
        PathBuf::from("../../runs/run-002/workspace")
    );
    assert_eq!(
        std::fs::read_link(report_dir.join("iteration-1/eval-2/with_skill")).unwrap(),
        PathBuf::from("../../runs/run-003/workspace")
    );
    assert_eq!(
        std::fs::read_link(report_dir.join("iteration-1/eval-2/without_skill")).unwrap(),
        PathBuf::from("../../runs/run-004/workspace")
    );
}
