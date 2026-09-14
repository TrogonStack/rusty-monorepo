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
        .args(["--iteration", "1", "--trust-skill"])
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
        .args(["--iteration", "1", "--trust-skill", "--min-pass-rate", "1.0"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("nothing was scored"),
        "expected the gate to say it could not be evaluated, got: {stdout}"
    );
}

/// A bundle that is not there is not a bundle that scored badly.
///
/// Every stage downstream of `run` is handed a report directory, and a directory that
/// cannot be read leaves the stage with nothing to say about the skill. Reported as a
/// gate failure, each of these would send someone to read a diff that no measurement
/// ever pointed at.
#[test]
fn a_stage_handed_a_bundle_that_does_not_exist_reports_a_broken_tool_and_not_a_failed_gate() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("no-such-bundle");

    for stage in [
        vec!["verify"],
        vec!["grade"],
        vec!["benchmark"],
        vec!["iteration-summary"],
        vec!["compare"],
        vec!["next-iteration"],
        vec!["html-report"],
        vec!["feedback", "init"],
        vec!["feedback", "list"],
        vec!["feedback", "validate"],
    ] {
        let output = trg()
            .args(["ai", "skills", "eval"])
            .args(&stage)
            .arg(&missing)
            .output()
            .unwrap();

        assert_eq!(
            output.status.code(),
            Some(3),
            "`{}` was handed a bundle it could not read, which says nothing about the skill; got {:?} and {}",
            stage.join(" "),
            output.status.code(),
            stderr_of(&output)
        );
    }
}

/// A pass asked for an arm it was given no way to build never got as far as measuring
/// anything, so the refusal is about the invocation and not about the skill.
#[test]
fn a_pass_asked_for_an_arm_it_was_given_no_skill_dir_for_reports_a_broken_tool() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);

    let output = trg()
        .args(["ai", "skills", "eval", "run", "--skill-dir"])
        .arg(&skill_dir)
        .arg("--out-dir")
        .arg(temp.path().join("artifacts"))
        .args(["--iteration", "1", "--scenario", "old_skill"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3), "got {}", stderr_of(&output));
    assert!(
        stderr_of(&output).contains("--old-skill-dir is required"),
        "got {}",
        stderr_of(&output)
    );
}

/// A scaffold that would overwrite work is refused before it does, and the refusal is the
/// tool declining to act rather than a verdict on the suite it did not touch.
#[test]
fn a_scaffold_refused_rather_than_allowed_to_overwrite_reports_a_broken_tool() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);

    let output = trg()
        .args(["ai", "skills", "eval", "init", "--skill-dir"])
        .arg(&skill_dir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3), "got {}", stderr_of(&output));
}

/// The two reds have to stay apart under the same walk. A suite held to a gate it cannot
/// meet and a stage handed a bundle it cannot read are the only two ways this pipeline
/// goes red without a runner, and they must not answer with the same code.
#[test]
fn a_failed_gate_and_a_broken_tool_do_not_share_a_code() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);

    let gated = trg()
        .args(["ai", "skills", "eval", "run", "--skill-dir"])
        .arg(&skill_dir)
        .arg("--out-dir")
        .arg(temp.path().join("artifacts"))
        .args(["--iteration", "1", "--trust-skill", "--min-pass-rate", "1.0"])
        .output()
        .unwrap();

    let broken = trg()
        .args(["ai", "skills", "eval", "grade"])
        .arg(temp.path().join("no-such-bundle"))
        .output()
        .unwrap();

    assert_eq!(gated.status.code(), Some(1));
    assert_eq!(broken.status.code(), Some(3));
    assert_ne!(
        gated.status.code(),
        broken.status.code(),
        "a job that goes red has to say which kind of red it is"
    );
}

/// A path that is not a directory is not a bundle that failed its checks. The checker
/// reports it the same way it reports a real finding, so the distinction has to be drawn
/// before it is reached, and the two reasons have to stay apart in what is printed.
#[test]
fn a_workspace_that_is_not_a_directory_reports_a_broken_tool_and_says_which_reason() {
    let temp = tempfile::tempdir().unwrap();

    let a_file = temp.path().join("report.json");
    fs::write(&a_file, "{}").unwrap();
    let on_a_file = trg()
        .args(["ai", "skills", "eval", "verify"])
        .arg(&a_file)
        .output()
        .unwrap();

    assert_eq!(on_a_file.status.code(), Some(3), "got {}", stderr_of(&on_a_file));
    assert!(
        stderr_of(&on_a_file).contains("must be a directory"),
        "got {}",
        stderr_of(&on_a_file)
    );

    let missing = trg()
        .args(["ai", "skills", "eval", "verify"])
        .arg(temp.path().join("no-such-bundle"))
        .output()
        .unwrap();

    assert_eq!(missing.status.code(), Some(3), "got {}", stderr_of(&missing));
    assert!(
        stderr_of(&missing).contains("does not exist"),
        "the two reasons are not the same reason, got {}",
        stderr_of(&missing)
    );
}

/// An artifact the checker cannot parse is an artifact it never read. The bundle was there
/// and the checker got as far as opening it, which is exactly where the old code stopped
/// telling the two apart: a file of garbage would have reported that the skill failed.
#[test]
fn a_grading_artifact_that_cannot_be_parsed_reports_a_broken_tool_and_not_a_failed_gate() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);
    let report_dir = scaffold_bundle(&skill_dir, &temp.path().join("artifacts"));

    fs::write(report_dir.join("grading.json"), "not json at all").unwrap();

    let output = trg()
        .args(["ai", "skills", "eval", "verify"])
        .arg(&report_dir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3), "got {}", stderr_of(&output));
    assert_ne!(
        output.status.code(),
        Some(1),
        "nothing read the bundle, so nothing can be said about the skill"
    );
}
