use super::*;
use crate::agentskills::evals::EvalSplit;
use crate::agentskills::mocks::{ExpectPath, MockCallLogEntry, MockViolation, ServerName, ToolName};
use crate::agentskills::model_name::ModelName;
use crate::agentskills::report::{RunMetrics, RunPaths, ScenarioKind};
use tempfile::tempdir;

fn sample_key_input(scenario: ScenarioKind, skill_hash: &str, fixture_hash: &str) -> CacheKeyInput {
    sample_key_input_for_case("one", scenario, skill_hash, fixture_hash)
}

fn sample_key_input_for_case(
    eval_case_id: &str,
    scenario: ScenarioKind,
    skill_hash: &str,
    fixture_hash: &str,
) -> CacheKeyInput {
    CacheKeyInput {
        eval_case_id: eval_case_id.to_string(),
        skill_hash: skill_hash.to_string(),
        evals_hash: "sha256:evals".to_string(),
        fixture_hash: fixture_hash.to_string(),
        mock_hash: None,
        model_config: "ci-default".to_string(),
        runner_model: None,
        runner_kind: "codex".to_string(),
        runner_version: None,
        scenario,
        attempt: 1,
        prompt_contract_version: PROMPT_CONTRACT_VERSION.to_string(),
        environment: EnvironmentPolicy::default(),
        permission: PermissionGrant::default(),
        tool_grant: None,
        skill_staging: SkillStaging::default(),
        caps: RunCaps::default(),
        scaffold_hash: None,
        companion_hash: None,
    }
}

/// A run set up by another script worked in another directory, so it answers another
/// question. `skill_hash` is the digest of `SKILL.md` alone and would not notice.
#[test]
fn a_run_from_another_scaffold_is_a_different_run() {
    let base = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
    let seeded = CacheKeyInput {
        scaffold_hash: Some("sha256:seeded".to_string()),
        ..base.clone()
    };
    let edited = CacheKeyInput {
        scaffold_hash: Some("sha256:edited".to_string()),
        ..base.clone()
    };

    assert_ne!(
        CacheKey::from_input(&seeded),
        CacheKey::from_input(&edited),
        "the state a run was handed belongs in its identity"
    );
    assert_ne!(
        CacheKey::from_input(&seeded),
        CacheKey::from_input(&base),
        "a run handed a scaffolded directory is not the run that got an empty one"
    );
    assert_ne!(
        ReuseKeyInput::of(&seeded),
        ReuseKeyInput::of(&edited),
        "--reuse-completed cannot forget it either"
    );
}

/// A case that declares no scaffold has to key exactly as it did before the field
/// existed, or every entry already on disk is evicted for nothing.
#[test]
fn a_case_with_no_scaffold_keys_as_it_did_before() {
    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
    let serialized = serde_json::to_string(&input).unwrap();
    assert!(
        !serialized.contains("scaffold_hash"),
        "an absent scaffold must not reach the key: {serialized}"
    );
}

/// The distractors are what an unannounced case asks the run to choose between, so a
/// run answers only for the set it was handed.
#[test]
fn a_run_handed_other_companion_skills_is_a_different_run() {
    let base = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
    let staged = CacheKeyInput {
        companion_hash: Some("sha256:companions".to_string()),
        ..base.clone()
    };
    let edited = CacheKeyInput {
        companion_hash: Some("sha256:companions-edited".to_string()),
        ..base.clone()
    };

    assert_ne!(
        CacheKey::from_input(&staged),
        CacheKey::from_input(&edited),
        "an edited distractor is not the distractor the run passed over"
    );
    assert_ne!(
        CacheKey::from_input(&staged),
        CacheKey::from_input(&base),
        "a run that had something to choose between is not the run that had nothing"
    );
    assert_ne!(
        ReuseKeyInput::of(&staged),
        ReuseKeyInput::of(&edited),
        "--reuse-completed cannot forget it either"
    );
}

/// A case that declares no companions has to key exactly as it did before the field
/// existed, or every entry already on disk is evicted for nothing.
#[test]
fn a_case_with_no_companion_skills_keys_as_it_did_before() {
    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
    let serialized = serde_json::to_string(&input).unwrap();
    assert!(
        !serialized.contains("companion_hash"),
        "an absent companion set must not reach the key: {serialized}"
    );
}

/// A case that declares no mocks has to key exactly as it did before `mock_hash`
/// existed, or every entry already on disk is evicted for nothing.
#[test]
fn a_case_with_no_mocks_keys_as_it_did_before() {
    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
    let serialized = serde_json::to_string(&input).unwrap();
    assert!(
        !serialized.contains("mock_hash"),
        "an absent mock set must not reach the key: {serialized}"
    );
}

/// A run the harness carried out under one grant is not the run another grant asked
/// for, and serving it would put a grant in the report that no harness ever ran with.
#[test]
fn a_run_from_another_permission_grant_is_a_different_run() {
    let workspace_write = CacheKeyInput {
        permission: PermissionGrant::WorkspaceWrite,
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
    };
    let unrestricted = CacheKeyInput {
        permission: PermissionGrant::Unrestricted,
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
    };

    assert_ne!(
        CacheKey::from_input(&workspace_write),
        CacheKey::from_input(&unrestricted),
        "the grant a run was carried out under belongs in its identity"
    );
}

/// A run allowed forty tool calls completed; the same run allowed five would have been
/// stopped, so the two are different runs and neither may be served for the other.
#[test]
fn a_run_under_other_caps_is_a_different_run() {
    let unbounded = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
    let capped = CacheKeyInput {
        caps: RunCaps {
            tool_calls: Some("5".parse().unwrap()),
            turns: None,
        },
        ..unbounded.clone()
    };

    assert_ne!(
        CacheKey::from_input(&unbounded),
        CacheKey::from_input(&capped),
        "the caps a run ran under belong in its identity"
    );
    assert_ne!(
        ReuseKeyInput::of(&unbounded),
        ReuseKeyInput::of(&capped),
        "a reused run answers only for the caps it ran under"
    );
    let serialized = serde_json::to_string(&unbounded).unwrap();
    assert!(
        !serialized.contains("caps"),
        "an unbounded run must serialize as it did before caps existed: {serialized}"
    );
}

/// A run that inherited this machine is not the run a scrubbed invocation asked
/// for, so the completed one must not be served in its place.
#[test]
fn a_run_from_another_environment_policy_is_a_different_run() {
    let inherited = CacheKeyInput {
        environment: EnvironmentPolicy::Inherited,
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
    };
    let scrubbed = CacheKeyInput {
        environment: EnvironmentPolicy::Scrubbed,
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
    };
    let symlinked = CacheKeyInput {
        skill_staging: SkillStaging::Symlink,
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
    };

    assert_ne!(
        CacheKey::from_input(&inherited),
        CacheKey::from_input(&scrubbed),
        "the environment a run saw belongs in its identity"
    );
    assert_ne!(
        CacheKey::from_input(&symlinked),
        CacheKey::from_input(&sample_key_input(
            ScenarioKind::WithSkill,
            "sha256:skill",
            "sha256:fixture"
        )),
        "what the run could reach out of its workspace belongs in its identity"
    );
}

fn sample_run_record(run_id: &str, eval_case_id: &str, scenario: ScenarioKind) -> RunRecord {
    RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: run_id.to_string(),
        eval_case_id: eval_case_id.to_string(),
        eval_slug: eval_case_id.to_string(),
        split: EvalSplit::Train,
        scenario_id: scenario,
        iteration: 2,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 0,
        status: "skipped".to_string(),
        paths: RunPaths {
            workspace: format!("runs/{run_id}/workspace"),
            outputs: format!("runs/{run_id}/workspace/outputs"),
        },
        mirror_path: format!("iteration-2/eval-{eval_case_id}/{}/", scenario.as_str()),
        tool_grant: None,
        artifacts: Vec::new(),
        metrics: RunMetrics {
            duration_ms: None,
            exit_code: None,
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score: None,
        trace: None,
        grade_trace: None,
    }
}

fn write_completed_run(report_dir: &Path, run_id: &str, eval_case_id: &str, scenario: ScenarioKind) {
    write_completed_run_with(report_dir, run_id, eval_case_id, scenario, |_| {});
}

fn write_completed_run_with(
    report_dir: &Path,
    run_id: &str,
    eval_case_id: &str,
    scenario: ScenarioKind,
    customize: impl FnOnce(&mut RunRecord),
) {
    let run_dir = report_dir.join("runs").join(run_id);
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(workspace.join("outputs")).unwrap();
    fs::write(run_dir.join("transcript.jsonl"), "{}\n").unwrap();
    fs::write(run_dir.join("timing.json"), r#"{"duration_ms":42}"#).unwrap();
    fs::write(workspace.join("outputs/final.md"), "done").unwrap();

    let mut run = RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: run_id.to_string(),
        eval_case_id: eval_case_id.to_string(),
        eval_slug: eval_case_id.to_string(),
        split: EvalSplit::Train,
        scenario_id: scenario,
        iteration: 1,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 1,
        status: "completed".to_string(),
        paths: RunPaths {
            workspace: format!("runs/{run_id}/workspace"),
            outputs: format!("runs/{run_id}/workspace/outputs"),
        },
        mirror_path: format!("iteration-1/eval-{eval_case_id}/{}/", scenario.as_str()),
        tool_grant: None,
        artifacts: vec![serde_json::json!({"kind":"transcript","path":format!("runs/{run_id}/transcript.jsonl")})],
        metrics: RunMetrics {
            duration_ms: Some(42),
            exit_code: Some(0),
            total_tokens: Some(10),
            input_tokens: Some(6),
            output_tokens: Some(4),
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score: None,
        trace: None,
        grade_trace: None,
    };

    customize(&mut run);

    let document = serde_json::json!({
        "report": {"id":"r1","generated_at":"2026-01-01T00:00:00Z","iteration":1,"producer":{"name":"trg","version":"0.0.0"}},
        "suite": {"skill_name":"demo","skill_path":"demo","skill_hash":"sha256:skill","evals_path":"demo/evals/evals.json","evals_hash":"sha256:evals"},
        "dimensions": {"eval_cases":[],"assertions":[],"skill_revisions":[],"model_configs":[],"scenarios":[],"grading_strategies":[]},
        "runs": [run],
        "assertion_results": [],
        "summaries": {"by_scenario":[]},
        "comparisons": []
    });
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();
}

/// Same as `write_completed_run`, but the run recorded one `expect` violation while it
/// ran: `mock-calls.jsonl` carries it as the mock server would have logged it, and the
/// run's own `mock_violations` carries it as `apply_outcome` would have read it back.
fn write_completed_run_with_mock_violation(report_dir: &Path, run_id: &str, eval_case_id: &str) {
    let run_dir = report_dir.join("runs").join(run_id);
    let workspace = run_dir.join("workspace");
    fs::create_dir_all(workspace.join("outputs")).unwrap();
    fs::write(run_dir.join("transcript.jsonl"), "{}\n").unwrap();
    fs::write(run_dir.join("timing.json"), r#"{"duration_ms":42}"#).unwrap();
    fs::write(workspace.join("outputs/final.md"), "done").unwrap();

    let violation = MockViolation {
        server: ServerName::from("github"),
        tool: ToolName::from("create_issue"),
        path: ExpectPath::from("repo"),
        constraint: "/^acme\\//".to_string(),
        received: serde_json::json!("other/repo"),
    };
    let call_log = MockCallLogEntry {
        server: violation.server.clone(),
        tool: violation.tool.clone(),
        input: serde_json::json!({"repo": "other/repo"}),
        violations: vec![violation.clone()],
        received_at: None,
    };
    fs::write(
        run_dir.join(MOCK_CALLS_LOG_NAME),
        format!("{}\n", serde_json::to_string(&call_log).unwrap()),
    )
    .unwrap();

    let run = RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: run_id.to_string(),
        eval_case_id: eval_case_id.to_string(),
        eval_slug: eval_case_id.to_string(),
        split: EvalSplit::Train,
        scenario_id: ScenarioKind::WithSkill,
        iteration: 1,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 1,
        status: "completed".to_string(),
        paths: RunPaths {
            workspace: format!("runs/{run_id}/workspace"),
            outputs: format!("runs/{run_id}/workspace/outputs"),
        },
        mirror_path: format!("iteration-1/eval-{eval_case_id}/with_skill/"),
        tool_grant: None,
        artifacts: vec![serde_json::json!({"kind":"transcript","path":format!("runs/{run_id}/transcript.jsonl")})],
        metrics: RunMetrics {
            duration_ms: Some(42),
            exit_code: Some(0),
            total_tokens: Some(10),
            input_tokens: Some(6),
            output_tokens: Some(4),
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        warnings: Vec::new(),
        read_only_fixture_violations: Vec::new(),
        mock_violations: vec![violation],
        case_score: None,
        trace: None,
        grade_trace: None,
    };

    let document = serde_json::json!({
        "report": {"id":"r1","generated_at":"2026-01-01T00:00:00Z","iteration":1,"producer":{"name":"trg","version":"0.0.0"}},
        "suite": {"skill_name":"demo","skill_path":"demo","skill_hash":"sha256:skill","evals_path":"demo/evals/evals.json","evals_hash":"sha256:evals"},
        "dimensions": {"eval_cases":[],"assertions":[],"skill_revisions":[],"model_configs":[],"scenarios":[],"grading_strategies":[]},
        "runs": [run],
        "assertion_results": [],
        "summaries": {"by_scenario":[]},
        "comparisons": []
    });
    fs::write(
        report_dir.join("report.json"),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();
}

#[test]
fn cache_key_is_stable_for_identical_input() {
    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures");
    let a = CacheKey::from_input(&input);
    let b = CacheKey::from_input(&input);
    assert_eq!(a, b);
}

#[test]
fn cache_key_changes_when_scenario_differs() {
    let with_skill = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures");
    let without_skill = sample_key_input(ScenarioKind::WithoutSkill, "sha256:skill", "sha256:fixtures");
    assert_ne!(CacheKey::from_input(&with_skill), CacheKey::from_input(&without_skill));
}

#[test]
fn cache_key_changes_when_the_eval_case_differs() {
    let empty_fixtures = FixtureHash::empty();
    let first = sample_key_input_for_case("one", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
    let second = sample_key_input_for_case("two", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
    assert_ne!(CacheKey::from_input(&first), CacheKey::from_input(&second));
}

#[test]
fn a_completed_run_is_not_served_to_a_different_eval_case() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let empty_fixtures = FixtureHash::empty();
    let first = sample_key_input_for_case("one", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
    record_completion(&out_dir, &CacheKey::from_input(&first), &first, &report_a, "run-001").unwrap();

    let second = sample_key_input_for_case("two", ScenarioKind::WithSkill, "sha256:skill", empty_fixtures.as_str());
    let reuse_second = ReuseKeyInput::of(&second);
    let options = CacheOptions {
        enabled: true,
        reuse_completed: true,
    };

    assert!(try_resolve_cache(&out_dir, options, &second, &reuse_second).is_none());
    assert!(try_resolve_cache(&out_dir, options, &first, &reuse_second).is_some());
}

#[test]
fn fixture_hash_is_stable_and_changes_with_content() {
    let temp = tempdir().unwrap();
    let skill = temp.path().join("skill");
    let fixtures = skill.join("evals/one/fixtures");
    fs::create_dir_all(&fixtures).unwrap();

    let first = compute_fixture_hash(&skill, &EvalDirName::default(), "one").unwrap();
    fs::write(fixtures.join("input.txt"), "alpha").unwrap();
    let second = compute_fixture_hash(&skill, &EvalDirName::default(), "one").unwrap();
    fs::write(fixtures.join("input.txt"), "beta").unwrap();
    let third = compute_fixture_hash(&skill, &EvalDirName::default(), "one").unwrap();

    assert_eq!(first, FixtureHash::empty());
    assert_ne!(first, second);
    assert_ne!(second, third);
}

#[test]
fn exact_cache_hit_reuses_completed_run() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    let key = CacheKey::from_input(&input);
    record_completion(&out_dir, &key, &input, &report_a, "run-001").unwrap();

    let report_b = out_dir.join("demo/report-b");
    fs::create_dir_all(report_b.join("runs/run-001/workspace/outputs")).unwrap();
    let mut run = RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: "run-001".to_string(),
        eval_case_id: "one".to_string(),
        eval_slug: "one".to_string(),
        split: EvalSplit::Train,
        scenario_id: ScenarioKind::WithSkill,
        iteration: 2,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 0,
        status: "skipped".to_string(),
        paths: RunPaths {
            workspace: "runs/run-001/workspace".to_string(),
            outputs: "runs/run-001/workspace/outputs".to_string(),
        },
        mirror_path: "iteration-2/eval-one/with_skill/".to_string(),
        tool_grant: None,
        artifacts: Vec::new(),
        metrics: RunMetrics {
            duration_ms: None,
            exit_code: None,
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score: None,
        trace: None,
        grade_trace: None,
    };

    let pointer = lookup_exact(&out_dir, &key, &input).unwrap();
    apply_cache_hit(&mut run, &key, &pointer, &report_b).unwrap();

    assert_eq!(run.status, "completed");
    assert_eq!(run.metrics.duration_ms, Some(42));
    assert!(run.cache.as_ref().unwrap().hit);
    assert_eq!(run.cache.as_ref().unwrap().source_run_id, "run-001");
    assert!(report_b.join("runs/run-001/transcript.jsonl").is_file());
    assert!(report_b.join("runs/run-001/workspace/outputs/final.md").is_file());
}

/// `capture_status: "partial"` is a retired spelling of `incomplete` that an older build
/// left behind in a source report a cache pointer still names. Reading it back must not
/// be a cache miss: it aliases cleanly to `incomplete`, so the hit reuses the run exactly
/// as `exact_cache_hit_reuses_completed_run` does for a report the current build wrote.
#[test]
fn a_cache_hit_against_a_report_with_the_retired_partial_capture_status_still_reuses_the_run() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let report_path = report_a.join("report.json");
    let mut document: serde_json::Value = serde_json::from_str(&fs::read_to_string(&report_path).unwrap()).unwrap();
    document["dimensions"]["model_configs"] = serde_json::json!([{
        "id": "ci-default",
        "capture_status": "partial",
        "label": "ci-default",
        "parameters": {},
        "parameter_sources": {},
        "extra": {}
    }]);
    fs::write(&report_path, serde_json::to_string_pretty(&document).unwrap()).unwrap();

    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    let key = CacheKey::from_input(&input);
    record_completion(&out_dir, &key, &input, &report_a, "run-001").unwrap();

    let report_b = out_dir.join("demo/report-b");
    fs::create_dir_all(report_b.join("runs/run-001/workspace/outputs")).unwrap();
    let mut run = RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: "run-001".to_string(),
        eval_case_id: "one".to_string(),
        eval_slug: "one".to_string(),
        split: EvalSplit::Train,
        scenario_id: ScenarioKind::WithSkill,
        iteration: 2,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 0,
        status: "skipped".to_string(),
        paths: RunPaths {
            workspace: "runs/run-001/workspace".to_string(),
            outputs: "runs/run-001/workspace/outputs".to_string(),
        },
        mirror_path: "iteration-2/eval-one/with_skill/".to_string(),
        tool_grant: None,
        artifacts: Vec::new(),
        metrics: RunMetrics {
            duration_ms: None,
            exit_code: None,
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score: None,
        trace: None,
        grade_trace: None,
    };

    let pointer = lookup_exact(&out_dir, &key, &input).expect("the pointer's own key input is still fresh");
    apply_cache_hit(&mut run, &key, &pointer, &report_b)
        .expect("a retired 'partial' capture_status must not turn a cache hit into a miss");

    assert_eq!(run.status, "completed");
    assert!(run.cache.as_ref().unwrap().hit);
}

/// A cache hit must not launder a run that failed its mock's `expect` guard into one
/// that looks clean: grading only ever sees `mock_violations` and `mock-calls.jsonl`,
/// so if a cache hit drops either, the second run of a suite passes where the first
/// failed. Both must survive the hit exactly as the first run left them.
#[test]
fn a_cache_hit_still_carries_the_mock_violation_the_first_run_recorded() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run_with_mock_violation(&report_a, "run-001", "one");

    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    let key = CacheKey::from_input(&input);
    record_completion(&out_dir, &key, &input, &report_a, "run-001").unwrap();

    let report_b = out_dir.join("demo/report-b");
    fs::create_dir_all(report_b.join("runs/run-001/workspace/outputs")).unwrap();
    let mut run = RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: "run-001".to_string(),
        eval_case_id: "one".to_string(),
        eval_slug: "one".to_string(),
        split: EvalSplit::Train,
        scenario_id: ScenarioKind::WithSkill,
        iteration: 2,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 0,
        status: "skipped".to_string(),
        paths: RunPaths {
            workspace: "runs/run-001/workspace".to_string(),
            outputs: "runs/run-001/workspace/outputs".to_string(),
        },
        mirror_path: "iteration-2/eval-one/with_skill/".to_string(),
        tool_grant: None,
        artifacts: Vec::new(),
        metrics: RunMetrics {
            duration_ms: None,
            exit_code: None,
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score: None,
        trace: None,
        grade_trace: None,
    };

    let pointer = lookup_exact(&out_dir, &key, &input).unwrap();
    apply_cache_hit(&mut run, &key, &pointer, &report_b).unwrap();

    assert_eq!(
        run.mock_violations.len(),
        1,
        "a cache hit must reuse the violation the first run logged, not silently clear it"
    );
    assert_eq!(run.mock_violations[0].path.as_str(), "repo");
    let copied_calls = fs::read_to_string(report_b.join("runs/run-001").join(MOCK_CALLS_LOG_NAME)).expect(
        "mock-calls.jsonl is as much a record of what the first run did as transcript.jsonl, \
             and must be copied alongside it",
    );
    assert!(copied_calls.contains("\"repo\""));
}

#[test]
fn cache_hit_carries_read_only_fixture_violations() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run_with(&report_a, "run-001", "one", ScenarioKind::WithSkill, |run| {
        run.read_only_fixture_violations = vec!["evals/one/fixtures/input.txt".to_string()];
        run.failure_kind = Some("read_only_fixture_violation".to_string());
        run.warnings = vec!["fixture evals/one/fixtures/input.txt was modified".to_string()];
    });

    let input = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    let key = CacheKey::from_input(&input);
    record_completion(&out_dir, &key, &input, &report_a, "run-001").unwrap();

    let report_b = out_dir.join("demo/report-b");
    fs::create_dir_all(report_b.join("runs/run-001/workspace/outputs")).unwrap();
    let mut run = RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: "run-001".to_string(),
        eval_case_id: "one".to_string(),
        eval_slug: "one".to_string(),
        split: EvalSplit::Train,
        scenario_id: ScenarioKind::WithSkill,
        iteration: 2,
        model_config_id: "ci-default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        failure_kind: None,
        runner_invocations: 0,
        status: "skipped".to_string(),
        paths: RunPaths {
            workspace: "runs/run-001/workspace".to_string(),
            outputs: "runs/run-001/workspace/outputs".to_string(),
        },
        mirror_path: "iteration-2/eval-one/with_skill/".to_string(),
        tool_grant: None,
        artifacts: Vec::new(),
        metrics: RunMetrics {
            duration_ms: None,
            exit_code: None,
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            cached_tokens: None,
            cost: None,
            transcript: None,
        },
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score: None,
        trace: None,
        grade_trace: None,
    };

    let pointer = lookup_exact(&out_dir, &key, &input).unwrap();
    apply_cache_hit(&mut run, &key, &pointer, &report_b).unwrap();

    assert_eq!(
        run.read_only_fixture_violations,
        vec!["evals/one/fixtures/input.txt".to_string()]
    );
    assert_eq!(run.failure_kind.as_deref(), Some("read_only_fixture_violation"));
    assert_eq!(
        run.warnings,
        vec!["fixture evals/one/fixtures/input.txt was modified".to_string()]
    );
}

#[test]
fn stale_skill_hash_invalidates_exact_cache_entry() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let stored = sample_key_input(ScenarioKind::WithSkill, "sha256:old", FixtureHash::empty().as_str());
    let key = CacheKey::from_input(&stored);
    record_completion(&out_dir, &key, &stored, &report_a, "run-001").unwrap();

    let current = sample_key_input(ScenarioKind::WithSkill, "sha256:new", FixtureHash::empty().as_str());
    assert!(lookup_exact(&out_dir, &key, &current).is_none());
    assert!(!cache_root(&out_dir).join(key.as_str()).exists());
}

#[test]
fn stale_fixture_hash_invalidates_reuse_entry() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let stored = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures-old");
    let key = CacheKey::from_input(&stored);
    record_completion(&out_dir, &key, &stored, &report_a, "run-001").unwrap();

    let current = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixtures-new");
    assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&current)).is_none());
}

#[test]
fn stale_mock_hash_invalidates_reuse_entry() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let stored = CacheKeyInput {
        mock_hash: Some("sha256:mocks-old".to_string()),
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str())
    };
    let key = CacheKey::from_input(&stored);
    record_completion(&out_dir, &key, &stored, &report_a, "run-001").unwrap();

    let current = CacheKeyInput {
        mock_hash: Some("sha256:mocks-new".to_string()),
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str())
    };
    assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&current)).is_none());
}

/// The two arms of a case differ in the scenario and nothing else, so a reuse that
/// forgot it would answer the baseline with the with-skill run and report a delta of
/// zero for a skill that was never exercised.
#[test]
fn reuse_completed_does_not_serve_one_arm_of_a_case_to_the_other() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let with_skill = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    record_completion(
        &out_dir,
        &CacheKey::from_input(&with_skill),
        &with_skill,
        &report_a,
        "run-001",
    )
    .unwrap();

    let without_skill = sample_key_input(
        ScenarioKind::WithoutSkill,
        "sha256:skill",
        FixtureHash::empty().as_str(),
    );
    assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&without_skill)).is_none());

    let pointer = lookup_reuse(&out_dir, &ReuseKeyInput::of(&with_skill)).unwrap();
    assert_eq!(pointer.run_id, "run-001");
}

/// What a run could reach decides what it could do, which is the whole premise of
/// granting tools at all. Serving a run made under one grant to a request made under
/// another measures a skill against tool access it never had, and the report would
/// name the request's grant rather than the one the run really used.
#[test]
fn reuse_completed_does_not_serve_a_run_produced_under_another_tool_grant() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let mut granted = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    granted.tool_grant = Some(ToolGrant::parse(["Read"]).unwrap());
    record_completion(
        &out_dir,
        &CacheKey::from_input(&granted),
        &granted,
        &report_a,
        "run-001",
    )
    .unwrap();

    let pointer = lookup_reuse(&out_dir, &ReuseKeyInput::of(&granted)).unwrap();
    assert_eq!(pointer.run_id, "run-001");

    // A key mismatch evicts the slot, and every grant shares one slot, so each of these
    // is asked of its own recording rather than of the leftovers of the last question.
    for (label, grant) in [
        (
            "a wider grant is a different run",
            Some(ToolGrant::parse(["Read", "Write"]).unwrap()),
        ),
        ("no grant at all is a different run again", None),
    ] {
        let temp = tempdir().unwrap();
        let out_dir = temp.path().join("out");
        let report = out_dir.join("demo/report-a");
        fs::create_dir_all(&report).unwrap();
        write_completed_run(&report, "run-001", "one", ScenarioKind::WithSkill);
        record_completion(&out_dir, &CacheKey::from_input(&granted), &granted, &report, "run-001").unwrap();

        let mut other = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
        other.tool_grant = grant;
        assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&other)).is_none(), "{label}");
    }
}

/// What `--reuse-completed` is for: the run that answered this case under a different
/// model config is still the run that answered this case.
#[test]
fn reuse_completed_serves_a_run_produced_under_another_model_config() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let stored = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    record_completion(&out_dir, &CacheKey::from_input(&stored), &stored, &report_a, "run-001").unwrap();

    let other_config = CacheKeyInput {
        model_config: "hand-tuned".to_string(),
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str())
    };
    assert!(lookup_exact(&out_dir, &CacheKey::from_input(&other_config), &other_config).is_none());

    let pointer = lookup_reuse(&out_dir, &ReuseKeyInput::of(&other_config)).unwrap();
    assert_eq!(pointer.run_id, "run-001");
}

/// `--reuse-completed` forgets the model on purpose, so the run it serves can have been
/// produced under a different one. Leaving the requesting run's own model in place would
/// have the report name a model that never produced the artifacts underneath it, which
/// is the mislabel the field was added to prevent.
#[test]
fn a_reused_run_reports_the_model_that_produced_it_not_the_one_that_asked() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run_with(&report_a, "run-001", "one", ScenarioKind::WithSkill, |run| {
        run.runner_model = Some(ModelName::parse("opus").unwrap());
    });

    let stored = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    record_completion(&out_dir, &CacheKey::from_input(&stored), &stored, &report_a, "run-001").unwrap();

    let asked = CacheKeyInput {
        runner_model: Some("sonnet".to_string()),
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str())
    };
    let key = CacheKey::from_input(&asked);
    let pointer = lookup_reuse(&out_dir, &ReuseKeyInput::of(&asked)).unwrap();

    let report_b = out_dir.join("demo/report-b");
    fs::create_dir_all(report_b.join("runs/run-002/workspace/outputs")).unwrap();
    let mut run = sample_run_record("run-002", "one", ScenarioKind::WithSkill);
    run.runner_model = Some(ModelName::parse("sonnet").unwrap());

    apply_cache_hit(&mut run, &key, &pointer, &report_b).unwrap();

    assert_eq!(
        run.runner_model.as_ref().map(ModelName::as_str),
        Some("opus"),
        "a reused run answers for the model it ran under, not the one the request named"
    );
}

/// A second attempt is a second sample of the same cell, which is the whole reason
/// for asking for it. Serving it the first attempt's run turns the spread across
/// attempts into zero and the mean into the first draw.
#[test]
fn a_second_draw_of_a_cell_is_a_different_run() {
    let first = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture");
    let second = CacheKeyInput {
        attempt: 2,
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", "sha256:fixture")
    };

    assert_ne!(
        CacheKey::from_input(&first),
        CacheKey::from_input(&second),
        "which draw of the cell a run is belongs in its identity"
    );
}

/// `--reuse-completed` forgets the model config and the runner. It cannot forget the
/// draw, for the same reason the exact key cannot.
#[test]
fn reuse_completed_does_not_serve_one_attempt_to_the_next() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");
    let report_a = out_dir.join("demo/report-a");
    fs::create_dir_all(&report_a).unwrap();
    write_completed_run(&report_a, "run-001", "one", ScenarioKind::WithSkill);

    let first = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    record_completion(&out_dir, &CacheKey::from_input(&first), &first, &report_a, "run-001").unwrap();

    let second = CacheKeyInput {
        attempt: 2,
        ..sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str())
    };
    assert!(lookup_reuse(&out_dir, &ReuseKeyInput::of(&second)).is_none());

    let report_b = out_dir.join("demo/report-b");
    fs::create_dir_all(&report_b).unwrap();
    write_completed_run(&report_b, "run-002", "one", ScenarioKind::WithSkill);
    record_completion(&out_dir, &CacheKey::from_input(&second), &second, &report_b, "run-002").unwrap();

    assert_eq!(
        lookup_reuse(&out_dir, &ReuseKeyInput::of(&first)).unwrap().run_id,
        "run-001",
        "one slot per draw, so a later draw does not evict an earlier one"
    );
    assert_eq!(
        lookup_reuse(&out_dir, &ReuseKeyInput::of(&second)).unwrap().run_id,
        "run-002"
    );
}

/// One slot per case would let each arm evict the other, so a comparison would
/// re-execute every run it was told to reuse.
#[test]
fn each_arm_of_a_case_keeps_its_own_completed_run() {
    let temp = tempdir().unwrap();
    let out_dir = temp.path().join("out");

    let with_report = out_dir.join("demo/report-with");
    fs::create_dir_all(&with_report).unwrap();
    write_completed_run(&with_report, "run-001", "one", ScenarioKind::WithSkill);
    let with_skill = sample_key_input(ScenarioKind::WithSkill, "sha256:skill", FixtureHash::empty().as_str());
    record_completion(
        &out_dir,
        &CacheKey::from_input(&with_skill),
        &with_skill,
        &with_report,
        "run-001",
    )
    .unwrap();

    let without_report = out_dir.join("demo/report-without");
    fs::create_dir_all(&without_report).unwrap();
    write_completed_run(&without_report, "run-002", "one", ScenarioKind::WithoutSkill);
    let without_skill = sample_key_input(
        ScenarioKind::WithoutSkill,
        "sha256:skill",
        FixtureHash::empty().as_str(),
    );
    record_completion(
        &out_dir,
        &CacheKey::from_input(&without_skill),
        &without_skill,
        &without_report,
        "run-002",
    )
    .unwrap();

    assert_eq!(
        lookup_reuse(&out_dir, &ReuseKeyInput::of(&with_skill)).unwrap().run_id,
        "run-001"
    );
    assert_eq!(
        lookup_reuse(&out_dir, &ReuseKeyInput::of(&without_skill))
            .unwrap()
            .run_id,
        "run-002"
    );
}
