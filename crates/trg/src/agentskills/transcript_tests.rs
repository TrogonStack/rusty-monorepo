use super::*;
use crate::agentskills::evals::SkillDisclosure;
use crate::agentskills::redact::redact_transcript_bytes;
use crate::agentskills::report::ScenarioKind;

const CURSOR_STREAM: &[u8] = br#"{"type":"system","subtype":"init","cwd":"/w"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I'll read the skill first."}]}}
{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":".skill/SKILL.md"}},"toolCallId":"call-1","startedAtMs":"1"}}
{"type":"thinking","text":"..."}
{"type":"tool_call","subtype":"started","tool_call":{"globToolCall":{"args":{"targetDirectory":"/w","globPattern":"**/*.csv"}},"toolCallId":"call-2","startedAtMs":"2"}}
{"type":"tool_call","subtype":"completed","tool_call":{"globToolCall":{"args":{"targetDirectory":"/w","globPattern":"**/*.csv"},"result":{"success":{"files":[]}}},"toolCallId":"call-2","startedAtMs":"2","completedAtMs":"3"}}
{"type":"tool_call","subtype":"started","tool_call":{"editToolCall":{"args":{"path":"outputs/summary.md","streamContent":"Report line\n"}},"toolCallId":"call-3","startedAtMs":"4"}}
{"type":"result","is_error":false,"result":"done"}
"#;

const CODEX_THREAD: &[u8] = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I'll read the staged skill first."}}
{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"sed -n '1,240p' .skill/SKILL.md\"","status":"in_progress"}}
{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc \"sed -n '1,240p' .skill/SKILL.md\"","exit_code":0,"status":"completed"}}
{"type":"item.started","item":{"id":"item_8","type":"file_change","changes":[{"path":"outputs/summary.md","kind":"add"}],"status":"in_progress"}}
{"type":"turn.completed","usage":{"input_tokens":229974,"output_tokens":1877}}
"#;

const CLAUDE_STREAM: &[u8] = br#"{"type":"system","subtype":"init"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Let me check the skill."},{"type":"tool_use","name":"Read","input":{"file_path":".skill/SKILL.md"}}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/report.md"}}]}}
{"type":"result","is_error":false,"result":"done"}
"#;

#[test]
fn tool_name_rejects_blank() {
    assert!(ToolName::new("  ").is_none());
    assert_eq!(ToolName::new("Read").unwrap().as_str(), "Read");
}

#[test]
fn normalizes_claude_stream_into_ordered_events() {
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(CLAUDE_STREAM),
        &WorkspaceBoundary::unknown(),
    );

    assert_eq!(transcript.tool_visibility, ToolVisibility::Observed);
    assert_eq!(
        transcript
            .tool_sequence()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>(),
        vec!["Read", "Write"]
    );
    assert_eq!(transcript.tool_call_count("read"), 1);
    assert_eq!(transcript.tool_call_count("Bash"), 0);
    assert!(matches!(
        transcript.events.last(),
        Some(TranscriptEvent::Terminal { ok: true })
    ));
}

#[test]
fn staged_skill_path_counts_as_engagement() {
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(CLAUDE_STREAM),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
    assert_eq!(transcript.skill_engagement().engaged(), Some(true));
}

#[test]
fn native_skill_tool_counts_as_engagement() {
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Skill","input":{"skill":"demo-skill"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NativeSkillTool);
}

#[test]
fn an_unannounced_staged_skill_path_counts_as_engagement() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"skills/demo-skill/SKILL.md"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
}

/// A harness keeps its own skills in a directory named the same way, and
/// reading one of those is not this run reaching for the skill under test.
#[test]
fn a_skill_installed_outside_the_workspace_is_not_the_staged_skill() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/opt/harness/skills/other/SKILL.md"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );
    assert!(transcript.escaped_workspace());
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

#[test]
fn tools_without_skill_reference_are_not_engagement() {
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"outputs/report.md"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
    assert_eq!(transcript.skill_engagement().engaged(), Some(false));
}

#[test]
fn unobservable_harness_reports_none_rather_than_not_engaged() {
    let transcript = NormalizedTranscript::unavailable("codex");
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotObservable);
    assert_eq!(transcript.skill_engagement().engaged(), None);
}

#[test]
fn malformed_and_unknown_lines_are_skipped() {
    let stdout = br#"not json at all
{"type":"mystery_future_event","payload":1}
{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":".skill/SKILL.md"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.events.len(), 1);
    assert_eq!(transcript.tool_call_count("Read"), 1);
}

#[test]
fn error_terminal_is_recorded_as_not_ok() {
    let stdout = br#"{"type":"result","is_error":true,"result":"boom"}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert!(matches!(
        transcript.events.first(),
        Some(TranscriptEvent::Terminal { ok: false })
    ));
}

#[test]
fn codex_thread_events_are_normalized_in_the_shell_vocabulary_codex_uses() {
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(CODEX_THREAD),
        &WorkspaceBoundary::unknown(),
    );

    assert_eq!(transcript.tool_visibility, ToolVisibility::Observed);
    assert_eq!(
        transcript
            .tool_sequence()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>(),
        vec!["command_execution", "file_change"]
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
    assert!(matches!(
        transcript.events.first(),
        Some(TranscriptEvent::AssistantText { .. })
    ));
    assert!(matches!(
        transcript.events.last(),
        Some(TranscriptEvent::Terminal { ok: true })
    ));
}

#[test]
fn a_codex_turn_without_items_still_reports_its_terminal_event() {
    let stdout = br#"{"type":"thread.started","thread_id":"abc"}
{"type":"turn.completed","usage":{"input_tokens":120,"output_tokens":40}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.events, vec![TranscriptEvent::Terminal { ok: true }]);
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

#[test]
fn cursor_tool_calls_are_normalized_from_their_member_name() {
    let transcript = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(CURSOR_STREAM),
        &WorkspaceBoundary::unknown(),
    );

    assert_eq!(transcript.tool_visibility, ToolVisibility::Observed);
    assert_eq!(
        transcript
            .tool_sequence()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>(),
        vec!["read", "glob", "edit"]
    );
    assert_eq!(transcript.tool_call_count("Read"), 1);
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
    assert!(matches!(
        transcript.events.first(),
        Some(TranscriptEvent::AssistantText { .. })
    ));
}

#[test]
fn a_completed_cursor_tool_call_does_not_double_count_the_started_one() {
    let stdout = br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":".skill/SKILL.md"}}}}
{"type":"tool_call","subtype":"completed","tool_call":{"readToolCall":{"args":{"path":".skill/SKILL.md"},"result":{"success":{"content":"..."}}}}}
"#;
    let transcript = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.tool_call_count("read"), 1);
}

#[test]
fn a_read_outside_the_workspace_is_reported_against_the_tool_that_made_it() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout =
        br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"/etc/hosts"}}}}
{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"outputs/summary.md"}}}}
{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"../../elsewhere/secret.txt"}}}}
"#;
    let transcript = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(transcript.escaped_workspace());
    assert_eq!(
        transcript
            .workspace_escapes
            .iter()
            .map(|escape| escape.path.as_str())
            .collect::<Vec<_>>(),
        vec!["/etc/hosts", "../../elsewhere/secret.txt"]
    );
    assert!(transcript.workspace_escapes[0].tool.eq_ignore_case("read"));
}

#[test]
fn a_shell_command_is_never_reported_as_a_path_that_left_the_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout =
        br#"{"type":"item.started","item":{"type":"command_execution","command":"sed -n '1,240p' .skill/SKILL.md"}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(!transcript.escaped_workspace());
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
}

#[test]
fn a_shell_read_of_a_host_skill_is_not_read_as_reaching_for_the_staged_one() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout =
        br#"{"type":"item.started","item":{"type":"command_execution","command":"cat ~/.codex/skills/demo/SKILL.md"}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(transcript.escaped_workspace());
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

#[test]
fn a_command_that_reads_both_skills_still_counts_as_reaching_for_the_staged_one() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"item.started","item":{"type":"command_execution","command":"cat skills/demo/SKILL.md ~/.codex/skills/demo/SKILL.md"}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(transcript.escaped_workspace());
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
}

/// A run that reads the filesystem root records `/` as a path that left the
/// workspace, and `/` is the separator every staged skill directory is
/// written with, so one such read must not decide how the rest of the
/// transcript is read.
#[test]
fn a_read_of_the_filesystem_root_does_not_hide_a_later_reach_for_the_staged_skill() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"item.started","item":{"type":"command_execution","command":"ls /"}}
{"type":"item.started","item":{"type":"command_execution","command":"cat .skill/SKILL.md"}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert_eq!(
        transcript
            .workspace_escapes
            .iter()
            .map(|escape| escape.path.as_str())
            .collect::<Vec<_>>(),
        vec!["/"]
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
}

/// A file tool names one path however many spaces it holds, so the argument
/// is judged whole before its tokens are, or the tail of a host path would
/// read as a directory of this run's own.
#[test]
fn a_host_skill_path_holding_a_space_is_not_the_staged_skill() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"/opt/my harness/skills/other/SKILL.md"}}}}
"#;
    let transcript = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(transcript.escaped_workspace());
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

/// A command line quotes a path that holds a space, so the quoting says where
/// the path ends. Read to the next space instead, the tail of a host skill path
/// is a directory of this run's own and a run that never opened the staged skill
/// is graded as having reached for it.
#[test]
fn a_quoted_host_skill_path_holding_a_space_is_not_the_staged_skill() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"item.started","item":{"type":"command_execution","command":"cat \"/opt/my harness/skills/other/SKILL.md\""}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert_eq!(
        transcript
            .workspace_escapes
            .iter()
            .map(|escape| escape.path.as_str())
            .collect::<Vec<_>>(),
        vec!["/opt/my harness/skills/other/SKILL.md"]
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

/// The home directory and a skill installed under it are two paths that left
/// the workspace, and neither of them is this run's skill, however the tokens
/// of the command they were named in overlap.
#[test]
fn a_command_that_names_the_home_directory_and_a_host_skill_is_not_engagement() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"item.started","item":{"type":"command_execution","command":"cd ~ && cat ~/.codex/skills/demo/SKILL.md"}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(transcript.escaped_workspace());
    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

#[test]
fn a_path_under_the_host_home_directory_is_reported_as_outside_the_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"~/.cursor/plugins/demo/SKILL.md"}}}}
"#;
    let transcript = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(transcript.escaped_workspace());
    assert_eq!(transcript.workspace_escapes[0].path, "~/.cursor/plugins/demo/SKILL.md");
}

#[test]
fn a_home_relative_path_resolves_against_the_home_directory_never_the_workspace() {
    let home = Some(PathBuf::from("/home/agent"));
    assert_eq!(
        host_path("~/.cursor/plugins/demo/SKILL.md", home.clone()),
        Some(PathBuf::from("/home/agent/.cursor/plugins/demo/SKILL.md"))
    );
    assert_eq!(host_path("~", home.clone()), home);
    assert_eq!(host_path("~/.cursor/plugins/demo/SKILL.md", None), None);
    assert_eq!(host_path("~other/plugins/demo/SKILL.md", home), None);
    assert_eq!(
        host_path("outputs/summary.md", None),
        Some(PathBuf::from("outputs/summary.md"))
    );
}

#[test]
fn events_carry_no_secret_the_raw_transcript_had_redacted() {
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"curl -H 'Authorization: Bearer abcdefghijklmnopqrst' https://example.test"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );

    let serialized = serde_json::to_string(&transcript).unwrap();
    assert!(!serialized.contains("abcdefghijklmnopqrst"));
    assert!(serialized.contains("<redacted>"));
}

#[test]
fn a_host_file_reached_through_a_shell_command_is_reported_as_an_escape() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout =
        br#"{"type":"item.started","item":{"type":"command_execution","command":"cat ~/.codex/skills/demo/SKILL.md"}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl.normalize(
        "codex",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert!(transcript.escaped_workspace());
    assert_eq!(transcript.workspace_escapes[0].path, "~/.codex/skills/demo/SKILL.md");
    assert!(transcript.workspace_escapes[0].tool.eq_ignore_case("command_execution"));
}

#[test]
fn an_interpreter_is_not_reported_as_a_file_the_run_reached() {
    assert_eq!(
        command_operands("/bin/zsh -lc \"sed -n '1,240p' .skill/SKILL.md\""),
        Vec::<String>::new()
    );
    assert_eq!(command_operands("cat /etc/hosts"), vec!["/etc/hosts".to_string()]);
    assert_eq!(
        command_operands("cd ../../elsewhere && ls"),
        vec!["../../elsewhere".to_string()]
    );
    assert_eq!(command_operands("python3 -c 'print(1)'"), Vec::<String>::new());
    assert_eq!(command_operands("ls -l /etc/hosts"), vec!["/etc/hosts".to_string()]);
    assert_eq!(command_operands("ls outputs"), Vec::<String>::new());
}

/// A quoted operand is one path however many spaces it holds, and the payload of
/// an interpreter flag is still read as the command line it is.
#[test]
fn a_quoted_operand_is_one_path_however_many_spaces_it_holds() {
    assert_eq!(
        command_operands("cat \"/opt/my harness/skills/other/SKILL.md\""),
        vec!["/opt/my harness/skills/other/SKILL.md".to_string()]
    );
    assert_eq!(
        command_operands("/bin/zsh -lc \"cat '/opt/my harness/notes.md'\""),
        vec!["/opt/my harness/notes.md".to_string()]
    );
}

#[test]
fn an_unknown_boundary_reports_nothing_as_outside_it() {
    let boundary = WorkspaceBoundary::unknown();
    assert!(boundary.contains("/etc/hosts"));
}

#[test]
fn cursor_terminal_event_carries_its_error_flag() {
    let ok = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(
            br#"{"type":"system","subtype":"init"}
{"type":"result","is_error":false,"result":"hello"}
"#,
        ),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(ok.events, vec![TranscriptEvent::Terminal { ok: true }]);

    let failed = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(br#"{"type":"result","is_error":true,"result":"boom"}"#),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(failed.events, vec![TranscriptEvent::Terminal { ok: false }]);
}

#[test]
fn normalized_transcript_is_persisted_beside_the_raw_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("transcript.jsonl");
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(CLAUDE_STREAM),
        &WorkspaceBoundary::unknown(),
    );

    write_normalized_transcript(&transcript_path, &transcript).unwrap();

    assert_eq!(
        normalized_transcript_path(&transcript_path),
        dir.path().join(NORMALIZED_TRANSCRIPT_FILE)
    );
    assert_eq!(read_normalized_transcript(&transcript_path).unwrap(), transcript);
}

#[test]
fn normalized_transcript_round_trips_as_json() {
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(CLAUDE_STREAM),
        &WorkspaceBoundary::unknown(),
    );
    let json = serde_json::to_string(&transcript).unwrap();
    let parsed: NormalizedTranscript = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, transcript);
}

fn staged(disclosure: SkillDisclosure) -> StagedSkill {
    StagedSkill::At {
        directory: StagedSkillDir::for_run(ScenarioKind::WithSkill, disclosure, "demo-skill").unwrap(),
        name: StagedSkillName::known(SkillName::parse("demo-skill").unwrap()),
    }
}

/// The arm that runs without a skill is the control every comparison rests on.
/// `skills/` is where an unannounced run stages a skill, so a control arm that
/// looked into one while searching is read as having used a skill it was never
/// handed, and the comparison has nothing left to compare against.
#[test]
fn the_arm_that_staged_no_skill_did_not_reach_for_one() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"skills/demo-skill/SKILL.md"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    )
    .staged_at(StagedSkill::Nothing);

    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

/// A harness carries skills of its own, so its native skill tool can be invoked
/// on an arm that was handed none, and what it invoked was never the skill under
/// test.
#[test]
fn a_native_skill_tool_call_is_not_engagement_when_nothing_was_staged() {
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Skill","input":{"command":"some-installed-skill"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    )
    .staged_at(StagedSkill::Nothing);

    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

/// Each arm stages at the directory its own scenario and disclosure ask for, so
/// the directory a different arm would have staged at is not this run's skill.
#[test]
fn the_directory_another_arm_would_stage_at_is_not_this_run_s_skill() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"skills/demo-skill/SKILL.md"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    )
    .staged_at(staged(SkillDisclosure::Announced));

    assert_eq!(transcript.skill_engagement(), SkillEngagement::NotEngaged);
}

#[test]
fn the_directory_this_run_staged_at_is_engagement() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"item.started","item":{"type":"command_execution","command":"/bin/zsh -lc \"sed -n '1,240p' skills/demo-skill/SKILL.md\""}}
"#;
    let transcript = TranscriptFormat::CodexThreadJsonl
        .normalize(
            "codex",
            &redact_transcript_bytes(stdout),
            &WorkspaceBoundary::at(workspace.path()),
        )
        .staged_at(staged(SkillDisclosure::Unannounced));

    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
}

/// Grading reads the transcript back from disk rather than the run that wrote
/// it, so what a run staged has to survive the round trip to be read against.
#[test]
fn what_a_run_staged_survives_the_transcript_being_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let transcript_path = dir.path().join("transcript.jsonl");
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(CLAUDE_STREAM),
        &WorkspaceBoundary::unknown(),
    )
    .staged_at(staged(SkillDisclosure::Announced));

    write_normalized_transcript(&transcript_path, &transcript).unwrap();

    assert_eq!(
        read_normalized_transcript(&transcript_path).unwrap().staged_skill,
        staged(SkillDisclosure::Announced)
    );
}

/// A transcript written before a run recorded what it staged is still read, and
/// the only reading left is against every directory a run stages at.
#[test]
fn a_transcript_that_records_no_staging_is_read_against_every_staged_directory() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"skills/demo-skill/SKILL.md"}}]}}
"#;
    let transcript = normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );
    let json = serde_json::to_value(&transcript).unwrap();

    assert!(
        json.get("staged_skill").is_none(),
        "a run that recorded nothing writes no field: {json}"
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
}

fn a_skill_tool_call_naming(skill: &str) -> NormalizedTranscript {
    let stdout = format!(
        r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"Skill","input":{{"skill":"{skill}"}}}}]}}}}"#
    );
    normalize_stream_json(
        "claude",
        &redact_transcript_bytes(stdout.as_bytes()),
        &WorkspaceBoundary::unknown(),
    )
}

/// A harness invokes its own installed skills through the same tool, so a run
/// that reached for one of those never touched the skill it was handed, and a
/// case that stages several has no way to ask which one the agent picked unless
/// the name the call carries decides it.
#[test]
fn a_skill_tool_call_that_names_another_skill_is_not_this_run_s_skill() {
    for disclosure in [SkillDisclosure::Announced, SkillDisclosure::Unannounced] {
        let transcript = a_skill_tool_call_naming("some-installed-skill").staged_at(staged(disclosure));

        assert_eq!(
            transcript.skill_engagement(),
            SkillEngagement::NotEngaged,
            "{disclosure:?} credited a skill this run never staged"
        );
    }
}

/// The announced arms stage under a directory that carries no name at all, so
/// the name a call reports can only be read against the name the run recorded.
#[test]
fn a_skill_tool_call_that_names_the_staged_skill_is_engagement() {
    for disclosure in [SkillDisclosure::Announced, SkillDisclosure::Unannounced] {
        let transcript = a_skill_tool_call_naming("demo-skill").staged_at(staged(disclosure));

        assert_eq!(
            transcript.skill_engagement(),
            SkillEngagement::NativeSkillTool,
            "{disclosure:?} did not credit the skill it staged"
        );
    }
}

/// A harness writes the name, not this repository, so the reading does not rest
/// on the case it came back in.
#[test]
fn a_skill_tool_call_names_the_staged_skill_whatever_case_the_harness_reports() {
    let transcript = a_skill_tool_call_naming("Demo-Skill").staged_at(staged(SkillDisclosure::Announced));

    assert_eq!(transcript.skill_engagement(), SkillEngagement::NativeSkillTool);
}

/// A transcript written before the name was recorded cannot say which skill a
/// call invoked, and that is not evidence the wrong one was, so it keeps the
/// reading it was written under rather than losing every skill tool call.
#[test]
fn a_transcript_that_records_no_skill_name_still_reads_a_skill_tool_call_as_engagement() {
    let transcript = a_skill_tool_call_naming("some-installed-skill").staged_at(StagedSkill::At {
        directory: StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Announced, "demo-skill").unwrap(),
        name: StagedSkillName::Unrecorded,
    });

    assert_eq!(transcript.skill_engagement(), SkillEngagement::NativeSkillTool);
}

/// Grading reads transcripts that were written before the name was part of the
/// record, so the field a run now writes has to be one an older file can omit.
#[test]
fn a_staging_record_written_without_a_skill_name_is_still_read() {
    let staged: StagedSkill = serde_json::from_str(r#"{"kind":"at","directory":".skill/"}"#).unwrap();

    assert_eq!(
        staged,
        StagedSkill::At {
            directory: StagedSkillDir::for_run(ScenarioKind::WithSkill, SkillDisclosure::Announced, "demo-skill")
                .unwrap(),
            name: StagedSkillName::Unrecorded,
        }
    );
}

/// Shaped after a captured `trogonai-agent ask --output-format stream-json` run.
const TROGONAI_AGENT_STREAM: &[u8] = br#"{"protocol_version":1,"seq":1,"session_id":"s","turn_id":1,"event":{"type":"text_chunk","origin":"assistant","text":"Reading "}}
{"protocol_version":1,"seq":2,"session_id":"s","turn_id":1,"event":{"type":"text_chunk","origin":"operational","text":"Retrying the provider"}}
{"protocol_version":1,"seq":3,"session_id":"s","turn_id":1,"event":{"type":"text_chunk","origin":"assistant","text":"the skill."}}
{"protocol_version":1,"seq":4,"session_id":"s","turn_id":1,"event":{"type":"tool_started","call_id":"call_1","tool_name":"read_file","arguments_json":"{\"path\": \".skill/SKILL.md\"}"}}
{"protocol_version":1,"seq":5,"session_id":"s","turn_id":1,"event":{"type":"tool_finished","call_id":"call_1","outcome":"completed","result":"..."}}
{"protocol_version":1,"seq":6,"session_id":"s","turn_id":1,"event":{"type":"tool_started","call_id":"call_2","tool_name":"run_command","arguments_json":"{\"command\": \"cat ~/.trogonai-agent/settings.json\"}"}}
{"protocol_version":1,"seq":7,"session_id":"s","turn_id":1,"event":{"type":"tool_finished","call_id":"call_2","outcome":"failed","result":"denied"}}
{"protocol_version":1,"seq":8,"session_id":"s","turn_id":1,"event":{"type":"text_chunk","origin":"assistant","text":"Done."}}
{"type":"result","output":"Reading the skill.\n\nDone.","final_output":"Done.","exit_code":0,"model":"gpt-6.2-astra","resolved_provider":null,"session_id":"","steps":1,"tool_calls":[],"usage":{"input_tokens":200,"output_tokens":40}}
"#;

#[test]
fn trogonai_agent_text_chunks_between_tool_calls_are_one_assistant_message() {
    let transcript = TranscriptFormat::TrogonaiAgentStreamJson.normalize(
        "trogonai-agent",
        &redact_transcript_bytes(TROGONAI_AGENT_STREAM),
        &WorkspaceBoundary::unknown(),
    );

    let texts: Vec<&str> = transcript
        .events
        .iter()
        .filter_map(|event| match event {
            TranscriptEvent::AssistantText { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["Reading the skill.", "Done."],
        "operational notices are the runtime's, not the model's"
    );
    assert_eq!(transcript.events.last(), Some(&TranscriptEvent::Terminal { ok: true }));
}

#[test]
fn trogonai_agent_tool_arguments_are_read_out_of_their_json_string() {
    let workspace = tempfile::tempdir().unwrap();
    let transcript = TranscriptFormat::TrogonaiAgentStreamJson.normalize(
        "trogonai-agent",
        &redact_transcript_bytes(TROGONAI_AGENT_STREAM),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert_eq!(
        transcript
            .tool_sequence()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>(),
        vec!["read_file", "run_command"]
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
    assert_eq!(
        transcript
            .workspace_escapes
            .iter()
            .map(|escape| escape.path.as_str())
            .collect::<Vec<_>>(),
        vec!["~/.trogonai-agent/settings.json"],
        "a shell command that names the host profile left the workspace"
    );
}

#[test]
fn a_trogonai_agent_result_that_names_an_error_is_a_terminal_that_did_not_succeed() {
    let stdout = br#"{"type":"result","output":"","final_output":"","exit_code":2,"model":"","steps":0,"tool_calls":[],"usage":{"input_tokens":null,"output_tokens":null},"error":"InvalidAskArgs"}
"#;
    let transcript = TranscriptFormat::TrogonaiAgentStreamJson.normalize(
        "trogonai-agent",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.events, vec![TranscriptEvent::Terminal { ok: false }]);
}

/// trogonai-agent names the skill its own `skill` tool loads in a `name` argument.
#[test]
fn a_trogonai_agent_skill_tool_call_naming_the_staged_skill_is_native_engagement() {
    let load = |skill: &str| {
        let stdout = format!(
            r#"{{"protocol_version":1,"seq":1,"session_id":"s","turn_id":1,"event":{{"type":"tool_started","call_id":"call_1","tool_name":"skill","arguments_json":"{{\"name\": \"{skill}\"}}"}}}}
"#
        );
        TranscriptFormat::TrogonaiAgentStreamJson
            .normalize(
                "trogonai-agent",
                &redact_transcript_bytes(stdout.as_bytes()),
                &WorkspaceBoundary::unknown(),
            )
            .staged_at(staged(SkillDisclosure::Unannounced))
            .skill_engagement()
    };
    assert_eq!(load("demo-skill"), SkillEngagement::NativeSkillTool);
    assert_eq!(load("some-installed-skill"), SkillEngagement::NotEngaged);
}

/// Shaped after a captured `opencode run --format json` run.
const OPENCODE_STREAM: &[u8] = br#"{"type":"step_start","part":{"type":"step-start"}}
{"type":"text","part":{"type":"text","text":"Reading the skill."}}
{"type":"tool_use","part":{"id":"call_1","tool":"read","state":{"status":"completed","input":{"filePath":".skill/SKILL.md"},"output":"..."}}}
{"type":"tool_use","part":{"id":"call_2","tool":"shell","state":{"status":"completed","input":{"command":"cat ~/.config/opencode/opencode.json"},"output":"..."}}}
{"type":"step_finish","part":{"type":"step-finish","reason":"tool-calls","tokens":{"input":3,"output":10}}}
{"type":"step_start","part":{"type":"step-start"}}
{"type":"text","part":{"type":"text","text":"Done."}}
{"type":"step_finish","part":{"type":"step-finish","reason":"stop","tokens":{"input":3,"output":2}}}
"#;

#[test]
fn opencode_tools_text_and_a_stopped_last_step_are_normalized() {
    let workspace = tempfile::tempdir().unwrap();
    let transcript = TranscriptFormat::OpenCodeJson.normalize(
        "opencode",
        &redact_transcript_bytes(OPENCODE_STREAM),
        &WorkspaceBoundary::at(workspace.path()),
    );

    assert_eq!(
        transcript
            .tool_sequence()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>(),
        vec!["read", "shell"]
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
    assert_eq!(
        transcript
            .workspace_escapes
            .iter()
            .map(|escape| escape.path.as_str())
            .collect::<Vec<_>>(),
        vec!["~/.config/opencode/opencode.json"]
    );
    assert_eq!(transcript.events.last(), Some(&TranscriptEvent::Terminal { ok: true }));
}

#[test]
fn pi_tools_and_assistant_text_are_normalized_and_a_stopped_run_succeeded() {
    let stdout = br#"{"type":"turn_start"}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Reading the skill."}],"stopReason":"toolUse"}}
{"type":"tool_execution_start","toolCallId":"c1","toolName":"read","args":{"path":".skill/SKILL.md"}}
{"type":"tool_execution_start","toolCallId":"c2","toolName":"bash","args":{"command":"cat ~/.pi/agent/auth.json"}}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Done."}],"stopReason":"stop"}}
"#;
    let workspace = tempfile::tempdir().unwrap();
    let transcript = TranscriptFormat::PiJson.normalize(
        "pi",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );
    assert_eq!(
        transcript
            .tool_sequence()
            .iter()
            .map(|t| t.as_str())
            .collect::<Vec<_>>(),
        vec!["read", "bash"]
    );
    assert_eq!(transcript.skill_engagement(), SkillEngagement::StagedPathReference);
    assert_eq!(
        transcript
            .workspace_escapes
            .iter()
            .map(|escape| escape.path.as_str())
            .collect::<Vec<_>>(),
        vec!["~/.pi/agent/auth.json"]
    );
    assert_eq!(transcript.events.last(), Some(&TranscriptEvent::Terminal { ok: true }));
}

#[test]
fn a_pi_run_whose_last_message_stopped_on_an_error_did_not_succeed() {
    let stdout = br#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error"}}
"#;
    let transcript =
        TranscriptFormat::PiJson.normalize("pi", &redact_transcript_bytes(stdout), &WorkspaceBoundary::unknown());
    assert_eq!(transcript.events.last(), Some(&TranscriptEvent::Terminal { ok: false }));
}

#[test]
fn a_pi_stream_cut_off_after_a_tool_call_message_records_no_terminal() {
    let stdout = br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Reading."}],"stopReason":"toolUse"}}
{"type":"tool_execution_start","toolCallId":"c1","toolName":"read","args":{"path":"notes.txt"}}
"#;
    let transcript =
        TranscriptFormat::PiJson.normalize("pi", &redact_transcript_bytes(stdout), &WorkspaceBoundary::unknown());
    assert!(
        !transcript
            .events
            .iter()
            .any(|event| matches!(event, TranscriptEvent::Terminal { .. })),
        "{:?}",
        transcript.events
    );
}

#[test]
fn an_opencode_error_event_is_a_terminal_that_did_not_succeed() {
    let stdout = br#"{"type":"step_start","part":{}}
{"type":"error","error":{"type":"provider.auth","message":"HTTP 401"}}
"#;
    let transcript = TranscriptFormat::OpenCodeJson.normalize(
        "opencode",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::unknown(),
    );
    assert_eq!(transcript.events, vec![TranscriptEvent::Terminal { ok: false }]);
}
