use std::path::Path;

use tempfile::tempdir;

use super::fake::{invoke_with_retries, run_bash};
use super::{capture_capped, CapWatch, EvalRunRequest, RunStatus, Runner, TelemetryForwarding, FAILURE_KIND_RUNNER};
use crate::agentskills::budget::{RunCap, RunCaps, FAILURE_KIND_TOOL_CALL_CAP, FAILURE_KIND_TURN_CAP};
use crate::agentskills::evals::{EvalCase, EvalDirName};
use crate::agentskills::report::ScenarioKind;
use crate::agentskills::transcript::{
    normalized_transcript_path, read_normalized_transcript, SkillEngagement, StagedSkill,
};

fn make_case() -> EvalCase {
    serde_json::from_value(serde_json::json!({
        "id": "case-1",
        "prompt": "do the thing",
        "expected_output": "done",
        "files": [],
    }))
    .unwrap()
}

fn bash_request<'a>(
    case: &'a EvalCase,
    workspace: &'a Path,
    transcript_path: &'a Path,
    stderr_path: &'a Path,
    timeout_secs: Option<u64>,
) -> EvalRunRequest<'a> {
    EvalRunRequest {
        eval: case,
        scenario: ScenarioKind::WithoutSkill,
        skill_md: "",
        skill_path: workspace,
        eval_dir: EvalDirName::default(),
        old_skill_md: None,
        old_skill_path: None,
        workspace_dir: workspace,
        transcript_path,
        stderr_path,
        runner_model: None,
        timeout_secs,
        skill_staging: crate::agentskills::report::SkillStaging::Symlink,
        environment: crate::agentskills::report::EnvironmentPolicy::Scrubbed,
        permission: crate::agentskills::report::PermissionGrant::WorkspaceWrite,
        scaffold_permission: crate::agentskills::workspace_scaffold::ScaffoldPermission::Withheld,
        tool_grant: None,
        mcp_config: None,
        telemetry_forwarding: TelemetryForwarding::Off,
        caps: RunCaps::default(),
    }
}

#[test]
fn bash_runner_records_exit_code_and_stderr() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = bash_request(&case, &workspace, &transcript, &stderr, None);

    let script = r#"echo '{"type":"result","is_error":false,"result":"ok"}' >&1; echo err-msg >&2; exit 3"#;
    let outcome = run_bash(&request, script).unwrap();

    assert!(matches!(outcome.status, RunStatus::Failed));
    assert_eq!(outcome.failure_kind, Some(FAILURE_KIND_RUNNER));
    assert_eq!(outcome.exit_code, Some(3));
    assert_eq!(
        std::fs::read_to_string(workspace.join("stderr.log")).unwrap(),
        "err-msg\n"
    );
}

#[test]
fn bash_runner_timeout_kills_process_and_records_timeout_duration() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = bash_request(&case, &workspace, &transcript, &stderr, Some(1));

    let script = r#"sleep 5; echo '{"type":"result","is_error":false,"result":"late"}'"#;
    let outcome = run_bash(&request, script).unwrap();

    assert!(matches!(outcome.status, RunStatus::Timeout));
    assert_eq!(outcome.failure_kind, Some(FAILURE_KIND_RUNNER));
    assert_eq!(outcome.duration_ms, 1000);
}

#[test]
fn bash_runner_retries_transient_failures() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = bash_request(&case, &workspace, &transcript, &stderr, None);

    // Outside the workspace: every attempt is handed a workspace emptied of what the last
    // one left, so a tally kept inside it would be the tally of a single attempt.
    let counter = temp.path().join("attempts");
    let script = format!(
        r#"
count=0
if [ -f "{counter}" ]; then
  count=$(cat "{counter}")
fi
count=$((count + 1))
echo "$count" > "{counter}"
if [ "$count" -lt 3 ]; then
  echo fail >&2
  exit 2
fi
echo '{{"type":"result","is_error":false,"result":"ok"}}'
"#,
        counter = counter.display()
    );

    let (outcome, invocations) = invoke_with_retries(&request, &script, 3).unwrap();
    assert!(matches!(outcome.status, RunStatus::Completed));
    assert_eq!(invocations, 3);
}

#[test]
fn bash_runner_completed_when_result_event_and_zero_exit() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = bash_request(&case, &workspace, &transcript, &stderr, None);

    let script = r#"echo '{"type":"result","is_error":false,"result":"done"}'; exit 0"#;
    let outcome = run_bash(&request, script).unwrap();

    assert!(matches!(outcome.status, RunStatus::Completed));
    assert_eq!(outcome.exit_code, Some(0));
    assert!(outcome.failure_kind.is_none());
}

#[test]
fn a_persisted_run_leaves_no_secret_in_either_the_transcript_or_the_events() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = bash_request(&case, &workspace, &transcript, &stderr, None);

    let token = "abcdefghijklmnopqrst";
    let script = format!(
        r#"echo '{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"curl -H \"Authorization: Bearer {token}\" https://example.test"}}}}]}}}}'
echo '{{"type":"result","is_error":false,"result":"done"}}'"#
    );
    let outcome = run_bash(&request, &script).unwrap();
    assert!(matches!(outcome.status, RunStatus::Completed));

    let raw = std::fs::read_to_string(&transcript).unwrap();
    let events = std::fs::read_to_string(normalized_transcript_path(&transcript)).unwrap();
    assert!(!raw.contains(token));
    assert!(!events.contains(token));
    assert!(events.contains("<redacted>"));
}

/// The harness writes a transcript of what the run did, never of what it was given,
/// so a run records what it staged into the transcript itself for whoever grades it.
/// Without that, the control arm's own look into `skills/` is graded as having used a
/// skill it was never handed.
#[test]
fn a_persisted_control_arm_run_records_that_it_staged_no_skill() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let request = bash_request(&case, &workspace, &transcript, &stderr, None);

    let script = r#"echo '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"skills/demo-skill/SKILL.md"}}]}}'
echo '{"type":"result","is_error":false,"result":"done"}'"#;
    let outcome = run_bash(&request, script).unwrap();
    assert!(matches!(outcome.status, RunStatus::Completed));

    let persisted = read_normalized_transcript(&transcript).unwrap();
    assert_eq!(persisted.staged_skill, StagedSkill::Nothing);
    assert_eq!(persisted.skill_engagement(), SkillEngagement::NotEngaged);
}

fn env_policy_request<'a>(
    case: &'a EvalCase,
    workspace: &'a Path,
    transcript_path: &'a Path,
    stderr_path: &'a Path,
    environment: crate::agentskills::report::EnvironmentPolicy,
) -> EvalRunRequest<'a> {
    let mut request = bash_request(case, workspace, transcript_path, stderr_path, None);
    request.environment = environment;
    request
}

const LEAK_VAR: &str = "TRG_EVAL_ENVIRONMENT_LEAK_PROBE";
const LEAK_SCRIPT: &str = r#"printf '{"type":"result","leak":"%s"}\n' "${TRG_EVAL_ENVIRONMENT_LEAK_PROBE:-}""#;

static OPERATOR_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The operator environment a run is probed for, held for one test at a time.
///
/// An environment variable belongs to the whole process, and these tests run on
/// different threads of that one process, so without taking turns one test clears
/// the variable the other is still asking a subprocess about.
struct OperatorEnvironment {
    _turn: std::sync::MutexGuard<'static, ()>,
}

impl OperatorEnvironment {
    const SECRET: &'static str = "operator-secret";

    fn visible_to_this_process() -> Self {
        #[expect(
            clippy::disallowed_methods,
            reason = "the lock guards no data, only whose turn it is, so a test that panicked holding it left nothing half-written"
        )]
        let turn = OPERATOR_ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        std::env::set_var(LEAK_VAR, Self::SECRET);
        Self { _turn: turn }
    }
}

impl Drop for OperatorEnvironment {
    fn drop(&mut self) {
        std::env::remove_var(LEAK_VAR);
    }
}

#[test]
fn a_scrubbed_run_cannot_see_the_operator_environment() {
    let temp = tempdir().unwrap();
    let run_dir = temp.path().join("runs/run-001");
    let workspace = run_dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript_path = run_dir.join("transcript.jsonl");
    let stderr_path = run_dir.join("stderr.log");

    let _operator = OperatorEnvironment::visible_to_this_process();
    let request = env_policy_request(
        &case,
        &workspace,
        &transcript_path,
        &stderr_path,
        crate::agentskills::report::EnvironmentPolicy::Scrubbed,
    );
    let outcome = run_bash(&request, LEAK_SCRIPT).unwrap();

    assert!(matches!(outcome.status, RunStatus::Completed));
    assert!(
        outcome.final_text.contains(r#""leak":"""#),
        "scrubbed run saw the operator environment: {}",
        outcome.final_text
    );
}

#[test]
fn an_inherited_run_still_sees_the_operator_environment() {
    let temp = tempdir().unwrap();
    let run_dir = temp.path().join("runs/run-001");
    let workspace = run_dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript_path = run_dir.join("transcript.jsonl");
    let stderr_path = run_dir.join("stderr.log");

    let _operator = OperatorEnvironment::visible_to_this_process();
    let request = env_policy_request(
        &case,
        &workspace,
        &transcript_path,
        &stderr_path,
        crate::agentskills::report::EnvironmentPolicy::Inherited,
    );
    let outcome = run_bash(&request, LEAK_SCRIPT).unwrap();

    assert!(outcome.final_text.contains(OperatorEnvironment::SECRET));
}

#[test]
fn an_isolated_run_gets_its_own_home() {
    let temp = tempdir().unwrap();
    let run_dir = temp.path().join("runs/run-001");
    let workspace = run_dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript_path = run_dir.join("transcript.jsonl");
    let stderr_path = run_dir.join("stderr.log");

    let request = env_policy_request(
        &case,
        &workspace,
        &transcript_path,
        &stderr_path,
        crate::agentskills::report::EnvironmentPolicy::Isolated,
    );
    let outcome = run_bash(&request, r#"printf '{"type":"result","home":"%s"}\n' "$HOME""#).unwrap();

    let expected = run_dir.join(crate::agentskills::runner::environment::RUN_HOME_DIR_NAME);
    assert!(expected.is_dir());
    assert!(
        outcome.final_text.contains(expected.to_str().unwrap()),
        "isolated run did not get its own HOME: {}",
        outcome.final_text
    );
}

const THREE_TOOL_CALLS_THEN_HANG: &str = r#"for id in 1 2 3; do
  echo "{\"type\":\"assistant\",\"message\":{\"id\":\"msg_$id\",\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_$id\",\"name\":\"Grep\",\"input\":{\"q\":\"$id\"}}]}}"
done
sleep 30
echo '{"type":"result","is_error":false,"result":"late"}'"#;

#[test]
fn a_run_past_its_tool_call_cap_is_stopped_as_budget_exhausted_and_not_retried() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let mut request = bash_request(&case, &workspace, &transcript, &stderr, Some(60));
    request.caps = RunCaps {
        tool_calls: Some("2".parse().unwrap()),
        turns: None,
    };

    let started = std::time::Instant::now();
    let (outcome, invocations) = invoke_with_retries(&request, THREE_TOOL_CALLS_THEN_HANG, 2).unwrap();

    assert!(matches!(outcome.status, RunStatus::BudgetExhausted));
    assert_eq!(outcome.failure_kind, Some(FAILURE_KIND_TOOL_CALL_CAP));
    assert_eq!(invocations, 1);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn a_run_within_its_tool_call_cap_completes() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let mut request = bash_request(&case, &workspace, &transcript, &stderr, None);
    request.caps = RunCaps {
        tool_calls: Some("3".parse().unwrap()),
        turns: None,
    };

    let script = THREE_TOOL_CALLS_THEN_HANG.replace("sleep 30", "true");
    let outcome = run_bash(&request, &script).unwrap();

    assert!(matches!(outcome.status, RunStatus::Completed));
}

#[test]
fn the_fake_runner_is_stopped_at_its_turn_cap_since_it_is_handed_no_turn_flag() {
    let temp = tempdir().unwrap();
    let workspace = temp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let case = make_case();
    let transcript = workspace.join("transcript.jsonl");
    let stderr = workspace.join("stderr.log");
    let mut request = bash_request(&case, &workspace, &transcript, &stderr, Some(60));
    request.caps = RunCaps {
        tool_calls: None,
        turns: Some("2".parse().unwrap()),
    };
    let script = r#"for id in 1 2 3; do
  echo "{\"type\":\"assistant\",\"message\":{\"id\":\"msg_$id\",\"content\":[{\"type\":\"text\",\"text\":\"turn $id\"}]}}"
done
sleep 30
echo '{"type":"result","is_error":false,"result":"late"}'"#;

    let started = std::time::Instant::now();
    let outcome = run_bash(&request, script).unwrap();

    assert!(matches!(outcome.status, RunStatus::BudgetExhausted));
    assert_eq!(outcome.failure_kind, Some(FAILURE_KIND_TURN_CAP));
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
}

#[test]
fn a_harness_without_a_turn_flag_is_stopped_once_it_starts_a_turn_past_its_cap() {
    let caps = RunCaps {
        tool_calls: None,
        turns: Some("1".parse().unwrap()),
    };
    let script = r#"echo '{"type":"turn.started"}'
echo '{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}}'
echo '{"type":"turn.started"}'
sleep 30"#;
    let mut command = std::process::Command::new("bash");
    command.arg("-c").arg(script);

    let captured = capture_capped(&mut command, None, CapWatch::of(Runner::Codex, caps)).unwrap();

    assert!(matches!(captured.cap_reached, Some(RunCap::Turns(_))));
}

#[test]
fn claude_enforces_its_own_turn_cap_so_no_watch_is_kept_for_it() {
    let caps = RunCaps {
        tool_calls: None,
        turns: Some("1".parse().unwrap()),
    };

    assert!(CapWatch::of(Runner::ClaudeCode, caps).is_none());
    assert!(CapWatch::of(Runner::Codex, caps).is_some());
    assert!(CapWatch::of(Runner::Codex, RunCaps::default()).is_none());
}
