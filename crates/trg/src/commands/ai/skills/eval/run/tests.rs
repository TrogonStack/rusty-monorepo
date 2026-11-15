use super::*;
use crate::agentskills::evals::EvalSplit;
use crate::agentskills::proportion::ScoredRate;
use crate::agentskills::redact::redact_transcript_bytes;
use crate::agentskills::report::{
    DimensionsSection, ProducerSection, ReportDocument, ReportSection, RunMetrics, RunPaths, ScenarioKind,
    ScenarioSummary, SuiteSection, SummariesSection,
};
use crate::agentskills::transcript::{ToolName, ToolVisibility, TranscriptFormat, WorkspaceBoundary, WorkspaceEscape};
use clap::Parser;
use std::path::{Path, PathBuf};

fn case_run(
    run_id: &str,
    eval_case_id: &str,
    scenario: ScenarioKind,
    attempt: u32,
    case_score: Option<f64>,
) -> RunRecord {
    RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: run_id.to_string(),
        eval_case_id: eval_case_id.to_string(),
        eval_slug: eval_case_id.to_string(),
        split: EvalSplit::Train,
        scenario_id: scenario,
        iteration: 1,
        model_config_id: "default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt,
        status: "completed".to_string(),
        runner_invocations: 1,
        failure_kind: None,
        paths: RunPaths {
            workspace: format!("runs/{run_id}/workspace"),
            outputs: format!("runs/{run_id}/workspace/outputs"),
        },
        mirror_path: format!("iteration-1/{eval_case_id}/{}/", scenario.as_str()),
        tool_grant: None,
        artifacts: Vec::new(),
        metrics: RunMetrics::default(),
        cache: None,
        skill_integrity: None,
        read_only_fixture_violations: Vec::new(),
        warnings: Vec::new(),
        mock_violations: Vec::new(),
        case_score,
        trace: None,
        grade_trace: None,
    }
}

fn bundle_with_runs(runs: Vec<RunRecord>, scenarios: &[ScenarioKind]) -> ReportBundle {
    ReportBundle {
        report_id: "report-1".to_string(),
        skill_name: "demo-skill".to_string(),
        workspace_dirs: Vec::new(),
        document: ReportDocument {
            report: ReportSection {
                id: "report-1".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                iteration: 1,
                producer: ProducerSection {
                    name: "trg".to_string(),
                    version: "0.0.0".to_string(),
                },
                runner: None,
                runner_binary: None,
                runner_version: None,
                environment: Default::default(),
                permission: Default::default(),
                allowed_tools: None,
                telemetry_forwarding: TelemetryForwarding::Off,
                ci: None,
                trace: None,
            },
            suite: SuiteSection {
                skill_name: "demo-skill".to_string(),
                skill_path: "demo-skill".to_string(),
                skill_hash: "sha256:abc".to_string(),
                evals_path: "demo-skill/evals/evals.json".to_string(),
                evals_hash: "sha256:def".to_string(),
                eval_dir: Default::default(),
                old_skill_path: None,
                old_skill_hash: None,
                case_selection: None,
            },
            dimensions: DimensionsSection {
                eval_cases: Vec::new(),
                assertions: Vec::new(),
                skill_revisions: Vec::new(),
                model_configs: Vec::new(),
                scenarios: Vec::new(),
                grading_strategies: Vec::new(),
            },
            runs,
            assertion_results: Vec::new(),
            summaries: SummariesSection {
                by_scenario: scenarios
                    .iter()
                    .map(|scenario| ScenarioSummary {
                        scenario_id: *scenario,
                        total_runs: 0,
                        passed_runs: 0,
                        skipped_runs: 0,
                        failed_runs: 0,
                        pass_at_k: ScoredRate::default(),
                        pass_pow_k: ScoredRate::default(),
                    })
                    .collect(),
                human_feedback: None,
            },
            improvement_feedback: Vec::new(),
            comparisons: Vec::new(),
            iteration_summary: None,
            budget: None,
        },
    }
}

#[test]
fn rebuild_summaries_separates_pass_at_k_from_pass_pow_k() {
    let runs = vec![
        case_run("run-001", "alpha", ScenarioKind::WithSkill, 1, Some(1.0)),
        case_run("run-002", "alpha", ScenarioKind::WithSkill, 2, Some(0.0)),
        case_run("run-003", "beta", ScenarioKind::WithSkill, 1, Some(1.0)),
        case_run("run-004", "beta", ScenarioKind::WithSkill, 2, Some(1.0)),
    ];
    let mut bundle = bundle_with_runs(runs, &[ScenarioKind::WithSkill]);

    recompute_scenario_summaries(&mut bundle.document);

    let summary = &bundle.document.summaries.by_scenario[0];
    assert_eq!(summary.pass_at_k.successes, 2);
    assert_eq!(summary.pass_at_k.trials, 2);
    assert_eq!(summary.pass_at_k.rate, Some(1.0));
    assert_eq!(summary.pass_pow_k.successes, 1);
    assert_eq!(summary.pass_pow_k.trials, 2);
    assert_eq!(summary.pass_pow_k.rate, Some(0.5));
}

#[test]
fn rebuild_summaries_ignores_ungraded_attempts_for_the_k_rates() {
    let runs = vec![case_run("run-001", "alpha", ScenarioKind::WithSkill, 1, None)];
    let mut bundle = bundle_with_runs(runs, &[ScenarioKind::WithSkill]);

    recompute_scenario_summaries(&mut bundle.document);

    let summary = &bundle.document.summaries.by_scenario[0];
    assert_eq!(summary.pass_at_k.trials, 0);
    assert_eq!(summary.pass_at_k.rate, None);
    assert_eq!(summary.pass_pow_k.trials, 0);
    assert_eq!(summary.pass_pow_k.rate, None);
}

#[test]
fn a_path_reached_outside_the_workspace_becomes_a_run_warning() {
    let mut transcript = NormalizedTranscript::new("cursor-agent", ToolVisibility::Observed, Vec::new());
    assert_eq!(workspace_escape_warning(&transcript), None);

    transcript.workspace_escapes = vec![WorkspaceEscape {
        tool: ToolName::new("read").unwrap(),
        path: "/host/plugins/cache/SKILL.md".to_string(),
    }];

    let warning = workspace_escape_warning(&transcript).unwrap();
    assert!(warning.contains("cursor-agent"), "{warning}");
    assert!(warning.contains("/host/plugins/cache/SKILL.md"), "{warning}");
    assert!(warning.contains("read"), "{warning}");
}

#[test]
fn a_run_that_stayed_in_its_workspace_is_not_warned_about() {
    let workspace = tempfile::tempdir().unwrap();
    let stdout = br#"{"type":"tool_call","subtype":"started","tool_call":{"readToolCall":{"args":{"path":"outputs/summary.md"}}}}
"#;
    let transcript = TranscriptFormat::CursorStreamJson.normalize(
        "cursor-agent",
        &redact_transcript_bytes(stdout),
        &WorkspaceBoundary::at(workspace.path()),
    );
    assert_eq!(workspace_escape_warning(&transcript), None);
}

fn write_fixture_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("fixture-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: fixture-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "fixture-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    },
                    {
                        "id": "two",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "graders": [{ "type": "contains", "text": "checks second" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

#[test]
fn run_command_builds_expected_bundle_layout() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let status = RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        runner: None,
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: false,
        iteration: Some(1),
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,

        require_graders: false,
        lint_evals: false,

        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs {
            strict_ci: false,
            fail_on_runner_failure: false,
            fail_on_failed_assertions: false,
            fail_on_missing_grading: false,
            fail_on_pass_rate_regression: false,
            fail_on_token_regression: false,
            fail_on_duration_regression: false,
            min_pass_rate: None,
            min_case_score: None,
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
            max_duration_ms: None,
            baseline_report_dir: None,
        },
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::Success);

    let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("fixture-skill"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(report_dirs.len(), 1);

    let report_dir = &report_dirs[0];
    let report_json_path = report_dir.join("report.json");
    assert!(report_json_path.is_file());

    let report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_json_path).unwrap()).unwrap();
    assert!(report
        .pointer("/suite/skill_hash")
        .and_then(|value| value.as_str())
        .unwrap()
        .starts_with("sha256:"));
    assert!(report
        .pointer("/suite/evals_hash")
        .and_then(|value| value.as_str())
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(report.get("runs").and_then(|value| value.as_array()).unwrap().len(), 4);
    assert_eq!(
        report.pointer("/report/iteration").and_then(|value| value.as_u64()),
        Some(1)
    );
    let first_run = report.get("runs").and_then(|value| value.as_array()).unwrap()[0].clone();
    assert_eq!(first_run.get("eval_slug").and_then(|value| value.as_str()), Some("one"));
    assert_eq!(first_run.get("iteration").and_then(|value| value.as_u64()), Some(1));
    assert_eq!(
        first_run.get("mirror_path").and_then(|value| value.as_str()),
        Some("iteration-1/eval-one/with_skill/")
    );

    let workspace_dir = report_dir.join("runs/run-001/workspace");
    assert!(workspace_dir.is_dir());
    assert!(workspace_dir.join("outputs").is_dir());
    assert_eq!(std::fs::read_dir(&workspace_dir).unwrap().count(), 1);

    let mirror = report_dir.join("iteration-1/eval-one/with_skill");
    assert!(mirror.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(&mirror).unwrap(),
        PathBuf::from("../../runs/run-001/workspace")
    );
    assert!(report_dir.join("iteration-1/benchmark.json").is_file());
    assert!(report_dir.join("iteration-1/alias-index.json").is_file());
}

fn run_with_scenario_selection(
    skill_dir: PathBuf,
    out_dir: PathBuf,
    scenario: Vec<ScenarioKind>,
    reuse_completed: bool,
) -> (ExitCode, PathBuf) {
    let status = RunArgs {
        skill_dir,
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario,
        runner: None,
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: false,
        iteration: Some(1),
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
    .handle(&crate::fs::RealFS);

    let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("fixture-skill"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let report_dir = report_dirs.into_iter().next().unwrap();
    (status, report_dir)
}

fn scenario_ids(report_dir: &Path) -> Vec<String> {
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    let mut ids: Vec<String> = report["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["scenario_id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[test]
fn a_pass_that_names_no_scenario_runs_both_arms() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (status, report_dir) = run_with_scenario_selection(skill_dir, out_dir, Vec::new(), false);
    assert_eq!(status, ExitCode::Success);
    assert_eq!(
        scenario_ids(&report_dir),
        vec!["with_skill".to_string(), "without_skill".to_string()]
    );
}

#[test]
fn a_pass_that_names_one_scenario_runs_only_that_one() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (status, report_dir) = run_with_scenario_selection(skill_dir, out_dir, vec![ScenarioKind::WithoutSkill], false);
    assert_eq!(status, ExitCode::Success);
    assert_eq!(scenario_ids(&report_dir), vec!["without_skill".to_string()]);
}

#[test]
fn a_pass_that_names_no_scenario_under_reuse_completed_runs_the_with_skill_arm_alone() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (status, report_dir) = run_with_scenario_selection(skill_dir, out_dir, Vec::new(), true);
    assert_eq!(status, ExitCode::Success);
    assert_eq!(scenario_ids(&report_dir), vec!["with_skill".to_string()]);
}

fn write_named_skill(root: &Path, dir_name: &str, skill_name: &str) -> PathBuf {
    let skill_dir = root.join(dir_name);
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {skill_name}\ndescription: fixture\n---\n"),
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        format!(
            r#"{{
                "skill_name": "{skill_name}",
                "evals": [
                    {{
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{{ "type": "contains", "text": "checks first" }}]
                    }}
                ]
            }}"#
        ),
    )
    .unwrap();
    skill_dir
}

#[test]
fn run_command_rejects_old_skill_name_mismatch_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_named_skill(temp.path(), "fixture-skill", "fixture-skill");
    let old_skill_dir = write_named_skill(temp.path(), "other-skill", "other-skill");
    let out_dir = temp.path().join("artifacts");

    let status = RunArgs {
        skill_dir,
        out_dir,
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::OldSkill],
        runner: None,
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: false,
        iteration: None,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: Some(old_skill_dir),
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,

        require_graders: false,
        lint_evals: false,

        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::InfrastructureFailure);
}

#[test]
fn run_command_allows_old_skill_name_mismatch_when_flag_set() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_named_skill(temp.path(), "fixture-skill", "fixture-skill");
    let old_skill_dir = write_named_skill(temp.path(), "other-skill", "other-skill");
    let out_dir = temp.path().join("artifacts");

    let status = RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::OldSkill],
        runner: None,
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: false,
        iteration: None,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: Some(old_skill_dir),
        allow_skill_name_mismatch: true,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,

        require_graders: false,
        lint_evals: false,

        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::Success);

    let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("fixture-skill"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(report_dirs.len(), 1);

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_dirs[0].join("report.json")).unwrap()).unwrap();
    assert!(report.pointer("/suite/old_skill_path").is_some());
    assert!(report.pointer("/suite/old_skill_hash").is_some());
    assert_eq!(
        report
            .pointer("/dimensions/skill_revisions/1/id")
            .and_then(|value| value.as_str()),
        Some("old")
    );
}

#[test]
fn run_command_requires_old_skill_dir_for_old_skill_scenario() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_fixture_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let status = RunArgs {
        skill_dir,
        out_dir,
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::OldSkill],
        runner: None,
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: false,
        iteration: None,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,

        require_graders: false,
        lint_evals: false,

        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::InfrastructureFailure);
}

fn write_cacheable_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("cache-skill");
    std::fs::create_dir_all(skill_dir.join("evals/one/fixtures")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: cache-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "cache-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v1").unwrap();
    skill_dir
}

fn run_with_fake_runner(args: RunArgs) -> PathBuf {
    let (status, report_dir) = run_with_fake_runner_reporting_status(args);
    assert_eq!(status, ExitCode::Success, "eval run failed");
    report_dir
}

/// Same as `run_with_fake_runner`, without insisting the pass exited clean, so a test
/// can look at a report a budget or a CI gate stopped from being one.
fn run_with_fake_runner_reporting_status(args: RunArgs) -> (ExitCode, PathBuf) {
    super::fake_runner::enable();
    let out_dir = args.out_dir.clone();
    let skill_name = std::fs::read_to_string(args.skill_dir.join("SKILL.md"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("name:"))
        .map(str::trim)
        .unwrap()
        .to_string();
    let status = args.handle(&crate::fs::RealFS);
    super::fake_runner::disable();

    let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join(skill_name))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let report_dir = report_dirs
        .into_iter()
        .max_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        })
        .expect("report dir");
    (status, report_dir)
}

fn read_report(report_dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap()
}

fn first_run_duration(report_dir: &Path) -> u64 {
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    report["runs"][0]["metrics"]["duration_ms"]
        .as_u64()
        .expect("duration_ms")
}

fn write_skill_where_one_case_picks_its_own_model(root: &Path) -> PathBuf {
    let skill_dir = root.join("mixed-model-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: mixed-model-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "mixed-model-skill",
                "evals": [
                    {
                        "id": "takes-the-operators-model",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    },
                    {
                        "id": "picks-its-own-model",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "graders": [{ "type": "contains", "text": "checks second" }],
                        "model": "case-chosen-model"
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

fn mixed_model_run_args(skill_dir: &Path, out_dir: &Path, operator_model: &str, cached: bool) -> RunArgs {
    RunArgs {
        skill_dir: skill_dir.to_path_buf(),
        out_dir: out_dir.to_path_buf(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: Some(ModelName::parse(operator_model).unwrap()),
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: !cached,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
}

/// `dimensions.model_configs` carries one label for the whole report, so a suite whose
/// cases pick their own models would otherwise read as though every run used the
/// operator's, which is a claim the report cannot support.
#[test]
fn a_report_says_which_model_each_run_actually_used() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill_where_one_case_picks_its_own_model(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(mixed_model_run_args(&skill_dir, &out_dir, "operator-model", false));

    let report = read_report(&report_dir);
    let model_of = |case: &str| {
        report["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["eval_case_id"] == case)
            .unwrap_or_else(|| panic!("no run for {case}"))["runner_model"]
            .as_str()
            .map(str::to_string)
    };

    assert_eq!(model_of("takes-the-operators-model").as_deref(), Some("operator-model"));
    assert_eq!(model_of("picks-its-own-model").as_deref(), Some("case-chosen-model"));
}

/// The cache key has to be cut from the model a run executes under, not the one asked
/// for at the CLI: keyed on the CLI's, a case that pins its own model would miss on
/// every pass that changed `--runner-model` even though nothing about that run changed,
/// and a suite of pinned cases would never cache at all.
#[test]
fn changing_the_operators_model_leaves_a_case_that_pinned_its_own_on_cache() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_skill_where_one_case_picks_its_own_model(temp.path());
    let out_dir = temp.path().join("artifacts");

    run_with_fake_runner(mixed_model_run_args(&skill_dir, &out_dir, "operator-model", true));
    let second = run_with_fake_runner(mixed_model_run_args(
        &skill_dir,
        &out_dir,
        "a-different-operator-model",
        true,
    ));

    let report = read_report(&second);
    let hit_for = |case: &str| {
        report["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["eval_case_id"] == case)
            .unwrap_or_else(|| panic!("no run for {case}"))["cache"]["hit"]
            .as_bool()
    };

    assert_eq!(
        hit_for("picks-its-own-model"),
        Some(true),
        "the pinned case executed under the same model both times"
    );
    assert_ne!(
        hit_for("takes-the-operators-model"),
        Some(true),
        "the unpinned case executed under a different model the second time"
    );
}

#[test]
fn consecutive_runs_with_identical_inputs_hit_cache() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let base = || RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    };

    let first_report = run_with_fake_runner(base());
    assert_eq!(first_run_duration(&first_report), 100);

    let second_report = run_with_fake_runner(base());
    assert_eq!(first_run_duration(&second_report), 100);

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
    let cache = &report["runs"][0]["cache"];
    assert_eq!(cache["hit"], true);
    assert_eq!(cache["source_run_id"], "run-001");
    assert!(cache["key"].as_str().unwrap().len() == 64);
}

#[test]
fn changing_skill_md_invalidates_cache() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let base = || RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    };

    run_with_fake_runner(base());
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: cache-skill\ndescription: changed\n---\n",
    )
    .unwrap();
    let second_report = run_with_fake_runner(base());
    assert_eq!(first_run_duration(&second_report), 200);

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
    assert!(report["runs"][0]["cache"].is_null());
}

#[test]
fn changing_fixture_invalidates_cache() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let base = || RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    };

    run_with_fake_runner(base());
    std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v2").unwrap();
    let second_report = run_with_fake_runner(base());
    assert_eq!(first_run_duration(&second_report), 200);
}

#[test]
fn no_cache_disables_cache_hit() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let base = |no_cache: bool| RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    };

    run_with_fake_runner(base(false));
    let second_report = run_with_fake_runner(base(true));
    assert_eq!(first_run_duration(&second_report), 200);
}

/// `--attempts` buys samples, and a sample the cache copied from the draw before it
/// is not one. A cell is identified by its draw as well, so the second attempt
/// executes and the second invocation of the same command still reuses both.
#[test]
fn each_attempt_of_a_cell_executes_rather_than_copying_the_draw_before_it() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let base = || RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::parse(2).unwrap()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    };

    let first_report = read_report(&run_with_fake_runner(base()));
    assert_eq!(first_report["runs"][0]["metrics"]["duration_ms"], 100);
    assert!(
        first_report["runs"][1]["cache"].is_null(),
        "the second draw must be executed, not copied from the first"
    );
    assert_eq!(
        first_report["runs"][1]["metrics"]["duration_ms"], 200,
        "the second draw is its own run"
    );

    let second_report = read_report(&run_with_fake_runner(base()));
    assert_eq!(second_report["runs"][0]["cache"]["source_run_id"], "run-001");
    assert_eq!(
        second_report["runs"][1]["cache"]["source_run_id"], "run-002",
        "each draw is served the run recorded for that draw"
    );
    assert_eq!(second_report["runs"][1]["metrics"]["duration_ms"], 200);
}

/// A pass costs a model call per case, per scenario and per attempt, so the lanes are
/// the difference between minutes and hours. What they may not change is the report:
/// a reader compares runs by position across passes, so the order has to come from
/// the suite rather than from which lane finished first.
#[test]
fn lanes_execute_runs_at_once_and_still_report_them_in_suite_order() {
    super::fake_runner::reset();
    super::fake_runner::expect_lanes(2);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::parse(2).unwrap(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }));

    assert_eq!(
        super::fake_runner::peak_lanes(),
        2,
        "both runs have to be in flight at once for the lanes to buy anything"
    );
    assert_eq!(report["runs"][0]["eval_case_id"], "one");
    assert_eq!(report["runs"][1]["eval_case_id"], "two");
    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(report["runs"][1]["status"], "completed");
}

/// Tamper detection exists to say that the skill a run was scored on is not the skill
/// it was handed. Lanes read one directory, so a rewrite lands on whichever runs were
/// still reading it, and charging it to the run whose window happened to contain it
/// would name a culprit the check cannot identify.
#[test]
fn a_skill_rewritten_during_a_concurrent_pass_is_not_charged_to_one_run() {
    super::fake_runner::reset();
    super::fake_runner::expect_lanes(2);
    super::fake_runner::tamper_on_case("one");
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        concurrency: RunConcurrency::parse(2).unwrap(),
        ..base_run_args(&skill_dir, &out_dir)
    }));

    for index in 0..2 {
        let run = &report["runs"][index];
        assert_eq!(
            run["skill_integrity"]["tampered"], true,
            "a rewritten skill was read by every lane still running"
        );
        assert!(
            run["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning.as_str().unwrap_or_default().contains("no single run answers")),
            "the report has to say the rewrite cannot be pinned on this run"
        );
    }
}

/// One run at a time is the case where the check can name the run, so it still does,
/// and says nothing about the runs that came before it.
#[test]
fn a_skill_rewritten_by_a_serial_run_is_charged_to_that_run() {
    super::fake_runner::reset();
    super::fake_runner::tamper_on_case("two");
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(base_run_args(&skill_dir, &out_dir)));

    assert_eq!(report["runs"][0]["eval_case_id"], "one");
    assert_eq!(
        report["runs"][0]["skill_integrity"]["tampered"], false,
        "the run that ran before the rewrite did not see it"
    );
    assert_eq!(report["runs"][1]["skill_integrity"]["tampered"], true);
    assert!(
        report["runs"][1]["warnings"]
            .as_array()
            .is_none_or(|warnings| warnings.is_empty()),
        "a serial pass can name the run, so there is nothing to qualify"
    );
}

/// The check exists to say whether the skill a run was scored on is still the skill it
/// was handed. A directory that cannot be read back answers neither way, and calling it
/// unchanged would hide the deletion the check is there to catch.
#[test]
fn a_skill_that_cannot_be_read_after_a_concurrent_pass_is_not_reported_as_unchanged() {
    super::fake_runner::reset();
    super::fake_runner::expect_lanes(2);
    super::fake_runner::remove_skill_on_case("two");
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        concurrency: RunConcurrency::parse(2).unwrap(),
        ..base_run_args(&skill_dir, &out_dir)
    }));

    for index in 0..2 {
        let run = &report["runs"][index];
        assert_eq!(
            run["skill_integrity"]["tampered"], true,
            "a skill directory that is gone is not a skill directory that was left alone"
        );
        assert!(
            run["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning.as_str().unwrap_or_default().contains("could not be read")),
            "the report has to say why nothing answers for the directory"
        );
    }
}

/// One run at a time reads the directory in its own window, and the same rule holds
/// there: a read that failed is not a comparison that passed.
#[test]
fn a_skill_that_cannot_be_read_after_a_serial_run_is_not_reported_as_unchanged() {
    super::fake_runner::reset();
    super::fake_runner::remove_skill_on_case("one");
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(base_run_args(&skill_dir, &out_dir)));

    assert_eq!(report["runs"][0]["eval_case_id"], "one");
    assert_eq!(report["runs"][0]["skill_integrity"]["tampered"], true);
    assert!(
        report["runs"][0]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning.as_str().unwrap_or_default().contains("could not be read")),
        "the run whose window lost the directory has to say so"
    );
}

fn base_run_args(skill_dir: &Path, out_dir: &Path) -> RunArgs {
    RunArgs {
        skill_dir: skill_dir.to_path_buf(),
        out_dir: out_dir.to_path_buf(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::ClaudeCode),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: true,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
}

/// The default decides what almost every pass measures, and a pass that draws each
/// cell once reports a number with no spread attached: any delta it prints against
/// another pass, or against a baseline, is as likely to be the agent's own variance
/// as it is to be the skill.
#[test]
fn a_pass_that_says_nothing_about_attempts_still_samples_each_cell() {
    let cli = crate::cli::Cli::try_parse_from([
        "trg",
        "ai",
        "skills",
        "eval",
        "run",
        "--skill-dir",
        "skill",
        "--out-dir",
        "out",
    ])
    .expect("parses");

    let crate::commands::Commands::Ai {
        command: crate::commands::ai::AiCommands::Skills { command },
    } = cli.command
    else {
        panic!("expected an ai skills command");
    };
    let crate::commands::ai::skills::SkillsCommands::Eval(eval) = command else {
        panic!("expected an eval command");
    };
    let super::super::EvalCommands::Run(args) = eval.command else {
        panic!("expected an eval run command");
    };

    assert_eq!(
        args.attempts, None,
        "an unset flag has to stay distinguishable from a count the operator typed"
    );

    assert!(
        !AttemptCount::recommended().is_single(),
        "a default of one draw reports a sample of size one as a measurement"
    );
}

/// A cell nobody draws is a row the report cannot fill in, so the count is refused at
/// the boundary rather than quietly raised to one later.
#[test]
fn an_attempt_count_of_zero_is_refused_at_the_command_line() {
    assert!(crate::cli::Cli::try_parse_from([
        "trg",
        "ai",
        "skills",
        "eval",
        "run",
        "--skill-dir",
        "skill",
        "--out-dir",
        "out",
        "--attempts",
        "0",
    ])
    .is_err());
}

fn write_two_case_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("lane-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: lane-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "lane-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    },
                    {
                        "id": "two",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "graders": [{ "type": "contains", "text": "checks second" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

fn write_conversation_seeding_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("resuming-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: resuming-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "resuming-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    },
                    {
                        "id": "resumes",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "graders": [{ "type": "contains", "text": "checks second" }],
                        "conversation_history": "history/prior-turn.json"
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

fn write_system_prompt_appendix_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("steered-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: steered-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "steered-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    },
                    {
                        "id": "steered",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "graders": [{ "type": "contains", "text": "checks second" }],
                        "append_system_prompt": "Answer in British English."
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

fn write_tool_grant_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("gated-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: gated-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "gated-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    },
                    {
                        "id": "disjoint",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "graders": [{ "type": "contains", "text": "checks second" }],
                        "allowed_tools": ["Bash"]
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

/// The operator granted `Read` and the case narrows to `Bash`; the two share nothing,
/// so the effective grant is empty. `--allowedTools` has no documented meaning for an
/// empty value, so trg has to refuse the run rather than pass one through and let the
/// harness decide for itself whether that denies every tool or grants them all.
#[test]
fn a_disjoint_case_grant_is_refused_rather_than_sent_as_an_empty_allowlist() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_tool_grant_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        allowed_tools: vec!["Read".to_string()],
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(report["runs"][0]["status"], "completed");

    assert_eq!(report["runs"][1]["status"], "skipped");
    assert_eq!(report["runs"][1]["failure_kind"], "unsupported");
    assert_eq!(report["runs"][1]["tool_grant"], serde_json::Value::Null);
    assert_eq!(
        super::fake_runner::invocations(),
        1,
        "the only invocation must be the case whose grant was not disjoint"
    );
    assert!(
        report["runs"][1]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap_or_default()
                .contains("has no way to express denying every tool")),
        "the report has to say why the run did not start"
    );
}

#[test]
fn reuse_completed_serves_the_same_arm_produced_under_another_model_config() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    run_with_fake_runner(RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    let second_report = run_with_fake_runner(RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "other-model".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: Some(ModelName::parse("gpt-test").unwrap()),
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: true,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
    let cache = &report["runs"][0]["cache"];
    assert_eq!(cache["hit"], true);
    assert_eq!(cache["source_run_id"], "run-001");
    assert_eq!(first_run_duration(&second_report), 100);
}

/// The two arms of a case differ in the scenario and nothing else, so answering the
/// baseline with the with-skill run would compare a run against itself.
#[test]
fn reuse_completed_does_not_serve_the_with_skill_run_to_the_baseline() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    run_with_fake_runner(RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    let second_report = run_with_fake_runner(RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "other-model".to_string(),
        scenario: vec![ScenarioKind::WithoutSkill],
        runner: Some(Runner::Codex),
        runner_model: Some(ModelName::parse("gpt-test").unwrap()),
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: true,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(second_report.join("report.json")).unwrap()).unwrap();
    assert!(
        report["runs"][0]["cache"].is_null(),
        "the baseline arm has to be executed rather than answered with the run that had the skill"
    );
}

#[test]
fn reuse_completed_rejects_multiple_scenarios() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_cacheable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let status = RunArgs {
        skill_dir,
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        runner: None,
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: false,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: true,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
    .handle(&crate::fs::RealFS);

    assert_eq!(status, ExitCode::InfrastructureFailure);
    assert!(!out_dir.exists());
}

fn write_timeout_skill(root: &Path, timeout_secs: Option<u32>) -> PathBuf {
    let skill_dir = root.join("timeout-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: timeout-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    let timeout_field = timeout_secs
        .map(|secs| format!(",\n                        \"timeout_secs\": {secs}"))
        .unwrap_or_default();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        format!(
            r#"{{
                "skill_name": "timeout-skill",
                "evals": [
                    {{
                        "id": "one",
                        "prompt": "first prompt long enough",
                        "expected_output": "first output long",
                        "graders": [{{ "type": "contains", "text": "checks first" }}]{timeout_field}
                    }}
                ]
            }}"#
        ),
    )
    .unwrap();
    skill_dir
}

#[test]
fn per_eval_timeout_overrides_global_timeout_in_runner_request() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_timeout_skill(temp.path(), Some(42));
    let out_dir = temp.path().join("artifacts");

    run_with_fake_runner(RunArgs {
        skill_dir,
        out_dir,
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: Some(99),
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: true,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    assert_eq!(super::fake_runner::last_timeout_secs(), Some(42));
}

fn write_expected_output_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("expected-output-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: expected-output-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "expected-output-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt long enough",
                        "expected_output": "first output long",
                        "graders": [{ "type": "contains", "text": "checks first" }],
                        "expected_output_files": ["report.md", "missing.md"]
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

#[test]
fn missing_expected_output_files_add_run_warnings_without_failing() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_expected_output_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        skill_dir,
        out_dir,
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: true,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["runs"][0]["status"], "completed");
    let warnings = report["runs"][0]["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 2);
    assert!(warnings
        .iter()
        .any(|warning| warning.as_str().unwrap().contains("report.md")));
    assert!(warnings
        .iter()
        .any(|warning| warning.as_str().unwrap().contains("missing.md")));
}

fn write_gradable_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("gradable-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: gradable-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "gradable-skill",
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

fn write_two_case_gradable_skill(root: &Path) -> PathBuf {
    let skill_dir = root.join("gradable-pair");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: gradable-pair\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "gradable-pair",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "graders": [{ "type": "file_exists", "path": "out.json" }]
                    },
                    {
                        "id": "two",
                        "prompt": "create output again",
                        "expected_output": "done",
                        "graders": [{ "type": "file_exists", "path": "out.json" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    skill_dir
}

fn count_grading_json(report_dir: &Path) -> usize {
    let runs_dir = report_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("grading.json").is_file())
        .count()
}

fn find_grading_json(report_dir: &Path) -> Option<PathBuf> {
    let runs_dir = report_dir.join("runs");
    if !runs_dir.is_dir() {
        return None;
    }
    for entry in std::fs::read_dir(&runs_dir).ok()? {
        let run_dir = entry.ok()?.path();
        let grading = run_dir.join("grading.json");
        if grading.is_file() {
            return Some(grading);
        }
    }
    None
}

#[test]
fn run_with_grade_and_benchmark_writes_all_artifacts() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_gradable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        skill_dir,
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: true,
        iteration: Some(1),
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: true,
        benchmark: true,
        require_graders: false,
        lint_evals: false,
        no_cache: true,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    assert!(report_dir.join("report.json").is_file());
    assert!(find_grading_json(&report_dir).is_some());
    assert!(report_dir.join("benchmark.json").is_file());
}

#[test]
fn chained_grade_failure_preserves_report_json() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = temp.path().join("gradable-skill");
    std::fs::create_dir_all(skill_dir.join("evals")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: gradable-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "gradable-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "create output",
                        "expected_output": "done",
                        "graders": [{ "type": "llm", "criterion": "custom check passes" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    let out_dir = temp.path().join("artifacts");

    let report_dir = {
        let status = RunArgs {
            skill_dir: skill_dir.clone(),
            out_dir: out_dir.clone(),
            eval_dir: None,
            model_config: "ci-default".to_string(),
            scenario: vec![ScenarioKind::WithSkill],
            runner: None,
            runner_model: None,
            timeout_secs: None,
            retries: 0,
            force: false,
            iteration: Some(1),
            attempts: Some(AttemptCount::single()),
            concurrency: RunConcurrency::serial(),
            old_skill_dir: None,
            allow_skill_name_mismatch: false,
            output_format: OutputFormat::Text,
            grade: false,
            benchmark: false,
            require_graders: false,
            lint_evals: false,
            no_cache: false,
            reuse_completed: false,
            skill_staging: SkillStaging::Symlink,
            environment: EnvironmentPolicy::Scrubbed,
            permission: PermissionGrant::WorkspaceWrite,
            allowed_tools: Vec::new(),
            allow_scaffold: false,
            trust_skill: true,
            forward_telemetry: false,
            cases: Vec::new(),
            tags: Vec::new(),
            split: SplitArg::All,
            max_cost_usd: None,
            max_tool_calls_per_run: None,
            max_turns_per_run: None,
            ci: EvalCiArgs::default(),
        }
        .handle(&crate::fs::RealFS);
        assert_eq!(status, ExitCode::Success);

        std::fs::read_dir(out_dir.join("gradable-skill"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .next()
            .expect("report dir")
    };

    let report_before = std::fs::read_to_string(report_dir.join("report.json")).expect("report.json");

    let (status, _) = grade_report_dir_with_report(
        &report_dir,
        GradeOptions {
            grader: GraderMode::Script,
            grader_provider: crate::agentskills::judge::JudgeProvider::default(),
            grader_model: None,
            grader_command: Some("/nonexistent/grader-binary".into()),
            grader_votes: crate::agentskills::judge_votes::JudgeVotes::single(),
            strict: false,
        },
        OutputFormat::Text,
    );

    assert_ne!(status, ExitCode::Success);
    assert!(report_dir.is_dir());
    assert_eq!(
        std::fs::read_to_string(report_dir.join("report.json")).unwrap(),
        report_before
    );
}

#[test]
fn run_with_grade_exits_nonzero_when_assertions_fail() {
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_gradable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let status = RunArgs {
        skill_dir,
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: None,
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: false,
        iteration: Some(1),
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: true,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    }
    .handle(&crate::fs::RealFS);

    assert_ne!(status, ExitCode::Success);
    let report_dirs: Vec<_> = std::fs::read_dir(out_dir.join("gradable-skill"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(report_dirs.len(), 1);
    assert!(report_dirs[0].join("report.json").is_file());
}

#[test]
fn benchmark_without_grade_flags_missing_grading() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_gradable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        skill_dir,
        out_dir,
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::Codex),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        force: true,
        iteration: Some(1),
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: true,
        require_graders: false,
        lint_evals: false,
        no_cache: true,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    });

    let benchmark: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("benchmark.json")).unwrap()).unwrap();
    assert_eq!(
        benchmark
            .pointer("/scenarios/with_skill/completed/missing_grading")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
}

/// A pass told to spend nothing, or a ceiling that cannot be compared against spend,
/// is refused where every other malformed run argument is refused: at the command
/// line, before a report exists to explain it later.
#[test]
fn a_max_cost_that_cannot_bound_a_run_is_refused_at_the_command_line() {
    for value in ["0", "-5", "nan"] {
        assert!(
            crate::cli::Cli::try_parse_from([
                "trg",
                "ai",
                "skills",
                "eval",
                "run",
                "--skill-dir",
                "skill",
                "--out-dir",
                "out",
                "--max-cost-usd",
                value,
            ])
            .is_err(),
            "'{value}' must not parse as a cost ceiling"
        );
    }
}

/// A pass that cannot afford everything it was asked to run must say so rather than
/// pretend it ran everything: the runs the ledger refuses are marked skipped for the
/// reason the ledger refused them, not dropped or reported as failed runs.
#[test]
fn a_pass_under_an_unaffordable_ceiling_skips_the_runs_it_cannot_afford() {
    super::fake_runner::reset();
    super::fake_runner::set_cost_usd(5.0);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
        max_cost_usd: Some(CostCeiling::parse(5.0).unwrap()),
        ..base_run_args(&skill_dir, &out_dir)
    });

    assert_eq!(
        status,
        ExitCode::BudgetExhausted,
        "a budget-exhausted pass has to say so in its exit code"
    );

    let report = read_report(&report_dir);
    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(report["runs"][1]["status"], "skipped");
    assert_eq!(report["runs"][1]["failure_kind"], "budget");
    assert!(
        report["runs"][1]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning.as_str().unwrap_or_default().contains("cost ceiling")),
        "the report has to say why the run did not start"
    );

    assert_eq!(report["budget"]["ceiling_usd"], 5.0);
    assert_eq!(report["budget"]["spent"]["usd"], 5.0);
    assert_eq!(report["budget"]["exhausted"], true);
    assert_eq!(report["budget"]["runs_skipped"], 1);
}

/// A case that names a conversation history is asking a harness to resume a
/// transcript it did not itself produce. No installed harness offers that, so the
/// run must be skipped before the runner is ever started, not silently answered as
/// a fresh conversation that never saw the history the case authored.
#[test]
fn a_case_seeding_a_conversation_history_is_skipped_before_the_runner_starts() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_conversation_seeding_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(report["runs"][0]["status"], "completed");

    assert_eq!(report["runs"][1]["status"], "skipped");
    assert_eq!(report["runs"][1]["failure_kind"], "unsupported");
    assert_eq!(
        super::fake_runner::invocations(),
        1,
        "the only invocation must be the case with no conversation history to seed"
    );
    assert!(
        report["runs"][1]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap_or_default()
                .contains("offers no conversation seeding")),
        "the report has to say why the run did not start"
    );
}

/// A case that appends to the system prompt is asking what the agent does when it was
/// instructed a particular way. A harness with no way to append one cannot be asked
/// that question, so running it without the appendix would file an answer to a
/// different question under the case's name.
#[test]
fn a_case_appending_to_the_system_prompt_is_skipped_on_a_harness_that_cannot_carry_it() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_system_prompt_appendix_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        runner: Some(Runner::Codex),
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(report["runs"][0]["status"], "completed");

    assert_eq!(report["runs"][1]["status"], "skipped");
    assert_eq!(report["runs"][1]["failure_kind"], "unsupported");
    assert_eq!(
        super::fake_runner::invocations(),
        1,
        "the only invocation must be the case with no appendix to carry"
    );
    assert!(
        report["runs"][1]["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap_or_default()
                .contains("offers no system prompt append")),
        "the report has to say why the run did not start"
    );
}

#[test]
fn a_case_appending_to_the_system_prompt_runs_on_a_harness_that_carries_it() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_system_prompt_appendix_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(report["runs"][1]["status"], "completed");
    assert_eq!(super::fake_runner::invocations(), 2);
}

/// `--permission` defaults to the narrowest grant, matching every other harness flag
/// this command exposes.
#[test]
fn permission_defaults_to_workspace_write_at_the_command_line() {
    let cli = crate::cli::Cli::try_parse_from([
        "trg",
        "ai",
        "skills",
        "eval",
        "run",
        "--skill-dir",
        "skill",
        "--out-dir",
        "out",
    ])
    .expect("parses");

    let crate::commands::Commands::Ai {
        command: crate::commands::ai::AiCommands::Skills { command },
    } = cli.command
    else {
        panic!("expected an ai skills command");
    };
    let crate::commands::ai::skills::SkillsCommands::Eval(eval) = command else {
        panic!("expected an eval command");
    };
    let super::super::EvalCommands::Run(args) = eval.command else {
        panic!("expected an eval run command");
    };

    assert_eq!(args.permission, PermissionGrant::WorkspaceWrite);
}

/// `--permission unrestricted` has to reach the harness, or the flag would only ever
/// change what the command line accepts and never what a run is allowed to do.
#[test]
fn an_explicit_permission_parses_from_the_command_line() {
    let cli = crate::cli::Cli::try_parse_from([
        "trg",
        "ai",
        "skills",
        "eval",
        "run",
        "--skill-dir",
        "skill",
        "--out-dir",
        "out",
        "--permission",
        "unrestricted",
    ])
    .expect("parses");

    let crate::commands::Commands::Ai {
        command: crate::commands::ai::AiCommands::Skills { command },
    } = cli.command
    else {
        panic!("expected an ai skills command");
    };
    let crate::commands::ai::skills::SkillsCommands::Eval(eval) = command else {
        panic!("expected an eval command");
    };
    let super::super::EvalCommands::Run(args) = eval.command else {
        panic!("expected an eval run command");
    };

    assert_eq!(args.permission, PermissionGrant::Unrestricted);
}

/// The grant a run was given has to be readable from the report it produced, or an
/// operator comparing two reports cannot tell whether a scoring difference came from
/// the skill or from what the harness was allowed to do.
#[test]
fn the_permission_a_run_was_given_round_trips_into_its_report() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        permission: PermissionGrant::Unrestricted,
        allowed_tools: Vec::new(),
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(report["report"]["permission"]["requested"], "unrestricted");
    assert_eq!(report["report"]["permission"]["effective"], "unrestricted");
}

/// cursor-agent has no `workspace_write` mode of its own: any run under it executes
/// unrestricted regardless of what was requested. A report that only recorded the
/// request would tell a reader the run was bounded when it was not.
#[test]
fn a_cursor_agent_run_reports_the_wider_grant_it_actually_ran_under() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report_dir = run_with_fake_runner(RunArgs {
        runner: Some(Runner::CursorAgent),
        permission: PermissionGrant::WorkspaceWrite,
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(report["report"]["permission"]["requested"], "workspace_write");
    assert_eq!(report["report"]["permission"]["effective"], "unrestricted");
}

/// Grading a run that never started reads the empty workspace as a wrong answer, and
/// every assertion fails. That turns a spending decision into a reported regression,
/// and the resulting exit 1 hides the exit 2 that would have told an operator to raise
/// the ceiling rather than go looking for a bug in the skill.
#[test]
fn a_run_the_ceiling_refused_is_not_graded_as_a_failure() {
    super::fake_runner::reset();
    super::fake_runner::set_cost_usd(5.0);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_gradable_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
        grade: true,
        max_cost_usd: Some(CostCeiling::parse(5.0).unwrap()),
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(report["runs"][1]["status"], "skipped");
    assert_eq!(report["runs"][1]["failure_kind"], "budget");
    assert_eq!(
        count_grading_json(&report_dir),
        1,
        "only the run that produced a workspace has anything to grade"
    );
    assert_eq!(
        status,
        ExitCode::BudgetExhausted,
        "the pass reports that it ran out of money, not that the skill got something wrong"
    );
}

/// Spend reaching the ceiling is not the same as spend passing it. A pass whose last
/// run lands exactly on the ceiling refused nothing and paid no more than it said it
/// would, so it has nothing to report that an operator did not already agree to. A
/// gate that cries wolf on the boundary is a gate a team learns to ignore.
#[test]
fn a_pass_that_lands_exactly_on_its_ceiling_without_losing_a_run_still_exits_clean() {
    super::fake_runner::reset();
    super::fake_runner::set_cost_usd(5.0);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
        max_cost_usd: Some(CostCeiling::parse(10.0).unwrap()),
        ..base_run_args(&skill_dir, &out_dir)
    });

    assert_eq!(
        status,
        ExitCode::Success,
        "a pass that did every run inside its ceiling has not failed"
    );

    let report = read_report(&report_dir);
    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(report["runs"][1]["status"], "completed");
    assert_eq!(report["budget"]["spent"]["usd"], 10.0);
    assert_eq!(report["budget"]["ceiling_usd"], 10.0);
    assert_eq!(
        report["budget"]["runs_skipped"], 0,
        "nothing was refused, so nothing may be reported as skipped"
    );
    assert_eq!(
        report["budget"]["exhausted"], true,
        "the ledger would admit nothing further, and the report still says so"
    );
}

/// A pass that prints its result reports the same code it exits with. A reader
/// parsing the JSON and a shell reading `$?` are asking the same question, and a
/// budget stop the JSON does not mention is a green result in every dashboard
/// built on it.
#[test]
fn the_exit_code_in_the_json_carries_the_budget_stop_the_process_exits_with() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (status, report_dir) = run_with_fake_runner_reporting_status(base_run_args(&skill_dir, &out_dir));
    assert_eq!(
        status,
        ExitCode::Success,
        "the pass itself is clean, so only the budget can move the code"
    );

    let ci = EvalCiArgs::default();
    let stopped = super::super::eval_output(&report_dir, ci.policy(), &ci.thresholds(), None, true)
        .expect("the report is readable");
    assert_eq!(
        stopped.exit_code,
        ExitCode::BudgetExhausted,
        "a pass the ceiling cut short reports the budget code in the document it prints"
    );

    let ran_fully = super::super::eval_output(&report_dir, ci.policy(), &ci.thresholds(), None, false)
        .expect("the report is readable");
    assert_eq!(
        ran_fully.exit_code,
        ExitCode::Success,
        "and reports nothing when the ceiling took nothing"
    );
}

/// A run the ceiling refused cost money it never spent, once. Retries are invisible in
/// the report, which keeps only the attempt that stuck, so a ledger fed from the report
/// prices a flaky pass at a fraction of what it actually billed and lets a ceiling
/// be walked straight through.
/// A transient attempt is discarded whole, and what it asked of the mocks has to go
/// with it. The call log is appended to for the life of a run, so an attempt nobody
/// kept otherwise leaves its breaches behind for the attempt that stuck to answer
/// for, and a run that did everything asked of it fails for a call it never made.
#[test]
fn a_run_is_not_graded_on_the_mock_calls_of_the_attempts_that_were_discarded() {
    super::fake_runner::reset();
    super::fake_runner::fail_transiently_times(2);
    super::fake_runner::log_mock_calls();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (_status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
        retries: 2,
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(
        report["runs"][0]["runner_invocations"], 3,
        "two attempts were discarded before one stuck"
    );

    let breaches = report["runs"][0]["mock_violations"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    assert_eq!(
        breaches, 0,
        "the surviving attempt called the mocks exactly as the case expected, so the run is clean"
    );
}

#[test]
fn the_ledger_counts_the_attempts_that_were_thrown_away() {
    super::fake_runner::reset();
    super::fake_runner::set_cost_usd(1.0);
    super::fake_runner::fail_transiently_times(2);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let (_status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
        retries: 2,
        ..base_run_args(&skill_dir, &out_dir)
    });

    let report = read_report(&report_dir);
    assert_eq!(
        report["runs"][0]["runner_invocations"], 3,
        "the first case was invoked three times before one stuck"
    );
    assert_eq!(report["runs"][1]["runner_invocations"], 1);
    assert_eq!(
        report["budget"]["spent"]["usd"], 4.0,
        "every invocation was billed, not only the two the report kept"
    );
}

/// A ceiling is checked before a run starts, not before one is served. A cache hit
/// costs nothing, so a case the pass already paid for in an earlier report has to be
/// readable from a later report even after that later report has spent its way past
/// the ceiling on the cases the cache could not answer for.
#[test]
fn a_cached_run_is_still_served_after_the_ceiling_is_reached() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = temp.path().join("mixed-skill");
    std::fs::create_dir_all(skill_dir.join("evals/one/fixtures")).unwrap();
    std::fs::create_dir_all(skill_dir.join("evals/two")).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: mixed-skill\ndescription: fixture\n---\n",
    )
    .unwrap();
    std::fs::write(
        skill_dir.join("evals/evals.json"),
        r#"{
                "skill_name": "mixed-skill",
                "evals": [
                    {
                        "id": "one",
                        "prompt": "first prompt",
                        "expected_output": "first output",
                        "graders": [{ "type": "contains", "text": "checks first" }]
                    },
                    {
                        "id": "two",
                        "prompt": "second prompt",
                        "expected_output": "second output",
                        "graders": [{ "type": "contains", "text": "checks second" }]
                    }
                ]
            }"#,
    )
    .unwrap();
    std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v1").unwrap();
    let out_dir = temp.path().join("artifacts");

    let base = || RunArgs {
        skill_dir: skill_dir.clone(),
        out_dir: out_dir.clone(),
        eval_dir: None,
        model_config: "ci-default".to_string(),
        scenario: vec![ScenarioKind::WithSkill],
        runner: Some(Runner::ClaudeCode),
        runner_model: None,
        timeout_secs: None,
        retries: 0,
        attempts: Some(AttemptCount::single()),
        concurrency: RunConcurrency::serial(),
        force: true,
        iteration: None,
        old_skill_dir: None,
        allow_skill_name_mismatch: false,
        output_format: OutputFormat::Text,
        grade: false,
        benchmark: false,
        require_graders: false,
        lint_evals: false,
        no_cache: false,
        reuse_completed: false,
        skill_staging: SkillStaging::Symlink,
        environment: EnvironmentPolicy::Scrubbed,
        permission: PermissionGrant::WorkspaceWrite,
        allowed_tools: Vec::new(),
        allow_scaffold: false,
        trust_skill: true,
        forward_telemetry: false,
        cases: Vec::new(),
        tags: Vec::new(),
        split: SplitArg::All,
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ci: EvalCiArgs::default(),
    };

    // The first pass prices nothing and populates the cache for both cases.
    run_with_fake_runner(base());

    // The second pass changes the fixture case "one" reads, so only "one" has to
    // execute again. It executes at a price the ceiling cannot cover a second run
    // of, while "two" is unchanged and must still come from the cache pass one paid
    // for.
    std::fs::write(skill_dir.join("evals/one/fixtures/input.txt"), "fixture-v2").unwrap();
    super::fake_runner::set_cost_usd(5.0);
    let (status, report_dir) = run_with_fake_runner_reporting_status(RunArgs {
        max_cost_usd: Some(CostCeiling::parse(1.0).unwrap()),
        ..base()
    });
    assert_eq!(
        status,
        ExitCode::BudgetExhausted,
        "the second pass has to report the ceiling it hit"
    );

    let report = read_report(&report_dir);
    assert!(
        report["runs"][0]["cache"].is_null(),
        "the fixture change has to force case one to execute again"
    );
    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(
        report["runs"][1]["cache"]["source_run_id"], "run-002",
        "case two is unchanged, so it is still served from the pass that already paid for it"
    );
    assert_eq!(report["runs"][1]["status"], "completed");
    assert_eq!(
        report["budget"]["runs_skipped"], 0,
        "a cache hit is not a run the ledger had to refuse"
    );
}

/// A pass told no ceiling has nothing to check a run against, so every case runs
/// whatever it costs. The ledger still counts what ran, since a number worth reporting
/// is not the same thing as a number worth enforcing.
#[test]
fn a_pass_with_no_ceiling_runs_every_case_even_when_each_one_costs_money() {
    super::fake_runner::reset();
    super::fake_runner::set_cost_usd(50.0);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        max_cost_usd: None,
        max_tool_calls_per_run: None,
        max_turns_per_run: None,
        ..base_run_args(&skill_dir, &out_dir)
    }));

    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(report["runs"][1]["status"], "completed");
    assert_eq!(report["budget"]["ceiling_usd"], serde_json::Value::Null);
    assert_eq!(report["budget"]["spent"]["usd"], 100.0);
    assert_eq!(report["budget"]["exhausted"], false);
    assert_eq!(report["budget"]["runs_skipped"], 0);
}

/// A runner that never says what a run cost has not run it for free: the ledger must
/// not treat a missing price as a zero one, or a runner that forgot to price itself
/// would silently exempt every run it made from the ceiling meant to bound it.
#[test]
fn a_runner_that_reports_no_cost_does_not_silently_count_as_free() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        max_cost_usd: Some(CostCeiling::parse(1.0).unwrap()),
        ..base_run_args(&skill_dir, &out_dir)
    }));

    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(
        report["runs"][1]["status"], "completed",
        "an uncosted run must not be mistaken for one the ledger had to refuse"
    );
    assert_eq!(report["budget"]["spent"]["usd"], 0.0);
    assert_eq!(report["budget"]["exhausted"], false);
    assert_eq!(report["budget"]["runs_skipped"], 0);
}

/// A harness that publishes no price still spends money, so the total it leaves
/// behind is zero for a reason that has nothing to do with thrift. A reader adding
/// spend across harnesses has to see the difference between a free pass and one
/// nobody can price.
#[test]
fn a_pass_no_harness_could_price_reports_no_total_rather_than_a_free_one() {
    super::fake_runner::reset();
    super::fake_runner::set_cost_usd(2.0);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        runner: Some(Runner::Codex),
        ..base_run_args(&skill_dir, &out_dir)
    }));

    assert_eq!(report["budget"]["spent"]["kind"], "unpriced");
    assert_eq!(report["budget"]["spent"]["harness"], "codex");
    assert!(report["budget"]["spent"]["usd"].is_null());
}

/// A ceiling over a harness that publishes no price never moves off zero, so it
/// admits every run while the operator believes a limit is holding. It is refused
/// at the command line, before a report exists to be read as bounded.
#[test]
fn a_ceiling_over_a_harness_that_publishes_no_price_is_refused() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    super::fake_runner::enable();
    let status = RunArgs {
        runner: Some(Runner::Codex),
        max_cost_usd: Some(CostCeiling::parse(5.0).unwrap()),
        ..base_run_args(&skill_dir, &out_dir)
    }
    .handle(&crate::fs::RealFS);
    super::fake_runner::disable();

    assert_eq!(status, ExitCode::InfrastructureFailure);
    assert!(
        !out_dir.exists(),
        "the pass was refused before it wrote a report that would have claimed a ceiling"
    );
}

/// The ledger is shared across every lane a pass spawns, so what it reports after a
/// concurrent pass must equal the sum of what each lane actually spent, whether the
/// lanes' writes land at the same instant or not.
#[test]
fn a_ledger_shared_across_lanes_records_the_sum_of_every_run_it_admitted() {
    super::fake_runner::reset();
    super::fake_runner::expect_lanes(2);
    super::fake_runner::set_cost_usd(3.0);
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        concurrency: RunConcurrency::parse(2).unwrap(),
        max_cost_usd: Some(CostCeiling::parse(100.0).unwrap()),
        ..base_run_args(&skill_dir, &out_dir)
    }));

    assert_eq!(
        super::fake_runner::peak_lanes(),
        2,
        "both runs have to be in flight at once for this to test the shared ledger"
    );
    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(report["runs"][1]["status"], "completed");
    assert_eq!(
        report["budget"]["spent"]["usd"], 6.0,
        "two lanes each recording $3 must add to $6, not lose a write to the other"
    );
    assert_eq!(report["budget"]["runs_skipped"], 0);
}

fn write_mocked_skill(root: &Path) -> PathBuf {
    let skill_dir = write_cacheable_skill(root);
    std::fs::create_dir_all(skill_dir.join("evals/mocks/github")).unwrap();
    std::fs::write(
        skill_dir.join("evals/mocks/github/create_issue.md"),
        "---\ntype: fixed\n---\n{\"issue_number\": 1}\n",
    )
    .unwrap();
    skill_dir
}

/// A case that declares mcp mocks against a runner whose harness offers no `mcp
/// servers` control at all must never reach the runner: there is nothing honest a
/// mocked run against such a harness could report, so it is skipped as `unsupported`
/// rather than invoked and reported as though the skill had done something wrong.
/// The log says a mock server came up, so an attempt must not inherit that claim from
/// the attempt before it: the next attempt's server writes the file when it has loaded
/// its mocks, and until it does the absence is what tells a grader nothing answered.
#[test]
fn discarding_the_log_between_attempts_leaves_the_next_server_to_say_it_came_up() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run-1");
    std::fs::create_dir_all(&run_dir).unwrap();
    let log = run_dir.join(MOCK_CALLS_LOG_NAME);
    std::fs::write(&log, "").unwrap();

    discard_mock_calls_from_earlier_attempts(&run_dir, "run-1");

    assert!(
        !log.exists(),
        "an empty log left behind would credit this attempt with a mock server it never started"
    );
}

/// Where no mock set was materialized there is no log to clear, and inventing one would
/// report a run that declared no mocks as one that declared some and used none.
#[test]
fn clearing_the_log_creates_nothing_for_a_run_that_declared_no_mocks() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join("run-1");
    std::fs::create_dir_all(&run_dir).unwrap();

    discard_mock_calls_from_earlier_attempts(&run_dir, "run-1");

    assert!(!run_dir.join(MOCK_CALLS_LOG_NAME).exists());
}

#[test]
fn a_case_with_mocks_against_a_runner_with_no_mcp_servers_control_is_skipped_not_invoked() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_mocked_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        runner: Some(Runner::CursorAgent),
        ..base_run_args(&skill_dir, &out_dir)
    }));

    assert_eq!(report["runs"][0]["status"], "skipped");
    assert_eq!(
        report["runs"][0]["failure_kind"],
        crate::agentskills::runner::FAILURE_KIND_UNSUPPORTED
    );
    assert!(
        report["runs"][0]["warnings"][0]
            .as_str()
            .unwrap()
            .contains("offers no mcp servers"),
        "a skipped run must say which control the harness lacks, not only that it was skipped"
    );
    assert_eq!(
        report["runs"][0]["metrics"]["duration_ms"],
        serde_json::Value::Null,
        "a skipped run was never handed to the runner, so it has no duration to report"
    );
}

/// codex takes its mock servers as a file in its config home and offers no flag that
/// excludes the operator's own. Under a policy that leaves the operator's config home
/// in place, a mocked run would have reached real MCP servers alongside the mocks and
/// been reported as though only the mocks had answered.
#[test]
fn a_mocked_codex_case_is_refused_where_the_operators_own_mcp_servers_would_stay_live() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_mocked_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        runner: Some(Runner::Codex),
        environment: EnvironmentPolicy::Scrubbed,
        ..base_run_args(&skill_dir, &out_dir)
    }));

    assert_eq!(report["runs"][0]["status"], "skipped");
    assert_eq!(
        report["runs"][0]["failure_kind"],
        crate::agentskills::runner::FAILURE_KIND_UNSUPPORTED
    );
    let warning = report["runs"][0]["warnings"][0].as_str().unwrap();
    assert!(
        warning.contains("--environment isolated"),
        "a refusal an operator can act on has to name the policy that grants the run its own config home: {warning}"
    );
}

#[test]
fn a_mocked_codex_case_runs_once_the_run_owns_the_config_home_the_mocks_are_declared_in() {
    super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_mocked_skill(temp.path());
    let out_dir = temp.path().join("artifacts");

    let report = read_report(&run_with_fake_runner(RunArgs {
        runner: Some(Runner::Codex),
        environment: EnvironmentPolicy::Isolated,
        ..base_run_args(&skill_dir, &out_dir)
    }));

    assert_eq!(report["runs"][0]["status"], "completed");
    assert_eq!(report["runs"][0]["failure_kind"], serde_json::Value::Null);
}

mod telemetry_tests;
