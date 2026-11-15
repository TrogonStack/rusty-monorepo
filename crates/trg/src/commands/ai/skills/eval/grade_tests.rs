use super::*;
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::fs::testutil::MemFS;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::tempdir;

fn write_fixture_skill(root: &Path) -> std::path::PathBuf {
    let skill_dir = root.join("fixture-skill");
    fs::create_dir_all(skill_dir.join("evals")).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: fixture-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "graders": [{ "type": "file_exists", "path": "out.json" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

#[test]
fn grade_command_writes_grading_json_for_report_bundle() {
    let temp = tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path());

    let fs = MemFS::new();
    fs.insert(
        skill_dir.join("SKILL.md"),
        "---\nname: fixture-skill\ndescription: fixture\n---\n",
    );
    fs.insert(
        skill_dir.join("evals/evals.json"),
        fs::read_to_string(skill_dir.join("evals/evals.json")).unwrap(),
    );

    let bundle = build_report_bundle(
        &fs,
        &skill_dir,
        &skill_dir,
        "fixture-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("test-report".to_string()),
            generated_at: Some("2026-05-26T00:00:00Z".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
    let run_dir = report_dir.join("runs/run-001");
    fs::create_dir_all(run_dir.join("outputs")).unwrap();
    fs::write(run_dir.join("outputs/out.json"), r#"{"ok": true}"#).unwrap();

    let status = GradeArgs {
        report_dir: report_dir.clone(),
        grader: GraderMode::Auto,
        grader_provider: JudgeProvider::default(),
        grader_model: None,
        grader_command: None,
        grader_votes: JudgeVotes::single(),
        strict: false,
        output_format: OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::Success);
    let grading_path = run_dir.join("grading.json");
    assert!(grading_path.is_file());
    let grading: serde_json::Value = serde_json::from_str(&fs::read_to_string(&grading_path).unwrap()).unwrap();
    assert_eq!(
        grading.pointer("/assertion_results/0/passed").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn grade_refuses_to_exit_zero_when_an_assertion_went_ungraded_even_without_strict() {
    let temp = tempdir().unwrap();
    let skill_dir = temp.path().join("fixture-skill");
    fs::create_dir_all(skill_dir.join("evals")).unwrap();
    let skill_md = "---\nname: fixture-skill\ndescription: fixture\n---\n";
    fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
    let suite = r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "graders": [{ "type": "llm", "criterion": "the summary reads as appropriately cautious" }]
                    }
                ]
            }"#;
    fs::write(skill_dir.join("evals/evals.json"), suite).unwrap();

    let mem = MemFS::new();
    mem.insert(skill_dir.join("SKILL.md"), skill_md);
    mem.insert(skill_dir.join("evals/evals.json"), suite);

    let bundle = build_report_bundle(
        &mem,
        &skill_dir,
        &skill_dir,
        "fixture-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("ungraded-report".to_string()),
            generated_at: Some("2026-05-26T00:00:00Z".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
    let run_dir = report_dir.join("runs/run-001");
    fs::create_dir_all(run_dir.join("outputs")).unwrap();

    let status = GradeArgs {
        report_dir: report_dir.clone(),
        grader: GraderMode::None,
        grader_provider: JudgeProvider::default(),
        grader_model: None,
        grader_command: None,
        grader_votes: JudgeVotes::single(),
        strict: false,
        output_format: OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(
        status,
        ExitCode::GateFailed,
        "a suite that measured less than it declared must not report success just because strict is off"
    );

    let grading: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("grading.json")).unwrap()).unwrap();
    assert_eq!(grading.pointer("/summary/ungraded").and_then(|v| v.as_u64()), Some(1));
    assert_eq!(grading.pointer("/summary/failed").and_then(|v| v.as_u64()), Some(0));
}

#[test]
fn grade_command_integration_with_script_grader() {
    let temp = tempdir().unwrap();
    let skill_dir = temp.path().join("fixture-skill");
    fs::create_dir_all(skill_dir.join("evals")).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: fixture-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    let suite = r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "graders": [{ "type": "llm", "criterion": "custom check passes" }]
                    }
                ]
            }"#;
    fs::write(skill_dir.join("evals/evals.json"), suite).unwrap();

    let script = temp.path().join("grader.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
read _input
echo '{"passed": true, "evidence": "script confirmed custom check", "rationale": "integration"}'
"#,
    )
    .unwrap();

    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

    let fs = MemFS::new();
    fs.insert(
        skill_dir.join("SKILL.md"),
        "---\nname: fixture-skill\ndescription: fixture\n---\n",
    );
    fs.insert(skill_dir.join("evals/evals.json"), suite);

    let bundle = build_report_bundle(
        &fs,
        &skill_dir,
        &skill_dir,
        "fixture-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("script-report".to_string()),
            generated_at: Some("2026-05-26T00:00:00Z".to_string()),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
    fs::create_dir_all(report_dir.join("runs/run-001/workspace")).unwrap();

    let status = GradeArgs {
        report_dir,
        grader: GraderMode::Script,
        grader_provider: JudgeProvider::default(),
        grader_model: None,
        grader_command: Some(script.to_string_lossy().into_owned()),
        grader_votes: JudgeVotes::single(),
        strict: false,
        output_format: OutputFormat::Text,
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::Success);
}
