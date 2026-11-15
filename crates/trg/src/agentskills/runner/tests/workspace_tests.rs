use super::super::*;
use crate::agentskills::evals::EvalCase;
use crate::agentskills::redact::{is_secret_env_key, redact_command_args, redact_env};
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

fn make_case(files: Vec<String>) -> EvalCase {
    serde_json::from_value(serde_json::json!({
        "id": "case-1",
        "prompt": "do the thing",
        "expected_output": "done",
        "files": files,
    }))
    .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn test_request<'a>(
    case: &'a EvalCase,
    scenario: ScenarioKind,
    skill_md: &'a str,
    skill_path: &'a Path,
    workspace: &'a Path,
    transcript_path: &'a Path,
    stderr_path: &'a Path,
    old_skill_md: Option<&'a str>,
    old_skill_path: Option<&'a Path>,
) -> EvalRunRequest<'a> {
    EvalRunRequest {
        eval: case,
        scenario,
        skill_md,
        skill_path,
        eval_dir: EvalDirName::default(),
        old_skill_md,
        old_skill_path,
        workspace_dir: workspace,
        transcript_path,
        stderr_path,
        runner_model: None,
        timeout_secs: None,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        scaffold_permission: ScaffoldPermission::Withheld,
        tool_grant: None,
        mcp_config: None,
        telemetry_forwarding: TelemetryForwarding::Off,
        caps: RunCaps::default(),
    }
}

const SCAFFOLD_SKILL_MD: &str = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";

fn make_scaffolded_case(script: &str) -> EvalCase {
    serde_json::from_value(serde_json::json!({
        "id": "case-1",
        "prompt": "do the thing",
        "expected_output": "done",
        "scaffold": script,
    }))
    .unwrap()
}

fn write_scaffold(skill_path: &Path, relative: &str, body: &str) {
    let script = skill_path.join(relative);
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, body).unwrap();
    #[cfg(unix)]
    {
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn scaffold_fixture(body: &str) -> (tempfile::TempDir, PathBuf, EvalCase) {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(&skill_path).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    write_scaffold(&skill_path, "evals/scaffold.sh", body);
    (temp, skill_path, make_scaffolded_case("evals/scaffold.sh"))
}

fn prepare_scaffolded(
    temp: &tempfile::TempDir,
    skill_path: &Path,
    case: &EvalCase,
    permission: ScaffoldPermission,
) -> (PathBuf, Result<PreparedRun, RunnerError>) {
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = EvalRunRequest {
        scaffold_permission: permission,
        ..test_request(
            case,
            ScenarioKind::WithSkill,
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
            skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        )
    };
    let prepared = prepare_workspace(&request, Runner::ClaudeCode);
    (workspace, prepared)
}

/// A case that says what state it is asking about gets that state, and gets it from trg
/// rather than from a harness, so the same case states the same thing to every one.
#[test]
fn a_case_that_declares_state_runs_in_that_state() {
    let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\nmkdir -p repo\necho seeded > repo/README.md\n");

    let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

    prepared.expect("a granted scaffold runs");
    assert_eq!(
        std::fs::read_to_string(workspace.join("repo/README.md")).unwrap(),
        "seeded\n"
    );
}

/// A case scored against a workspace it never asked for reads as an answer about the
/// skill when it is an answer about the wrong directory, so the run fails instead.
#[test]
fn a_declared_scaffold_nobody_allowed_fails_the_run_rather_than_running_without_it() {
    let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\nmkdir -p repo\n");

    let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Withheld);

    let error = prepared.expect_err("a withheld scaffold refuses the run");
    let message = error.to_string();
    assert!(
        message.contains("--allow-scaffold"),
        "expected the flag to be named: {message}"
    );
    assert!(
        message.contains("evals/scaffold.sh"),
        "expected the script to be named: {message}"
    );
    assert!(
        !workspace.join("repo").exists(),
        "nothing the script asked for was created"
    );
}

/// A scaffold that did not finish leaves a directory nobody described, so the run it was
/// setting up cannot be reported as an answer about anything.
#[test]
fn a_scaffold_that_failed_fails_the_run_it_was_setting_up() {
    let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\necho 'no disk' >&2\nexit 3\n");

    let (_workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

    let message = prepared.expect_err("a failing scaffold refuses the run").to_string();
    assert!(message.contains("exit code 3"), "expected the exit code: {message}");
    assert!(
        message.contains("no disk"),
        "expected the script's own words: {message}"
    );
}

/// The case's own fixtures are staged after the scaffold, so a file the case declared is
/// the file the run gets even when the script wrote to the same path.
#[test]
fn what_a_case_declares_outlives_what_its_scaffold_wrote() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals/files")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/files/input.txt"), "declared").unwrap();
    write_scaffold(
        &skill_path,
        "evals/scaffold.sh",
        "#!/bin/sh\nmkdir -p evals/files\necho scaffolded > evals/files/input.txt\n",
    );
    let case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "case-1",
        "prompt": "do the thing",
        "expected_output": "done",
        "files": ["evals/files/input.txt"],
        "scaffold": "evals/scaffold.sh",
    }))
    .unwrap();

    let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

    prepared.expect("a granted scaffold runs");
    assert_eq!(
        std::fs::read_to_string(workspace.join("evals/files/input.txt")).unwrap(),
        "declared"
    );
}

/// The script is spawned as a path under the skill directory while the child changes
/// into the workspace first, so a skill directory given relative to where trg was run
/// has to be resolved before that change of directory rather than after it.
#[test]
fn a_scaffold_runs_when_the_skill_directory_is_a_relative_path() {
    let nearby = tempfile::TempDir::new_in(".").unwrap();
    let skill_path =
        PathBuf::from(nearby.path().file_name().expect("a name under the current directory")).join("skill");
    assert!(skill_path.is_relative(), "the point of the test is the relative path");
    std::fs::create_dir_all(&skill_path).unwrap();
    std::fs::write(skill_path.join("SKILL.md"), SCAFFOLD_SKILL_MD).unwrap();
    write_scaffold(
        &skill_path,
        "evals/scaffold.sh",
        "#!/bin/sh\necho seeded > seeded.txt\n",
    );
    let case = make_scaffolded_case("evals/scaffold.sh");

    let elsewhere = tempdir().unwrap();
    let workspace = elsewhere.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = EvalRunRequest {
        scaffold_permission: ScaffoldPermission::Granted,
        ..test_request(
            &case,
            ScenarioKind::WithSkill,
            SCAFFOLD_SKILL_MD,
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        )
    };

    prepare_workspace(&request, Runner::ClaudeCode).expect("a relative skill directory still finds its scaffold");

    assert_eq!(
        std::fs::read_to_string(workspace.join("seeded.txt")).unwrap(),
        "seeded\n"
    );
}

/// A retry is another run of the same case, so it is handed the state the case declared
/// and not whatever the attempt before it left in the directory.
#[test]
fn a_workspace_prepared_again_does_not_keep_what_the_last_attempt_left() {
    let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\nmkdir -p repo\necho seeded > repo/README.md\n");

    let (workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);
    prepared.expect("a granted scaffold runs");
    std::fs::write(workspace.join("repo/README.md"), "rewritten by the agent").unwrap();
    std::fs::write(workspace.join("half-done.txt"), "from the attempt that failed").unwrap();

    let (_, again) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

    again.expect("the retry prepares the same workspace");
    assert!(
        !workspace.join("half-done.txt").exists(),
        "the retry asks the same question, so it starts from the same directory"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("repo/README.md")).unwrap(),
        "seeded\n"
    );
}

/// Clearing the workspace has to stop at the workspace. The staged skill is a tree of
/// symlinks into the skill directory, and following them would take the skill down with
/// the attempt that failed.
#[test]
fn clearing_a_workspace_does_not_reach_the_skill_it_staged() {
    let (temp, skill_path, case) = scaffold_fixture("#!/bin/sh\n:\n");

    let (_, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);
    prepared.expect("a granted scaffold runs");
    let (_, again) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Granted);

    again.expect("the retry prepares the same workspace");
    assert!(
        skill_path.join("SKILL.md").exists(),
        "the skill the next attempt reads is still there"
    );
    assert!(skill_path.join("evals/scaffold.sh").exists());
}

/// A case that declares nothing is untouched by any of this, and the permission it was
/// never asked for does not decide whether it runs.
#[test]
fn a_case_that_declares_no_state_is_unaffected_by_the_permission() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(&skill_path).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    let case = make_case(vec![]);

    let (_workspace, prepared) = prepare_scaffolded(&temp, &skill_path, &case, ScaffoldPermission::Withheld);

    prepared.expect("a case with no scaffold needs no permission");
}

/// A scaffold outside the skill directory is refused when the manifest is read, not when
/// a run reaches for it.
#[test]
fn a_scaffold_that_leaves_the_skill_directory_is_refused_at_parse_time() {
    let error = serde_json::from_value::<EvalCase>(serde_json::json!({
        "id": "case-1",
        "prompt": "do the thing",
        "expected_output": "done",
        "scaffold": "../../etc/setup.sh",
    }))
    .expect_err("a path that escapes is refused");
    assert!(
        error.to_string().contains("must stay inside the skill directory"),
        "got: {error}"
    );
}

#[test]
fn prepare_with_skill_symlinks_skill_and_prefixes_prompt() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals/files")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/files/input.txt"), "hello").unwrap();

    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let case = make_case(vec!["evals/files/input.txt".to_string()]);
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
    assert!(prepared.prompt.contains("Skill available at: .skill/"));
    assert!(prepared.prompt.contains("name: test-skill"));
    assert!(!prepared.prompt.contains("# Skill"));
    assert!(prepared.prompt.contains("do the thing"));
    assert!(prepared.prompt.contains("outputs/"));
    assert!(workspace.join("outputs").is_dir());

    let link = workspace.join(".skill");
    assert!(link.is_dir());
    assert!(link.join("SKILL.md").is_file());
    assert!(
        link.join("SKILL.md")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "staged entries are symlinks so staging stays cheap"
    );
    assert!(workspace.join("evals/files/input.txt").is_file());
}

/// What a run records as staged is read against the paths the run named, so it
/// has to be the directory the workspace was staged at rather than a second guess
/// at where staging put it.
#[test]
fn a_run_records_the_directory_it_staged_the_skill_at() {
    for disclosure in ["announced", "unannounced"] {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        let skill_md = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";
        std::fs::write(skill_path.join("SKILL.md"), skill_md).unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "case-1",
            "prompt": "do the thing",
            "expected_output": "done",
            "skill_disclosure": disclosure,
        }))
        .unwrap();
        let request = test_request(
            &case,
            ScenarioKind::WithSkill,
            skill_md,
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        );

        prepare_workspace(&request, Runner::ClaudeCode).unwrap();

        let StagedSkill::At { directory, .. } = staged_skill(&request).unwrap() else {
            panic!("a with-skill run staged a skill");
        };
        assert!(
            workspace.join(directory.as_str()).join("SKILL.md").is_file(),
            "{disclosure} recorded {directory:?}, which is not where the skill was staged"
        );
    }
}

/// A triggering case asks whether the run reaches for the skill on its own, so
/// the skill has to sit where a listing of the workspace reports it and the
/// prompt has to stay silent about it.
#[test]
fn an_unannounced_case_stages_the_skill_in_plain_sight_and_names_it_nowhere() {
    for staging in [SkillStaging::Symlink, SkillStaging::Copy] {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(&skill_path).unwrap();
        let skill_md = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";
        std::fs::write(skill_path.join("SKILL.md"), skill_md).unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "triggers-the-skill",
            "prompt": "do the thing",
            "expected_output": "done",
            "skill_disclosure": "unannounced",
        }))
        .unwrap();
        let mut request = test_request(
            &case,
            ScenarioKind::WithSkill,
            skill_md,
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        );
        request.skill_staging = staging;

        let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
        assert!(
            !prepared.prompt.contains("Skill available at:"),
            "{staging:?}: an unannounced case must not be told where the skill is"
        );
        assert!(
            !prepared.prompt.contains("test-skill"),
            "{staging:?}: an unannounced case must not be told which skill to use"
        );
        assert!(
            !workspace.join(".skill").exists(),
            "{staging:?}: the hidden link is what the prompt points at, and nothing points here"
        );
        assert!(
            workspace.join("skills/test-skill/SKILL.md").is_file(),
            "{staging:?}: the skill must be discoverable from the workspace"
        );
    }
}

fn write_companion_skill(dir: &Path, name: &str, description: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n# Companion\n"),
    )
    .unwrap();
}

fn companion_fixture() -> (tempfile::TempDir, PathBuf, EvalCase) {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(&skill_path).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    write_companion_skill(
        &skill_path.join("evals/companions/other-skill"),
        "other-skill",
        "Does a different job entirely.",
    );
    let case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "triggers-the-skill",
        "prompt": "do the thing",
        "expected_output": "done",
        "skill_disclosure": "unannounced",
        "companion_skills": ["evals/companions/other-skill"],
    }))
    .unwrap();
    (temp, skill_path, case)
}

/// A workspace holding one skill answers the routing question for the run. The
/// companion is what leaves the question open, so it has to arrive as a peer of the
/// skill under test rather than anywhere a prompt would have to point at.
#[test]
fn an_unannounced_case_stages_its_companions_beside_the_skill_under_test() {
    for staging in [SkillStaging::Symlink, SkillStaging::Copy] {
        let (temp, skill_path, case) = companion_fixture();
        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let skill_md = std::fs::read_to_string(skill_path.join("SKILL.md")).unwrap();
        let mut request = test_request(
            &case,
            ScenarioKind::WithSkill,
            &skill_md,
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        );
        request.skill_staging = staging;

        let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();

        assert!(
            workspace.join("skills/test-skill/SKILL.md").is_file(),
            "{staging:?}: the skill under test still has to be there"
        );
        assert!(
            workspace.join("skills/other-skill/SKILL.md").is_file(),
            "{staging:?}: the companion has to be a peer of it"
        );
        assert!(
            !prepared.prompt.contains("other-skill"),
            "{staging:?}: a distractor the prompt names is not one the run had to pass over"
        );
    }
}

/// The control arm exists so the gap between the arms is the skill under test. Staging
/// the distractors in only one arm would make the gap the skill plus every other skill
/// the case declared, which is not the quantity the arm was built to isolate.
#[test]
fn the_without_skill_arm_gets_the_same_companions_so_the_arms_differ_only_in_the_skill() {
    let (temp, skill_path, case) = companion_fixture();
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let skill_md = std::fs::read_to_string(skill_path.join("SKILL.md")).unwrap();
    let request = test_request(
        &case,
        ScenarioKind::WithoutSkill,
        &skill_md,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).unwrap();

    assert!(
        workspace.join("skills/other-skill/SKILL.md").is_file(),
        "the control arm has to hold the same distractors the treatment arm does"
    );
    assert!(
        !workspace.join("skills/test-skill").exists(),
        "the one thing the control arm must not hold is the skill under test"
    );
}

/// Two companions landing on one directory would stage as one skill, so the run would
/// be handed fewer choices than the case declares while the case still reads as having
/// declared them.
#[test]
fn two_companions_that_stage_as_one_directory_stop_the_run_rather_than_overwrite() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(&skill_path).unwrap();
    let skill_md = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";
    std::fs::write(skill_path.join("SKILL.md"), skill_md).unwrap();
    write_companion_skill(&skill_path.join("evals/a/other-skill"), "other-skill", "One of two.");
    write_companion_skill(
        &skill_path.join("evals/b/other-skill"),
        "other-skill",
        "The other of two.",
    );
    let case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "triggers-the-skill",
        "prompt": "do the thing",
        "expected_output": "done",
        "skill_disclosure": "unannounced",
        "companion_skills": ["evals/a/other-skill", "evals/b/other-skill"],
    }))
    .unwrap();
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        skill_md,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    let error = prepare_workspace(&request, Runner::ClaudeCode).expect_err("the collision stops the run");

    assert!(
        error.to_string().contains("evals/b/other-skill"),
        "the refusal has to name the companion that collided: {error}"
    );
}

/// The `old_skill` arm stages the older revision under that revision's own name, which
/// `--allow-skill-name-mismatch` lets differ from the current one. A companion wearing
/// the older name would be staged over it, and the arm would report on the companion
/// rather than on the revision it was drawn to measure.
#[test]
fn a_companion_wearing_the_old_revisions_name_is_refused_rather_than_staged_over_it() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(&skill_path).unwrap();
    let skill_md = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";
    std::fs::write(skill_path.join("SKILL.md"), skill_md).unwrap();
    let old_skill_path = temp.path().join("old-skill");
    let old_skill_md = "---\nname: renamed-skill\ndescription: The revision before the rename.\n---\n# Skill\n";
    std::fs::create_dir_all(&old_skill_path).unwrap();
    std::fs::write(old_skill_path.join("SKILL.md"), old_skill_md).unwrap();
    write_companion_skill(
        &skill_path.join("evals/companions/renamed-skill"),
        "renamed-skill",
        "Wears the name the skill answered to before the rename.",
    );
    let case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "triggers-the-skill",
        "prompt": "do the thing",
        "expected_output": "done",
        "skill_disclosure": "unannounced",
        "companion_skills": ["evals/companions/renamed-skill"],
    }))
    .unwrap();
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::OldSkill,
        skill_md,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        Some(old_skill_md),
        Some(old_skill_path.as_path()),
    );

    let error = prepare_workspace(&request, Runner::ClaudeCode).expect_err("the collision stops the run");

    assert!(
        error.to_string().contains("evals/companions/renamed-skill"),
        "the refusal has to name the companion: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("skills/renamed-skill/SKILL.md")).unwrap(),
        old_skill_md,
        "the revision the arm measures has to be the one still standing"
    );
}

/// A directory with no `SKILL.md` stages as a folder no run can route to, so a case
/// declaring one would read as offering a choice while offering nothing to choose.
#[test]
fn a_companion_that_is_not_a_skill_directory_stops_the_run() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals/companions/empty")).unwrap();
    let skill_md = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";
    std::fs::write(skill_path.join("SKILL.md"), skill_md).unwrap();
    let case: EvalCase = serde_json::from_value(serde_json::json!({
        "id": "triggers-the-skill",
        "prompt": "do the thing",
        "expected_output": "done",
        "skill_disclosure": "unannounced",
        "companion_skills": ["evals/companions/empty"],
    }))
    .unwrap();
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        skill_md,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    let error = prepare_workspace(&request, Runner::ClaudeCode).expect_err("an invalid companion stops the run");

    assert!(
        error.to_string().contains("evals/companions/empty"),
        "the refusal has to name the companion: {error}"
    );
}

/// The suite carries every case's expected output and its graders' literal patterns.
/// A run that can read it can be scored on text it copied, so neither staging mode may
/// put it in the workspace.
#[test]
fn staging_withholds_the_eval_suite_from_the_workspace() {
    for staging in [SkillStaging::Symlink, SkillStaging::Copy] {
        let temp = tempdir().unwrap();
        let skill_path = temp.path().join("skill");
        std::fs::create_dir_all(skill_path.join("evals/files")).unwrap();
        std::fs::write(
            skill_path.join("SKILL.md"),
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        )
        .unwrap();
        std::fs::write(skill_path.join("evals/files/input.txt"), "hello").unwrap();
        std::fs::write(
            skill_path.join("evals/evals.json"),
            r#"{"skill_name":"test-skill","evals":[{"id":"a","prompt":"p","expected_output":"the answer key"}]}"#,
        )
        .unwrap();
        std::fs::create_dir_all(skill_path.join("evals/a/fixtures")).unwrap();
        std::fs::write(skill_path.join("evals/a/fixtures/seed.txt"), "seed").unwrap();

        let workspace = temp.path().join("ws");
        let transcript = workspace.join("transcript.jsonl");
        let stderr = workspace.join("stderr.log");
        let case = make_case(vec!["evals/files/input.txt".to_string()]);
        let mut request = test_request(
            &case,
            ScenarioKind::WithSkill,
            "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
            &skill_path,
            &workspace,
            &transcript,
            &stderr,
            None,
            None,
        );
        request.skill_staging = staging;

        prepare_workspace(&request, Runner::ClaudeCode).unwrap();

        let staged_suite_dir = workspace.join(".skill/evals");
        assert!(
            !staged_suite_dir.exists(),
            "{staging:?}: the eval suite directory must not be staged"
        );
        assert!(
            std::fs::read_to_string(workspace.join(".skill/evals/evals.json")).is_err(),
            "{staging:?}: the manifest must be unreadable through the staged skill"
        );

        // The skill itself, and the fixture the case declared, are still there.
        assert!(
            workspace.join(".skill/SKILL.md").is_file(),
            "{staging:?}: the skill under test must still be staged"
        );
        assert!(
            workspace.join("evals/files/input.txt").is_file(),
            "{staging:?}: a declared fixture is staged separately and must survive"
        );
    }
}

fn staged_symlinks(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.symlink_metadata().unwrap().file_type().is_symlink() {
            found.push(path);
        } else if path.is_dir() {
            found.extend(staged_symlinks(&path));
        }
    }
    found
}

/// Keeping the suite out of the staged directory keeps it out of a listing, not out
/// of the run: a staged symlink names the live skill directory, and the suite sits
/// next to it, so following one entry is enough to read every expected output. The
/// default mode has to leave nothing to follow.
#[test]
fn the_default_staging_mode_leaves_no_path_out_of_the_workspace() {
    assert_eq!(
        SkillStaging::default(),
        SkillStaging::Copy,
        "the mode a run gets without asking must be the hermetic one"
    );

    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("reference")).unwrap();
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("reference/guide.md"), "guide").unwrap();
    std::fs::write(
        skill_path.join("evals/evals.json"),
        r#"{"skill_name":"test-skill","evals":[{"id":"a","prompt":"p","expected_output":"the answer key"}]}"#,
    )
    .unwrap();

    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let case = make_case(vec![]);
    let mut request = test_request(
        &case,
        ScenarioKind::WithSkill,
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );
    request.skill_staging = SkillStaging::default();

    prepare_workspace(&request, Runner::ClaudeCode).unwrap();

    let staged = workspace.join(".skill");
    assert!(
        staged_symlinks(&staged).is_empty(),
        "the default mode must stage nothing that names a path outside the workspace"
    );
    assert!(
        !staged.canonicalize().unwrap().join("../evals/evals.json").exists(),
        "walking out of the staged directory must not reach the suite"
    );
}

/// Why the cheap mode is the one you have to ask for.
#[test]
fn symlink_staging_tells_the_run_where_the_suite_lives() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(
        skill_path.join("evals/evals.json"),
        r#"{"skill_name":"test-skill","evals":[{"id":"a","prompt":"p","expected_output":"the answer key"}]}"#,
    )
    .unwrap();

    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let case = make_case(vec![]);
    let mut request = test_request(
        &case,
        ScenarioKind::WithSkill,
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );
    request.skill_staging = SkillStaging::Symlink;

    prepare_workspace(&request, Runner::ClaudeCode).unwrap();

    let staged_skill_md = workspace.join(".skill/SKILL.md");
    let target = std::fs::read_link(&staged_skill_md).unwrap();
    assert!(
            target.parent().unwrap().join("evals/evals.json").exists(),
            "symlink staging is documented as disclosing the skill's real location; if that stopped being true, make it the default"
        );
}

/// A nested `evals/` belongs to the skill's own content, so only the top level is filtered.
#[test]
fn staging_withholds_only_the_top_level_eval_suite() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::create_dir_all(skill_path.join("reference/evals")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/evals.json"), "{}").unwrap();
    std::fs::write(skill_path.join("reference/evals/guide.md"), "keep me").unwrap();

    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let case = make_case(vec![]);
    let mut request = test_request(
        &case,
        ScenarioKind::WithSkill,
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );
    request.skill_staging = SkillStaging::Copy;

    prepare_workspace(&request, Runner::ClaudeCode).unwrap();

    assert!(!workspace.join(".skill/evals").exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join(".skill/reference/evals/guide.md")).unwrap(),
        "keep me"
    );
}

fn skill_with_a_link(link_name: &str, target: &Path) -> (tempfile::TempDir, PathBuf) {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/evals.json"), "expected_output").unwrap();
    std::os::unix::fs::symlink(target, skill_path.join(link_name)).unwrap();
    (temp, skill_path)
}

fn stage_by_copy(skill_path: &Path, workspace: &Path) {
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let case = make_case(vec![]);
    let mut request = test_request(
        &case,
        ScenarioKind::WithSkill,
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        skill_path,
        workspace,
        &transcript,
        &stderr,
        None,
        None,
    );
    request.skill_staging = SkillStaging::Copy;
    prepare_workspace(&request, Runner::ClaudeCode).unwrap();
}

#[test]
fn copy_staging_does_not_follow_a_link_to_the_withheld_suite() {
    let (temp, skill_path) = skill_with_a_link("notes", Path::new("evals"));
    let workspace = temp.path().join("ws");

    stage_by_copy(&skill_path, &workspace);

    assert!(
        !workspace.join(".skill/notes").exists(),
        "copying dereferences links, so a link naming the suite would deliver the answer key"
    );
}

/// A skill that holds a fast suite and a full one is the reason `--eval-dir` exists.
/// Withholding only the directory this pass named would hand the run the other suite,
/// whose cases routinely overlap, so it could be scored on text it read.
#[test]
fn staging_withholds_a_sibling_suite_the_pass_did_not_name() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::create_dir_all(skill_path.join("evals-full")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/evals.json"), "{}").unwrap();
    std::fs::write(skill_path.join("evals-full/evals.json"), "expected_output").unwrap();

    let workspace = temp.path().join("ws");
    stage_by_copy(&skill_path, &workspace);

    assert!(
        !workspace.join(".skill/evals-full").exists(),
        "a suite the pass did not run is still an answer key"
    );
}

/// The case-directory form carries no manifest, so it is recognised by the shape
/// `resolve_eval_suite` accepts rather than by a file name.
#[test]
fn staging_withholds_a_sibling_suite_written_as_case_directories() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::create_dir_all(skill_path.join("evals-full/first-case")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/evals.json"), "{}").unwrap();
    std::fs::write(skill_path.join("evals-full/first-case/prompt.md"), "expected_output").unwrap();

    let workspace = temp.path().join("ws");
    stage_by_copy(&skill_path, &workspace);

    assert!(
        !workspace.join(".skill/evals-full").exists(),
        "a suite without a manifest is still a suite"
    );
}

/// Copying dereferences links, so the sibling suite has to be unreachable by name as
/// well as unreachable as a directory entry.
#[test]
fn copy_staging_does_not_follow_a_link_to_a_sibling_suite() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::create_dir_all(skill_path.join("evals-full")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/evals.json"), "{}").unwrap();
    std::fs::write(skill_path.join("evals-full/evals.json"), "expected_output").unwrap();
    std::os::unix::fs::symlink(Path::new("evals-full"), skill_path.join("notes")).unwrap();

    let workspace = temp.path().join("ws");
    stage_by_copy(&skill_path, &workspace);

    assert!(
        !workspace.join(".skill/notes").exists(),
        "a link naming the sibling suite would deliver it under another name"
    );
}

#[test]
fn copy_staging_does_not_follow_a_link_out_of_the_skill() {
    let outside = tempdir().unwrap();
    std::fs::write(outside.path().join("operator-secret"), "secret").unwrap();
    let (temp, skill_path) = skill_with_a_link("elsewhere", outside.path());
    let workspace = temp.path().join("ws");

    stage_by_copy(&skill_path, &workspace);

    assert!(
        !workspace.join(".skill/elsewhere").exists(),
        "every path a copied skill offers has to stay inside the workspace"
    );
}

#[test]
fn staging_withholds_the_history_the_eval_suite_is_recorded_in() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join("evals")).unwrap();
    std::fs::create_dir_all(skill_path.join(".git/objects")).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    std::fs::write(skill_path.join("evals/evals.json"), "{}").unwrap();
    std::fs::write(skill_path.join(".git/objects/answer-key"), "expected_output").unwrap();

    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let case = make_case(vec![]);
    let mut request = test_request(
        &case,
        ScenarioKind::WithSkill,
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );
    request.skill_staging = SkillStaging::Copy;

    prepare_workspace(&request, Runner::ClaudeCode).unwrap();

    assert!(
        !workspace.join(".skill/.git").exists(),
        "a skill that is its own checkout keeps every revision of the withheld suite in its history"
    );
}

#[test]
fn prepare_without_skill_returns_raw_prompt() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(&skill_path).unwrap();
    std::fs::write(
        skill_path.join("SKILL.md"),
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
    )
    .unwrap();
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");

    let case = make_case(vec![]);
    let request = test_request(
        &case,
        ScenarioKind::WithoutSkill,
        "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n",
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
    assert!(prepared.prompt.starts_with("do the thing"));
    assert!(prepared.prompt.contains("outputs/"));
    assert!(!prepared.prompt.contains("Skill available at:"));
    assert!(!prepared.prompt.contains("Skill summary:"));
    assert!(!workspace.join(".skill").exists());
}

#[test]
fn prepare_old_skill_symlinks_old_skill_not_current_and_uses_old_prompt() {
    let temp = tempdir().unwrap();
    let current_skill = temp.path().join("current-skill");
    let old_skill = temp.path().join("old-skill");
    std::fs::create_dir_all(current_skill.join("evals/files")).unwrap();
    std::fs::create_dir_all(&old_skill).unwrap();
    std::fs::write(
        current_skill.join("SKILL.md"),
        "---\nname: current-skill\ndescription: Current skill\n---\n# Current\n",
    )
    .unwrap();
    std::fs::write(
        old_skill.join("SKILL.md"),
        "---\nname: old-skill\ndescription: Old skill\n---\n# Old\n",
    )
    .unwrap();
    std::fs::write(current_skill.join("evals/files/input.txt"), "from-current").unwrap();

    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let case = make_case(vec!["evals/files/input.txt".to_string()]);
    let request = test_request(
        &case,
        ScenarioKind::OldSkill,
        "---\nname: current-skill\ndescription: Current skill\n---\n# Current\n",
        &current_skill,
        &workspace,
        &transcript,
        &stderr,
        Some("---\nname: old-skill\ndescription: Old skill\n---\n# Old\n"),
        Some(&old_skill),
    );

    let prepared = prepare_workspace(&request, Runner::ClaudeCode).unwrap();
    assert!(prepared.prompt.contains("Skill available at: .old-skill/"));
    assert!(prepared.prompt.contains("name: old-skill"));
    assert!(!prepared.prompt.contains("name: current-skill"));
    assert!(!prepared.prompt.contains("Skill available at: .skill/"));
    assert!(!prepared.prompt.contains("# Old\n"));
    assert!(!prepared.prompt.contains("# Current"));

    let link = workspace.join(".old-skill");
    assert!(link.is_dir());
    let target = std::fs::read_link(link.join("SKILL.md")).unwrap();
    assert_eq!(target, std::fs::canonicalize(old_skill.join("SKILL.md")).unwrap());
    assert_eq!(
        std::fs::read_to_string(link.join("SKILL.md")).unwrap(),
        "---\nname: old-skill\ndescription: Old skill\n---\n# Old\n"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("evals/files/input.txt")).unwrap(),
        "from-current"
    );
}

#[test]
fn old_skill_tampering_detection_is_scoped_to_old_skill_directory() {
    let temp = tempdir().unwrap();
    let current_skill = temp.path().join("current-skill");
    let old_skill = temp.path().join("old-skill");
    std::fs::create_dir_all(&current_skill).unwrap();
    std::fs::create_dir_all(&old_skill).unwrap();
    std::fs::write(current_skill.join("SKILL.md"), "current-stable").unwrap();
    std::fs::write(old_skill.join("SKILL.md"), "old-stable").unwrap();

    let old_before = compute_skill_digest(&old_skill).unwrap();
    std::fs::write(current_skill.join("SKILL.md"), "current-tampered").unwrap();
    let old_after_current_tampered = compute_skill_digest(&old_skill).unwrap();
    assert!(detect_tampering(&old_before, &old_after_current_tampered).is_empty());

    std::fs::write(old_skill.join("SKILL.md"), "old-tampered").unwrap();
    let old_after_old_tampered = compute_skill_digest(&old_skill).unwrap();
    assert_eq!(detect_tampering(&old_before, &old_after_old_tampered), vec!["SKILL.md"]);
}

#[test]
fn prepare_old_skill_requires_old_skill_fields() {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(&skill_path).unwrap();
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");

    let case = make_case(vec![]);
    let request = test_request(
        &case,
        ScenarioKind::OldSkill,
        "",
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    let err = prepare_workspace(&request, Runner::ClaudeCode).unwrap_err();
    assert!(matches!(err, RunnerError::InvalidOutput { .. }));
}

#[test]
fn detect_tampering_flags_changes_additions_and_removals() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "original").unwrap();
    std::fs::write(skill.join("notes.md"), "keep").unwrap();

    let before = compute_skill_digest(&skill).unwrap();
    assert_eq!(before.len(), 2);

    std::fs::write(skill.join("SKILL.md"), "tampered").unwrap();
    std::fs::write(skill.join("added.md"), "new").unwrap();
    std::fs::remove_file(skill.join("notes.md")).unwrap();

    let after = compute_skill_digest(&skill).unwrap();
    let changed = detect_tampering(&before, &after);
    assert_eq!(changed, vec!["SKILL.md", "added.md", "notes.md"]);
}

#[test]
fn detect_tampering_returns_empty_when_unchanged() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "stable").unwrap();
    let before = compute_skill_digest(&skill).unwrap();
    let after = compute_skill_digest(&skill).unwrap();
    assert!(detect_tampering(&before, &after).is_empty());
}

#[test]
fn a_persisted_transcript_holds_no_bearer_aws_github_or_jwt_token() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("transcript.jsonl");
    let github = "ghp_123456789012345678901234567890123456";
    let jwt =
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
    let raw = format!("Authorization: Bearer abcdefghijklmnop\nkeys AKIAIOSFODNN7EXAMPLE and {github}\ntoken={jwt}\n");
    write_transcript(&path, &redact_transcript_bytes(raw.as_bytes())).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("Bearer abcdefghijklmnop"));
    assert!(!written.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(!written.contains(github));
    assert!(!written.contains(jwt));
    assert!(written.contains("<redacted>"));
}

#[test]
fn write_stderr_redacts_secrets() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("stderr.log");
    write_stderr(&path, b"stderr Bearer abcdefghijklmnop\n").unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("Bearer abcdefghijklmnop"));
    assert!(written.contains("<redacted>"));
}

/// A run is handed its prompt on the command line, never from the operator's
/// keyboard. It also leads a background process group, where reading the controlling
/// terminal stops the reader rather than failing it, so a harness that tried would
/// hang the run instead of ending it.
#[test]
fn a_run_cannot_read_the_terminal_that_started_it() {
    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg("if [ -t 0 ]; then echo tty; else echo notty; fi; cat");

    let captured = capture_subprocess(&mut command, Some(Duration::from_secs(10))).unwrap();

    assert!(
        !captured.timed_out,
        "a read of the run's stdin has to end the read, not the run"
    );
    assert_eq!(String::from_utf8_lossy(&captured.stdout).trim(), "notty");
}

#[test]
fn a_timeout_takes_down_the_tools_the_harness_spawned() {
    let temp = tempdir().unwrap();
    let pid_file = temp.path().join("grandchild.pid");

    let mut command = Command::new("bash");
    command
        .arg("-c")
        .arg(format!("sleep 600 & echo $! > {}; sleep 600", pid_file.display()));

    let captured = capture_subprocess(&mut command, Some(Duration::from_secs(1))).unwrap();
    assert!(
        captured.timed_out,
        "the run had to be cut short for this to mean anything"
    );

    let grandchild: i32 = std::fs::read_to_string(&pid_file)
        .expect("the harness recorded the tool it spawned")
        .trim()
        .parse()
        .unwrap();
    assert!(
        !group::process_is_alive(grandchild),
        "a tool the harness spawned outlived the run it belonged to"
    );
}

fn wait_until_gone(pid: i32) -> bool {
    for _ in 0..40 {
        if !group::process_is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// A harness can finish its turn and still leave a tool running. That tool writes
/// into a workspace that is about to be scored, so the run is not over until it is.
#[test]
fn a_harness_that_exits_does_not_leave_its_tools_running() {
    let temp = tempdir().unwrap();
    let pid_file = temp.path().join("leftover.pid");

    let mut command = Command::new("bash");
    command.arg("-c").arg(format!(
        "sleep 600 >/dev/null 2>&1 & echo $! > {}; exit 0",
        pid_file.display()
    ));

    let captured = capture_subprocess(&mut command, Some(Duration::from_secs(30))).unwrap();
    assert!(
        !captured.timed_out,
        "the harness had to exit on its own for this to mean anything"
    );

    let leftover: i32 = std::fs::read_to_string(&pid_file)
        .expect("the harness recorded the tool it spawned")
        .trim()
        .parse()
        .unwrap();
    assert!(
        wait_until_gone(leftover),
        "a tool the harness spawned outlived the run it belonged to"
    );
}

#[test]
fn write_runner_invocation_metadata_redacts_command_args_and_env() {
    if super::run_with_operator_environment(
        "agentskills::runner::tests::workspace_tests::write_runner_invocation_metadata_redacts_command_args_and_env",
        &[
            ("TRG_REDACT_TEST_OPENAI_API_KEY", "sk-secret"),
            ("TRG_REDACT_TEST_SAFE", "visible"),
        ],
    ) {
        return;
    }
    let temp = tempdir().unwrap();
    let run_dir = temp.path().join("run-001");
    let github = "ghp_123456789012345678901234567890123456";
    let secret_key = "TRG_REDACT_TEST_OPENAI_API_KEY";

    write_runner_invocation_metadata(
        &run_dir,
        redact_command_args("codex", &["exec", "--api-key", github, "--model", "gpt-4"]),
        RecordedEnvironment {
            vars: redact_env(),
            config_home: None,
        },
    )
    .unwrap();

    let cmd = std::fs::read_to_string(run_dir.join("cmd")).unwrap();
    assert!(!cmd.contains(github));
    assert!(cmd.contains("--api-key"));
    assert!(cmd.contains("<redacted>"));

    let recorded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("env.json")).unwrap()).unwrap();
    let env = recorded["vars"].as_object().unwrap();
    assert!(env.contains_key("TRG_REDACT_TEST_SAFE"));
    assert!(!env.contains_key(secret_key));
    assert!(!env
        .keys()
        .any(|key| is_secret_env_key(key) && key.starts_with("TRG_REDACT_TEST_")));
}

#[test]
fn env_json_names_the_config_home_for_every_harness() {
    for (runner, expected_suffix) in [
        (Runner::ClaudeCode, ".claude"),
        (Runner::Codex, ".codex"),
        (Runner::CursorAgent, ".cursor"),
    ] {
        let temp = tempdir().unwrap();
        let run_dir = temp.path().join("run-001");
        let host = BTreeMap::from([("HOME".to_string(), "/host/home".to_string())]);

        let environment =
            RunEnvironment::prepare_from(runner, &run_dir, EnvironmentPolicy::Scrubbed, None, &host).unwrap();

        write_runner_invocation_metadata(
            &run_dir,
            redact_command_args(runner.program_name(), &[]),
            environment.record(),
        )
        .unwrap();

        let recorded: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join("env.json")).unwrap()).unwrap();
        let config_home = &recorded["config_home"];
        assert_eq!(
            config_home["origin"], "host",
            "{runner:?} must record that it saw the host's config home"
        );
        assert!(
            config_home["path"].as_str().unwrap().ends_with(expected_suffix),
            "{runner:?} env.json did not name its config home: {config_home}"
        );
    }
}

#[test]
fn env_json_names_the_run_config_home_when_isolated() {
    for runner in [Runner::ClaudeCode, Runner::Codex, Runner::CursorAgent] {
        let temp = tempdir().unwrap();
        let run_dir = temp.path().join("run-001");
        let host = BTreeMap::from([
            ("HOME".to_string(), "/host/home".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ]);

        let environment =
            RunEnvironment::prepare_from(runner, &run_dir, EnvironmentPolicy::Isolated, None, &host).unwrap();

        write_runner_invocation_metadata(
            &run_dir,
            redact_command_args(runner.program_name(), &[]),
            environment.record(),
        )
        .unwrap();

        let recorded: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run_dir.join("env.json")).unwrap()).unwrap();
        let config_home = &recorded["config_home"];
        assert_eq!(
            config_home["origin"], "run",
            "{runner:?} must record that its config home was made for this run"
        );
        assert!(
            config_home["path"]
                .as_str()
                .unwrap()
                .starts_with(run_dir.to_str().unwrap()),
            "{runner:?} recorded the host's config home instead of the run's: {config_home}"
        );
    }
}

const SKILL_MD: &str = "---\nname: test-skill\ndescription: Test skill\n---\n# Skill\n";

fn make_case_with_files(files: serde_json::Value) -> EvalCase {
    serde_json::from_value(serde_json::json!({
        "id": "case-1",
        "prompt": "do the thing",
        "expected_output": "done",
        "files": files,
    }))
    .unwrap()
}

fn skill_with_fixture(relative: &str, contents: &str) -> (tempfile::TempDir, PathBuf) {
    let temp = tempdir().unwrap();
    let skill_path = temp.path().join("skill");
    std::fs::create_dir_all(skill_path.join(Path::new(relative).parent().unwrap())).unwrap();
    std::fs::write(skill_path.join(relative), contents).unwrap();
    std::fs::write(skill_path.join("SKILL.md"), SKILL_MD).unwrap();
    (temp, skill_path)
}

#[cfg(unix)]
#[test]
fn a_read_only_fixture_is_staged_without_write_bits() {
    let relative = "evals/files/input.csv";
    let (temp, skill_path) = skill_with_fixture(relative, "a,b\n1,2\n");
    let case = make_case_with_files(serde_json::json!([{ "path": relative, "mode": "read_only" }]));
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        SKILL_MD,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).expect("workspace prepares");

    let staged = workspace.join(relative);
    let mode = std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o444, "the staged fixture must carry no write bits");
}

/// `std::fs::copy` preserves the source's mode, so a fixture committed executable stages
/// executable. Locking it for read-only must remove permission to write it without also
/// taking away permission to run it, or the agent under test is handed a script it was
/// given specifically so it could run it.
#[cfg(unix)]
#[test]
fn a_read_only_fixture_keeps_its_execute_bit_when_locked() {
    let relative = "evals/files/run.sh";
    let (temp, skill_path) = skill_with_fixture(relative, "#!/bin/sh\necho hi\n");
    std::fs::set_permissions(skill_path.join(relative), std::fs::Permissions::from_mode(0o755)).unwrap();
    let case = make_case_with_files(serde_json::json!([{ "path": relative, "mode": "read_only" }]));
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        SKILL_MD,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).expect("workspace prepares");

    let staged = workspace.join(relative);
    let mode = std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode & 0o222, 0, "the staged fixture must carry no write bits");
    assert_eq!(
        mode & 0o111,
        0o111,
        "locking must not take away the execute bits the fixture was staged with"
    );
}

/// A retry resets the workspace before staging again. If clearing a read-only fixture's
/// write bits also locked its directory, that reset would fail instead of clearing the
/// stale fixture, which is a worse defect than the tampering this feature detects.
#[test]
fn reset_workspace_clears_a_stale_read_only_fixture_between_attempts() {
    let relative = "evals/files/input.csv";
    let (temp, skill_path) = skill_with_fixture(relative, "a,b\n1,2\n");
    let case = make_case_with_files(serde_json::json!([{ "path": relative, "mode": "read_only" }]));
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        SKILL_MD,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).expect("first attempt prepares");
    assert!(workspace.join(relative).exists());

    prepare_workspace(&request, Runner::ClaudeCode)
        .expect("a stale read-only fixture must not block the next attempt's reset");
    assert!(
        workspace.join(relative).exists(),
        "the fixture is staged fresh on the next attempt"
    );
}

#[test]
fn verify_read_only_fixtures_flags_a_modified_fixture() {
    let relative = "evals/files/input.csv";
    let (temp, skill_path) = skill_with_fixture(relative, "a,b\n1,2\n");
    let case = make_case_with_files(serde_json::json!([{ "path": relative, "mode": "read_only" }]));
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        SKILL_MD,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).expect("workspace prepares");

    let staged = workspace.join(relative);
    #[cfg(unix)]
    {
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::write(&staged, "tampered").unwrap();

    assert_eq!(verify_read_only_fixtures(&request), vec![relative.to_string()]);
}

#[test]
fn verify_read_only_fixtures_flags_a_deleted_fixture() {
    let relative = "evals/files/input.csv";
    let (temp, skill_path) = skill_with_fixture(relative, "a,b\n1,2\n");
    let case = make_case_with_files(serde_json::json!([{ "path": relative, "mode": "read_only" }]));
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        SKILL_MD,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).expect("workspace prepares");
    std::fs::remove_file(workspace.join(relative)).unwrap();

    assert_eq!(verify_read_only_fixtures(&request), vec![relative.to_string()]);
}

#[test]
fn verify_read_only_fixtures_is_silent_when_nothing_changed() {
    let relative = "evals/files/input.csv";
    let (temp, skill_path) = skill_with_fixture(relative, "a,b\n1,2\n");
    let case = make_case_with_files(serde_json::json!([{ "path": relative, "mode": "read_only" }]));
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        SKILL_MD,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).expect("workspace prepares");

    assert!(verify_read_only_fixtures(&request).is_empty());
}

#[test]
fn verify_read_only_fixtures_ignores_writable_fixtures() {
    let relative = "evals/files/input.csv";
    let (temp, skill_path) = skill_with_fixture(relative, "a,b\n1,2\n");
    let case = make_case_with_files(serde_json::json!([relative]));
    let workspace = temp.path().join("ws");
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = test_request(
        &case,
        ScenarioKind::WithSkill,
        SKILL_MD,
        &skill_path,
        &workspace,
        &transcript,
        &stderr,
        None,
        None,
    );

    prepare_workspace(&request, Runner::ClaudeCode).expect("workspace prepares");
    std::fs::write(workspace.join(relative), "changed freely").unwrap();

    assert!(verify_read_only_fixtures(&request).is_empty());
}
