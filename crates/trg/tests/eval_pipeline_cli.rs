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

fn write_iteration_report(report_dir: &Path, iteration: u32, report_id: &str, runs: &[(String, &str, &str, bool)]) {
    fs::create_dir_all(report_dir).unwrap();
    let run_values: Vec<serde_json::Value> = runs
        .iter()
        .map(|(id, eval_case_id, split, _)| {
            serde_json::json!({
                "id": id,
                "eval_case_id": eval_case_id,
                "split": split,
                "scenario_id": "with_skill",
                "model_config_id": "ci-default",
                "skill_revision_id": "current",
                "attempt": 1,
                "status": "completed",
                "paths": { "workspace": format!("runs/{id}/workspace") },
                "artifacts": [],
                "metrics": {}
            })
        })
        .collect();
    let report = serde_json::json!({
        "report": {
            "id": report_id,
            "generated_at": "2026-05-26T00:00:00Z",
            "iteration": iteration,
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo",
            "skill_path": "demo",
            "skill_hash": "sha256:abc",
            "evals_path": "demo/evals/evals.json",
            "evals_hash": "sha256:def"
        },
        "dimensions": {
            "eval_cases": [],
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": run_values,
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();

    for (id, _, _, passed) in runs {
        let run_dir = report_dir.join(format!("runs/{id}"));
        fs::create_dir_all(run_dir.join("workspace")).unwrap();
        fs::write(
            run_dir.join("grading.json"),
            format!(r#"{{"assertion_results":[{{"assertion":"a","passed":{passed}}}]}}"#),
        )
        .unwrap();
    }
}

/// `--fail-on` is opt-in: the same regressed test split exits clean on its own and only
/// reports the gate's own exit code once the command is told to watch for a revert.
#[test]
fn fail_on_revert_gates_iteration_summary_on_a_regressed_test_split() {
    let temp = tempfile::tempdir().unwrap();
    let previous_dir = temp.path().join("report-iter-1");
    let current_dir = temp.path().join("report-iter-2");

    let previous_runs: Vec<(String, &str, &str, bool)> =
        (0..20).map(|i| (format!("prev-{i}"), "case-a", "test", true)).collect();
    let current_runs: Vec<(String, &str, &str, bool)> =
        (0..20).map(|i| (format!("cur-{i}"), "case-a", "test", false)).collect();

    write_iteration_report(&previous_dir, 1, "report-iter-1", &previous_runs);
    write_iteration_report(&current_dir, 2, "report-iter-2", &current_runs);

    let ungated = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&current_dir)
        .arg("--previous")
        .arg(&previous_dir)
        .output()
        .unwrap();
    assert_eq!(
        ungated.status.code(),
        Some(0),
        "the regression is real but --fail-on was never asked for, got {}",
        stderr_of(&ungated)
    );

    let gated = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&current_dir)
        .arg("--previous")
        .arg(&previous_dir)
        .args(["--fail-on", "revert"])
        .output()
        .unwrap();
    assert_eq!(gated.status.code(), Some(1), "got {}", stderr_of(&gated));
    assert!(
        stderr_of(&gated).contains("revert"),
        "expected the gate to name the recommendation it fired on, got {}",
        stderr_of(&gated)
    );

    let gated_on_the_other_condition = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&current_dir)
        .arg("--previous")
        .arg(&previous_dir)
        .args(["--fail-on", "overfitting"])
        .output()
        .unwrap();
    assert_eq!(
        gated_on_the_other_condition.status.code(),
        Some(0),
        "a revert is not overfitting, got {}",
        stderr_of(&gated_on_the_other_condition)
    );
}

/// A `--previous` the caller named but that cannot be read is a broken tool invocation, not
/// silence about there being nothing to compare against: it has to fail closed at the same
/// exit code every other unreadable-bundle path in this walk reports, not the clean exit a
/// first iteration with no `--previous` at all gets.
#[test]
fn an_unreadable_previous_report_fails_closed_instead_of_gating_on_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let current_dir = temp.path().join("report-iter-2");
    let missing_previous = temp.path().join("no-such-previous");

    let current_runs: Vec<(String, &str, &str, bool)> =
        (0..4).map(|i| (format!("cur-{i}"), "case-a", "test", true)).collect();
    write_iteration_report(&current_dir, 2, "report-iter-2", &current_runs);

    let named_but_unreadable = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&current_dir)
        .arg("--previous")
        .arg(&missing_previous)
        .output()
        .unwrap();
    assert_eq!(
        named_but_unreadable.status.code(),
        Some(3),
        "a named --previous that cannot be read must fail closed, got {:?} and {}",
        named_but_unreadable.status.code(),
        stderr_of(&named_but_unreadable)
    );

    let none_named = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&current_dir)
        .output()
        .unwrap();
    assert_eq!(
        none_named.status.code(),
        Some(0),
        "no --previous at all has nothing to compare against, and that is fine, got {}",
        stderr_of(&none_named)
    );
}

/// `--withhold-test-detail` is what makes it safe for a caller like the hillclimb loop to
/// read this command's full JSON output: without it, the held-out case id shows up in
/// several places (the top-level stability lists, the test split's own summary, the test
/// split's headroom warning), and the flag has to clear every one of them while leaving the
/// train-split case and the aggregate counts alone.
#[test]
fn withhold_test_detail_removes_the_held_out_case_id_from_the_json_output() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report-iter-1");

    let mut runs: Vec<(String, &str, &str, bool)> = (0..4)
        .map(|i| (format!("held-out-{i}"), "case-held-out", "test", true))
        .collect();
    runs.extend((0..4).map(|i| (format!("train-{i}"), "case-train", "train", true)));
    write_iteration_report(&report_dir, 1, "report-iter-1", &runs);

    let leaky = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&report_dir)
        .args(["--output-format", "json"])
        .output()
        .unwrap();
    assert_eq!(leaky.status.code(), Some(0), "got {}", stderr_of(&leaky));
    let leaky_stdout = String::from_utf8_lossy(&leaky.stdout);
    assert!(
        leaky_stdout.contains("case-held-out"),
        "sanity check: without the flag the held-out case must appear, or this test proves nothing"
    );

    let withheld = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&report_dir)
        .args(["--output-format", "json"])
        .arg("--withhold-test-detail")
        .output()
        .unwrap();
    assert_eq!(withheld.status.code(), Some(0), "got {}", stderr_of(&withheld));
    let withheld_stdout = String::from_utf8_lossy(&withheld.stdout);
    assert!(
        !withheld_stdout.contains("case-held-out"),
        "the held-out case id must not appear anywhere in the output, got: {withheld_stdout}"
    );
    assert!(
        withheld_stdout.contains("case-train"),
        "the train-split case id must still appear, got: {withheld_stdout}"
    );
}

/// A scaffolded suite declares no `test` case, so the improvement bundle it produces has
/// nothing to withhold. The bundle says so explicitly rather than reporting an empty
/// `held_out` field, and the markdown nudges toward declaring one, since every later
/// overfitting check depends on a held-out set existing.
#[test]
fn next_iteration_reports_no_test_cases_and_nudges_toward_declaring_one() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill(temp.path(), "pipeline-skill");
    scaffold_suite(&skill_dir);
    let report_dir = scaffold_bundle(&skill_dir, &temp.path().join("artifacts"));

    let output = trg()
        .args(["ai", "skills", "eval", "next-iteration"])
        .arg(&report_dir)
        .args(["--skill-dir"])
        .arg(&skill_dir)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "got {}", stderr_of(&output));

    let bundle_dir = report_dir.parent().unwrap().join("next-iteration");
    let document: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(bundle_dir.join("improvement-bundle.json")).unwrap()).unwrap();
    assert_eq!(document["held_out"]["status"], "no_test_cases");

    let markdown = fs::read_to_string(bundle_dir.join("improvement-bundle.md")).unwrap();
    assert!(markdown.contains("Held-Out Test Cases"));
    assert!(markdown.contains("Declare at least one case"));
}

/// A saturated `with_skill` arm is a suite with nothing left to measure from, and the
/// warning has to reach the operator through both output formats: prominently in the
/// text summary, and as a typed field in the JSON document a script would parse instead.
#[test]
fn benchmark_and_iteration_summary_warn_when_the_with_skill_arm_has_no_headroom_left() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    let runs: Vec<(String, &str, &str, bool)> = (0..40).map(|i| (format!("run-{i}"), "case-a", "test", true)).collect();
    write_iteration_report(&report_dir, 1, "report-a", &runs);

    let benchmark_text = trg()
        .args(["ai", "skills", "eval", "benchmark"])
        .arg(&report_dir)
        .output()
        .unwrap();
    assert_eq!(
        benchmark_text.status.code(),
        Some(0),
        "got {}",
        stderr_of(&benchmark_text)
    );
    let stdout = String::from_utf8_lossy(&benchmark_text.stdout);
    assert!(
        stdout.contains("WARN") && stdout.contains("no headroom left"),
        "expected a headroom warning in the text summary, got {stdout}"
    );

    let benchmark_json = trg()
        .args(["ai", "skills", "eval", "benchmark"])
        .arg(&report_dir)
        .args(["--output-format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        benchmark_json.status.code(),
        Some(0),
        "got {}",
        stderr_of(&benchmark_json)
    );
    let document: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&benchmark_json.stdout)).unwrap();
    assert_eq!(document["headroom"]["saturated_case_ids"][0], "case-a");
    assert_eq!(
        document["by_split"]["test"]["headroom"]["saturated_case_ids"][0],
        "case-a"
    );

    let summary_text = trg()
        .args(["ai", "skills", "eval", "iteration-summary"])
        .arg(&report_dir)
        .output()
        .unwrap();
    assert_eq!(summary_text.status.code(), Some(0), "got {}", stderr_of(&summary_text));
    let stdout = String::from_utf8_lossy(&summary_text.stdout);
    assert!(
        stdout.contains("WARN") && stdout.contains("no headroom left"),
        "expected a headroom warning in the text summary, got {stdout}"
    );
}

/// `--headroom-threshold` moves the floor a with_skill arm is judged saturated against,
/// so an arm the default would call saturated can be told it still has room.
#[test]
fn headroom_threshold_is_configurable_from_the_command_line() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    let runs: Vec<(String, &str, &str, bool)> = (0..40).map(|i| (format!("run-{i}"), "case-a", "test", true)).collect();
    write_iteration_report(&report_dir, 1, "report-a", &runs);

    let output = trg()
        .args(["ai", "skills", "eval", "benchmark"])
        .arg(&report_dir)
        .args(["--headroom-threshold", "0.999", "--output-format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "got {}", stderr_of(&output));
    let document: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert!(
        document["headroom"].is_null(),
        "a stricter threshold than the arm's own floor should not report saturation"
    );
}

/// A report bundle with exactly one graded, llm-scored assertion, laid out well enough
/// for `feedback label` to accept a verdict on it and for `grader-agreement` to join
/// that verdict against `grading.json`.
fn write_agreement_report(report_dir: &Path, run_id: &str, passed: bool) {
    fs::create_dir_all(report_dir).unwrap();
    let report = serde_json::json!({
        "report": {
            "id": "report-test",
            "generated_at": "2026-05-26T00:00:00Z",
            "iteration": 1,
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo",
            "skill_path": "demo",
            "skill_hash": "sha256:abc",
            "evals_path": "demo/evals/evals.json",
            "evals_hash": "sha256:def"
        },
        "dimensions": {
            "eval_cases": [{
                "id": "case-a",
                "slug": "case-a",
                "prompt": "do the thing",
                "expected_output": "the thing, done",
                "files": [],
                "assertion_ids": ["case-a:g0"]
            }],
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": [{
            "id": run_id,
            "eval_case_id": "case-a",
            "eval_slug": "case-a",
            "split": "train",
            "scenario_id": "with_skill",
            "iteration": 1,
            "model_config_id": "ci-default",
            "skill_revision_id": "current",
            "attempt": 1,
            "status": "completed",
            "paths": {
                "workspace": format!("runs/{run_id}/workspace"),
                "outputs": format!("runs/{run_id}/workspace/outputs")
            },
            "mirror_path": "iterations/1/case-a/with_skill/attempt-1",
            "artifacts": [],
            "metrics": {}
        }],
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();

    let run_dir = report_dir.join(format!("runs/{run_id}"));
    fs::create_dir_all(run_dir.join("workspace")).unwrap();
    let grading = serde_json::json!({
        "assertion_results": [{
            "assertion": "assert something",
            "passed": passed,
            "evidence": "the transcript said so",
            "grader": { "kind": "llm", "model": "test-model" }
        }],
        "summary": {
            "passed": usize::from(passed),
            "failed": usize::from(!passed),
            "total": 1
        }
    });
    fs::write(
        run_dir.join("grading.json"),
        serde_json::to_string_pretty(&grading).unwrap(),
    )
    .unwrap();
}

/// A human label recorded through `feedback label` has to be the same one
/// `grader-agreement` reads back out, through the real binary end to end.
#[test]
fn feedback_label_then_grader_agreement_walk_the_real_binary() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_agreement_report(&report_dir, "run-001", true);

    trg()
        .args(["ai", "skills", "eval", "feedback", "label"])
        .arg(&report_dir)
        .args(["--run", "run-001", "--assertion", "case-a:g0"])
        .args(["--verdict", "pass", "--reviewer", "reviewer@example.com"])
        .assert()
        .success();

    let output = trg()
        .args(["ai", "skills", "eval", "grader-agreement"])
        .arg(&report_dir)
        .args(["--output-format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "got {}", stderr_of(&output));

    let document: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(document["overall"]["agreements"], 1);
    assert_eq!(document["by_grader_kind"]["llm"]["agreements"], 1);
    assert_eq!(document["coverage"]["labeled_assertions"], 1);
    assert!(report_dir.join("grader-agreement.json").is_file());
}

/// A judge unanimous and wrong is exactly what `--min-agreement` exists to catch, and the
/// gate has to fire from the real binary, not only from the library function underneath it.
#[test]
fn a_confidently_wrong_judge_fails_the_min_agreement_gate() {
    let temp = tempfile::tempdir().unwrap();
    let report_dir = temp.path().join("report");
    write_agreement_report(&report_dir, "run-001", true);

    trg()
        .args(["ai", "skills", "eval", "feedback", "label"])
        .arg(&report_dir)
        .args(["--run", "run-001", "--assertion", "case-a:g0"])
        .args(["--verdict", "fail", "--reviewer", "reviewer@example.com"])
        .assert()
        .success();

    let output = trg()
        .args(["ai", "skills", "eval", "grader-agreement"])
        .arg(&report_dir)
        .args(["--min-agreement", "0.5"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "got {}", stderr_of(&output));
    assert!(stderr_of(&output).contains("min-agreement"));
}

/// Like `write_iteration_report`, but with every field `ReportDocument`'s own strict
/// `Deserialize` requires, since `scaling` reads a bundle back as that full type rather
/// than the lenient summary `iteration-summary` reads it as.
fn write_scaling_report(
    report_dir: &Path,
    report_id: &str,
    model_config_id: &str,
    runs: &[(String, &str, &str, bool)],
) {
    fs::create_dir_all(report_dir).unwrap();
    let run_values: Vec<serde_json::Value> = runs
        .iter()
        .map(|(id, eval_case_id, split, _)| {
            serde_json::json!({
                "id": id,
                "eval_case_id": eval_case_id,
                "eval_slug": eval_case_id,
                "split": split,
                "scenario_id": "with_skill",
                "iteration": 1,
                "model_config_id": model_config_id,
                "skill_revision_id": "current",
                "attempt": 1,
                "status": "completed",
                "paths": { "workspace": format!("runs/{id}/workspace"), "outputs": format!("runs/{id}/workspace/outputs") },
                "mirror_path": format!("iteration-1/eval-{eval_case_id}/with_skill/attempt-1/"),
                "artifacts": [],
                "metrics": {},
                "cache": null
            })
        })
        .collect();
    let report = serde_json::json!({
        "report": {
            "id": report_id,
            "generated_at": "2026-05-26T00:00:00Z",
            "iteration": 1,
            "producer": { "name": "trg", "version": "0.3.0" }
        },
        "suite": {
            "skill_name": "demo",
            "skill_path": "demo",
            "skill_hash": "sha256:abc",
            "evals_path": "demo/evals/evals.json",
            "evals_hash": "sha256:def"
        },
        "dimensions": {
            "eval_cases": [],
            "assertions": [],
            "skill_revisions": [],
            "model_configs": [{ "id": model_config_id, "capture_status": "incomplete", "label": model_config_id, "parameters": {}, "parameter_sources": {}, "extra": {} }],
            "scenarios": [],
            "grading_strategies": []
        },
        "runs": run_values,
        "assertion_results": [],
        "summaries": { "by_scenario": [] },
        "comparisons": []
    });
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();

    for (id, _, _, passed) in runs {
        let run_dir = report_dir.join(format!("runs/{id}"));
        fs::create_dir_all(run_dir.join("workspace")).unwrap();
        fs::write(
            run_dir.join("grading.json"),
            format!(r#"{{"assertion_results":[{{"assertion":"a","passed":{passed}}}]}}"#),
        )
        .unwrap();
    }
}

/// A stronger configuration that clears the noise on the same suite reads as improved
/// through the real binary, and `--fail-on-regression` only fires once a weaker
/// configuration is asked for last.
#[test]
fn scaling_reads_a_cleared_gain_as_improved_and_gates_on_a_reversed_order() {
    let temp = tempfile::tempdir().unwrap();
    let weaker_dir = temp.path().join("report-small-model");
    let stronger_dir = temp.path().join("report-large-model");

    let weaker_runs: Vec<(String, &str, &str, bool)> = vec![
        ("weak-a".to_string(), "case-a", "train", false),
        ("weak-b".to_string(), "case-b", "train", false),
    ];
    let stronger_runs: Vec<(String, &str, &str, bool)> = vec![
        ("strong-a".to_string(), "case-a", "train", true),
        ("strong-b".to_string(), "case-b", "train", true),
    ];
    write_scaling_report(&weaker_dir, "report-small-model", "small-model", &weaker_runs);
    write_scaling_report(&stronger_dir, "report-large-model", "large-model", &stronger_runs);

    let ungated = trg()
        .args(["ai", "skills", "eval", "scaling"])
        .arg(&weaker_dir)
        .arg(&stronger_dir)
        .args(["--output-format", "json"])
        .output()
        .unwrap();
    assert_eq!(ungated.status.code(), Some(0), "got {}", stderr_of(&ungated));
    let document: serde_json::Value = serde_json::from_slice(&ungated.stdout).unwrap();
    assert_eq!(document["steps"][0]["comparison"]["overall"]["status"], "improved");
    assert_eq!(document["bundles"][0]["model_config"]["id"], "small-model");
    assert_eq!(document["bundles"][1]["model_config"]["id"], "large-model");
    assert!(stronger_dir.join("scaling.json").is_file());

    let ungated_reversed = trg()
        .args(["ai", "skills", "eval", "scaling"])
        .arg(&stronger_dir)
        .arg(&weaker_dir)
        .output()
        .unwrap();
    assert_eq!(
        ungated_reversed.status.code(),
        Some(0),
        "the regression is real but --fail-on-regression was never asked for, got {}",
        stderr_of(&ungated_reversed)
    );

    let gated_reversed = trg()
        .args(["ai", "skills", "eval", "scaling"])
        .arg(&stronger_dir)
        .arg(&weaker_dir)
        .arg("--fail-on-regression")
        .output()
        .unwrap();
    assert_eq!(
        gated_reversed.status.code(),
        Some(1),
        "got {}",
        stderr_of(&gated_reversed)
    );
}
