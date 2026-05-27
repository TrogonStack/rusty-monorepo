use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn write_fixture_skill(root: &Path, relative_dir: &str, skill_name: &str) -> PathBuf {
    let skill_dir = root.join(relative_dir);
    fs::create_dir_all(skill_dir.join("evals")).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {skill_name}\ndescription: fixture\n---\n"),
    )
    .unwrap();
    fs::write(
        skill_dir.join("evals/evals.json"),
        format!(
            r#"{{
            "skill_name": "{skill_name}",
            "evals": [
                {{
                    "id": "one",
                    "prompt": "first prompt",
                    "expected_output": "first output",
                    "assertions": ["checks first"]
                }}
            ]
        }}"#
        ),
    )
    .unwrap();
    skill_dir
}

fn run_eval(skill_dir: &Path, out_dir: &Path, current_dir: Option<&Path>) {
    let mut cmd = Command::cargo_bin("trg").unwrap();
    if let Some(cwd) = current_dir {
        cmd.current_dir(cwd);
    }
    cmd.args([
        "ai",
        "skills",
        "eval",
        "run",
        "--skill-dir",
        &skill_dir.to_string_lossy(),
        "--out-dir",
        &out_dir.to_string_lossy(),
        "--iteration",
        "1",
    ])
    .assert()
    .success();
}

fn find_report_bundle(out_dir: &Path, skill_name: &str) -> PathBuf {
    let skill_out = out_dir.join(skill_name);
    let mut bundles: Vec<PathBuf> = fs::read_dir(&skill_out)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", skill_out.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(bundles.len(), 1, "expected one report bundle under {}", skill_out.display());
    bundles.pop().unwrap()
}

#[test]
fn run_accepts_skill_dir_with_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path(), "my skills/fixture-skill", "fixture-skill");
    let out_dir = temp.path().join("artifacts");

    run_eval(&skill_dir, &out_dir, None);

    let report_dir = find_report_bundle(&out_dir, "fixture-skill");
    assert!(report_dir.join("report.json").is_file());
}

#[test]
fn run_accepts_absolute_skill_dir() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path(), "fixture-skill", "fixture-skill");
    let out_dir = temp.path().join("artifacts");
    fs::create_dir_all(&out_dir).unwrap();
    let absolute_skill = fs::canonicalize(&skill_dir).unwrap();
    let absolute_out = fs::canonicalize(&out_dir).unwrap();

    run_eval(&absolute_skill, &absolute_out, None);

    let report_dir = find_report_bundle(&absolute_out, "fixture-skill");
    assert!(report_dir.join("report.json").is_file());
}

#[test]
fn run_accepts_relative_skill_dir_from_temp_cwd() {
    let temp = tempfile::tempdir().unwrap();
    let layout = temp.path().join("layout");
    fs::create_dir_all(&layout).unwrap();
    let skill_dir = write_fixture_skill(&layout, "skills/fixture-skill", "fixture-skill");
    let out_dir = layout.join("artifacts");
    fs::create_dir_all(&out_dir).unwrap();

    run_eval(
        Path::new("skills/fixture-skill"),
        Path::new("artifacts"),
        Some(&layout),
    );

    let report_dir = find_report_bundle(&out_dir, "fixture-skill");
    assert!(report_dir.join("report.json").is_file());
    assert_eq!(skill_dir, layout.join("skills/fixture-skill"));
}

#[test]
fn run_fails_fast_when_runner_missing_from_path() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path(), "fixture-skill", "fixture-skill");
    let out_dir = temp.path().join("artifacts");

    let output = Command::cargo_bin("trg")
        .unwrap()
        .env("PATH", "/nonexistent")
        .args([
            "ai",
            "skills",
            "eval",
            "run",
            "--skill-dir",
            &skill_dir.to_string_lossy(),
            "--out-dir",
            &out_dir.to_string_lossy(),
            "--runner",
            "codex",
            "--iteration",
            "1",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Runner 'codex' not found on PATH"));
    assert!(stderr.contains("Looked for binary: codex"));

    assert!(
        !out_dir.join("fixture-skill").exists(),
        "expected no report bundle before runner availability check passes"
    );
}
