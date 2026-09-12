//! The eval pipeline walked the way an operator walks it, through the real binary.
//!
//! Every step here is reachable without a runner on PATH, which is what makes the
//! walk runnable in CI at all: no harness is installed, no credentials exist, and
//! no network is reachable. What it protects is the seam between the steps, where a
//! scaffold the checker rejects, or a bundle no schema describes, would otherwise
//! only be found by whoever ran a real suite next.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;

fn trg() -> Command {
    Command::cargo_bin("trg").unwrap()
}

fn write_skill(root: &Path, name: &str) -> PathBuf {
    let skill_dir = root.join(name);
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: a skill to walk the pipeline with\n---\n\nBody.\n"),
    )
    .unwrap();
    skill_dir
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn scaffold_suite(skill_dir: &Path) {
    trg()
        .args(["ai", "skills", "eval", "init", "--skill-dir"])
        .arg(skill_dir)
        .assert()
        .success();
}

fn scaffold_bundle(skill_dir: &Path, out_dir: &Path) -> PathBuf {
    trg()
        .args(["ai", "skills", "eval", "run", "--skill-dir"])
        .arg(skill_dir)
        .arg("--out-dir")
        .arg(out_dir)
        .args(["--iteration", "1"])
        .assert()
        .success();

    let skill_out = out_dir.join(skill_dir.file_name().unwrap());
    let mut bundles: Vec<PathBuf> = fs::read_dir(&skill_out)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", skill_out.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    assert_eq!(
        bundles.len(),
        1,
        "expected one report bundle under {}",
        skill_out.display()
    );
    bundles.pop().unwrap()
}

/// What `init` writes has to be what `verify` accepts. The two commands read the same
/// manifest through the same checker, so a scaffold that fails strict verification means
/// the tool contradicts itself in the first two steps anyone takes.
#[test]
fn a_scaffolded_suite_passes_the_checker_that_gates_it() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");

    scaffold_suite(&skill_dir);
    assert!(skill_dir.join("evals/evals.json").is_file());

    trg()
        .args(["ai", "skills", "eval", "verify", "--skill-dir"])
        .arg(&skill_dir)
        .args(["--mode", "strict"])
        .assert()
        .success();
}

/// A pass with no runner is a scaffold, and scaffolding one is a legitimate use of
/// `eval run`, so it stays a success and the bundle it wrote stays readable.
#[test]
fn a_pass_with_no_runner_still_writes_a_bundle_verify_can_read() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);

    let report_dir = scaffold_bundle(&skill_dir, &temp.path().join("artifacts"));
    assert!(report_dir.join("report.json").is_file());

    trg()
        .args(["ai", "skills", "eval", "verify"])
        .arg(&report_dir)
        .assert()
        .success();
}

/// Whether a bundle conforms to the schemas is a fact about the writer that produced it, not
/// about how the suite scored. Read after the verdict, the check was reachable only on a
/// bundle whose every assertion passed, which no pass without a live runner can produce, so
/// nothing ever held a written bundle against the schemas that describe it.
#[test]
fn a_bundle_that_no_schema_describes_is_reported_even_when_the_suite_scored_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);
    let report_dir = scaffold_bundle(&skill_dir, &temp.path().join("artifacts"));

    let report_path = report_dir.join("report.json");
    let mut report: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    report["runs"][0]
        .as_object_mut()
        .expect("a run is an object")
        .remove("metrics")
        .expect("a run carries metrics");
    fs::write(&report_path, serde_json::to_string(&report).unwrap()).unwrap();

    let output = trg()
        .args(["ai", "skills", "eval", "verify"])
        .arg(&report_dir)
        .args(["--mode", "strict"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("Schema validation failed"),
        "expected the drift to be reported, got: {stderr}"
    );
    assert!(
        stderr.contains("metrics"),
        "expected the missing field to be named, got: {stderr}"
    );
}

/// The same check must not turn the ordinary "this bundle was never graded" answer into a
/// schema complaint, because a scaffolded bundle is well formed.
#[test]
fn a_bundle_that_was_never_graded_is_told_that_and_not_something_else() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);
    let report_dir = scaffold_bundle(&skill_dir, &temp.path().join("artifacts"));

    let output = trg()
        .args(["ai", "skills", "eval", "verify"])
        .arg(&report_dir)
        .args(["--mode", "strict"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stderr = stderr_of(&output);
    assert!(
        stderr.contains("must contain at least one grading.json"),
        "expected the ungraded bundle to be named as such, got: {stderr}"
    );
    assert!(
        !stderr.contains("Schema validation failed"),
        "a scaffolded bundle is well formed, got: {stderr}"
    );
}

/// The gate an operator asks for has to be a gate. A pass that scored nothing cannot meet a
/// minimum pass rate, and reporting it as met is how a suite stops being measured without
/// anyone losing a green check over it.
#[test]
fn a_minimum_pass_rate_fails_a_pass_that_scored_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);

    let output = trg()
        .args(["ai", "skills", "eval", "run", "--skill-dir"])
        .arg(&skill_dir)
        .arg("--out-dir")
        .arg(temp.path().join("artifacts"))
        .args(["--iteration", "1", "--min-pass-rate", "1.0"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("nothing was scored"),
        "expected the gate to say it could not be evaluated, got: {stdout}"
    );
}
