use std::path::Path;

use tempfile::tempdir;

use super::fake::{invoke_with_retries, run_bash};
use super::{EvalRunRequest, RunStatus, FAILURE_KIND_RUNNER};
use crate::agentskills::evals::EvalCase;
use crate::agentskills::report::ScenarioKind;

fn make_case() -> EvalCase {
    serde_json::from_value(serde_json::json!({
        "id": "case-1",
        "prompt": "do the thing",
        "expected_output": "done",
        "files": [],
        "assertions": [],
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
        old_skill_md: None,
        old_skill_path: None,
        workspace_dir: workspace,
        transcript_path,
        stderr_path,
        runner_model: None,
        timeout_secs,
        skill_staging: crate::agentskills::report::SkillStaging::Symlink,
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

    let counter = workspace.join("attempts");
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
