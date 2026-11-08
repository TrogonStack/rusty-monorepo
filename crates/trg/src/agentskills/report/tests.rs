use super::*;
use crate::fs::testutil::MemFS;
use std::path::Path;

#[test]
fn a_bundle_written_before_the_environment_field_reads_as_scrubbed_not_as_the_new_default() {
    let section: ReportSection = serde_json::from_str(
        r#"{
                "id": "report-1",
                "generated_at": "2026-01-01T00:00:00Z",
                "iteration": 1,
                "producer": { "name": "trg", "version": "0.0.0" }
            }"#,
    )
    .expect("a bundle predating the environment field still opens");

    assert_eq!(
        section.environment,
        EnvironmentPolicy::Scrubbed,
        "a bundle that recorded no policy must not be read as having had the strongest one"
    );
    assert_eq!(
        EnvironmentPolicy::default(),
        EnvironmentPolicy::Isolated,
        "the CLI default moved, and this field deliberately does not track it"
    );
}

#[test]
fn a_capture_status_of_partial_from_before_the_rename_reads_as_incomplete() {
    let status: ModelCaptureStatus =
        serde_json::from_str("\"partial\"").expect("the retired 'partial' spelling must still deserialize");
    assert_eq!(status, ModelCaptureStatus::Incomplete);

    assert_eq!(
        serde_json::to_string(&ModelCaptureStatus::Incomplete).unwrap(),
        "\"incomplete\"",
        "this process must only ever write the current spelling, never the retired alias"
    );
}

fn sample_suite() -> EvalSuite {
    serde_json::from_str(
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "case-a",
                        "name": "Case A",
                        "description": "Covers case A.",
                        "priority": "critical",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "files": ["fixture.txt"],
                        "graders": [
                            { "type": "file_exists", "path": "fixture.txt" },
                            { "type": "contains", "text": "total", "name": "mentions-total" }
                        ]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "files": [],
                        "graders": [
                            { "type": "contains", "text": "b1" },
                            { "type": "contains", "text": "b2" }
                        ]
                    }
                ]
            }"#,
    )
    .unwrap()
}

#[test]
fn build_dimensions_from_suite() {
    let suite = sample_suite();
    let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
    let dimensions = build_dimensions(
        &suite,
        "sha256:abc",
        None,
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        &slugs,
    );

    assert_eq!(dimensions.eval_cases.len(), 2);
    assert_eq!(dimensions.eval_cases[0].slug, "case-a");
    assert_eq!(dimensions.eval_cases[0].name.as_deref(), Some("Case A"));
    assert_eq!(dimensions.eval_cases[0].description.as_deref(), Some("Covers case A."));
    assert_eq!(dimensions.eval_cases[0].priority, Some(EvalPriority::Critical));
    assert_eq!(dimensions.eval_cases[0].assertion_ids, vec!["case-a:g0", "case-a:g1"]);
    assert_eq!(dimensions.eval_cases[1].name, None);
    assert_eq!(dimensions.eval_cases[1].description, None);
    assert_eq!(dimensions.eval_cases[1].priority, None);
    assert_eq!(dimensions.eval_cases[1].assertion_ids, vec!["case-b:g0", "case-b:g1"]);
    assert_eq!(dimensions.assertions.len(), 4);
    assert_eq!(dimensions.assertions[0].id, "case-a:g0");
    assert_eq!(dimensions.assertions[0].eval_case_id, "case-a");
    assert_eq!(dimensions.assertions[0].name, None);
    assert_eq!(dimensions.assertions[1].id, "case-a:g1");
    assert_eq!(dimensions.assertions[1].eval_case_id, "case-a");
    assert_eq!(dimensions.assertions[1].text, "final text contains 'total'");
    assert_eq!(dimensions.assertions[1].name.as_deref(), Some("mentions-total"));
    assert_eq!(dimensions.scenarios.len(), 2);
    assert_eq!(
        dimensions.model_configs[0].capture_status,
        ModelCaptureStatus::Incomplete
    );
    assert_eq!(dimensions.model_configs[0].label, "ci-default");
}

/// `build_dimensions` scaffolds every model config `Incomplete` before a run has
/// executed. Once every run resolves a model, the config it belongs to reads
/// `Complete`; a run that never resolves one keeps its config `Incomplete` even
/// though every other run for that config did resolve one.
#[test]
fn recompute_model_capture_status_reflects_what_runs_actually_resolved() {
    let suite = sample_suite();
    let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
    let dimensions = build_dimensions(
        &suite,
        "sha256:abc",
        None,
        "ci-default",
        &[ScenarioKind::WithSkill],
        &slugs,
    );
    let (runs, _workspace_dirs) = build_runs(
        &suite,
        &[ScenarioKind::WithSkill],
        "ci-default",
        1,
        None,
        &slugs,
        SkillStaging::Symlink,
    );

    let mut document = ReportDocument {
        report: ReportSection {
            id: "report-123".to_string(),
            generated_at: "2026-05-25T22:00:00Z".to_string(),
            iteration: 1,
            producer: ProducerSection {
                name: "trg".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            runner: None,
            runner_binary: None,
            runner_version: None,
            environment: EnvironmentPolicy::default(),
            permission: PermissionOutcome::default(),
            allowed_tools: None,
            telemetry_forwarding: TelemetryForwarding::Off,
            ci: None,
            trace: None,
        },
        suite: SuiteSection {
            skill_name: "demo-skill".to_string(),
            skill_path: "demo-skill".to_string(),
            skill_hash: "sha256:deadbeef".to_string(),
            evals_path: "demo-skill/evals/evals.json".to_string(),
            evals_hash: "sha256:feedface".to_string(),
            eval_dir: EvalDirName::default(),
            old_skill_path: None,
            old_skill_hash: None,
            case_selection: None,
        },
        dimensions,
        runs,
        assertion_results: Vec::new(),
        summaries: SummariesSection {
            by_scenario: Vec::new(),
            human_feedback: None,
        },
        improvement_feedback: Vec::new(),
        comparisons: Vec::new(),
        iteration_summary: None,
        budget: None,
    };

    recompute_model_capture_status(&mut document);
    assert_eq!(
        document.dimensions.model_configs[0].capture_status,
        ModelCaptureStatus::Incomplete
    );

    for run in document.runs.iter_mut() {
        run.runner_model = Some(ModelName::parse("opus").unwrap());
    }
    recompute_model_capture_status(&mut document);
    assert_eq!(
        document.dimensions.model_configs[0].capture_status,
        ModelCaptureStatus::Complete
    );

    document.runs[0].runner_model = None;
    recompute_model_capture_status(&mut document);
    assert_eq!(
        document.dimensions.model_configs[0].capture_status,
        ModelCaptureStatus::Incomplete
    );
}

#[test]
fn build_runs_multiple_attempts_produces_n_runs_per_eval_scenario() {
    let suite = sample_suite();
    let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
    let scenarios = [ScenarioKind::WithSkill];
    let (runs, workspace_dirs) = build_runs(
        &suite,
        &scenarios,
        "ci-default",
        1,
        Some(AttemptCount::parse(3).unwrap()),
        &slugs,
        SkillStaging::Symlink,
    );

    assert_eq!(runs.len(), 6);
    assert_eq!(workspace_dirs.len(), 6);
    for eval_case in ["case-a", "case-b"] {
        let case_runs: Vec<_> = runs.iter().filter(|r| r.eval_case_id == eval_case).collect();
        assert_eq!(case_runs.len(), 3);
        assert_eq!(case_runs[0].attempt, 1);
        assert_eq!(case_runs[1].attempt, 2);
        assert_eq!(case_runs[2].attempt, 3);
        assert_eq!(
            case_runs[0].mirror_path,
            format!("iteration-1/eval-{eval_case}/with_skill/attempt-1/")
        );
        assert_eq!(
            case_runs[2].mirror_path,
            format!("iteration-1/eval-{eval_case}/with_skill/attempt-3/")
        );
    }
}

#[test]
fn every_reason_a_run_is_not_started_records_the_same_unscoreable_status() {
    let suite = sample_suite();
    let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
    let (mut runs, _) = build_runs(
        &suite,
        &[ScenarioKind::WithSkill],
        "ci-default",
        1,
        Some(AttemptCount::parse(1).unwrap()),
        &slugs,
        SkillStaging::Symlink,
    );

    let reasons = [
        RunNotStarted::CostCeilingExhausted {
            spent_usd: 4.0,
            ceiling_usd: 3.0,
        },
        RunNotStarted::ControlUnsupported {
            control: HarnessControl::ConversationSeeding,
            runner: Runner::ClaudeCode,
        },
        RunNotStarted::ToolGrantUnrepresentable {
            runner: Runner::ClaudeCode,
        },
        RunNotStarted::ControlNeedsRunOwnedConfigHome {
            control: HarnessControl::McpServers,
            runner: Runner::Codex,
            environment: EnvironmentPolicy::Scrubbed,
        },
    ];

    let mut kinds = Vec::new();
    for reason in reasons {
        let run = &mut runs[0];
        run.warnings.clear();
        run.not_started(reason);

        assert!(!run.started());
        assert_eq!(run.warnings.len(), 1, "the reason is said once, in the run itself");
        assert!(run.warnings[0].ends_with("so this run was not started"));
        kinds.push(run.failure_kind.clone().expect("a run stopped early names its kind"));
    }

    assert_eq!(
        kinds,
        vec![
            "budget".to_string(),
            "unsupported".to_string(),
            "unsupported".to_string(),
            "unsupported".to_string(),
        ]
    );
}

/// A reader who sees `unsupported` on a codex run whose harness does list the control
/// has to be told which knob of their own turns it on, or the only way out of the
/// refusal is to guess.
#[test]
fn refusing_a_control_for_want_of_an_owned_config_home_names_the_environment_that_grants_one() {
    let warning = RunNotStarted::ControlNeedsRunOwnedConfigHome {
        control: HarnessControl::McpServers,
        runner: Runner::Codex,
        environment: EnvironmentPolicy::Scrubbed,
    }
    .warning();

    assert!(warning.contains("mcp servers"), "unexpected warning: {warning}");
    assert!(
        warning.contains("--environment isolated"),
        "unexpected warning: {warning}"
    );
    assert!(
        warning.contains("--environment scrubbed"),
        "unexpected warning: {warning}"
    );
    assert!(warning.ends_with("so this run was not started"));
}

#[test]
fn build_runs_single_attempt_keeps_legacy_mirror_path() {
    let suite = sample_suite();
    let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
    let (runs, _) = build_runs(
        &suite,
        &[ScenarioKind::WithSkill],
        "ci-default",
        1,
        Some(AttemptCount::single()),
        &slugs,
        SkillStaging::Symlink,
    );
    assert_eq!(runs[0].mirror_path, "iteration-1/eval-case-a/with_skill/");
    assert_eq!(runs[0].attempt, 1);
}

#[test]
fn build_runs_orders_by_eval_case_then_scenario() {
    let suite = sample_suite();
    let slugs = crate::agentskills::layout::assign_eval_slugs(&suite.evals);
    let scenarios = [ScenarioKind::WithSkill, ScenarioKind::WithoutSkill];
    let (runs, workspace_dirs) = build_runs(
        &suite,
        &scenarios,
        "ci-default",
        2,
        Some(AttemptCount::single()),
        &slugs,
        SkillStaging::Symlink,
    );

    assert_eq!(runs.len(), 4);
    assert_eq!(runs[0].id, "run-001");
    assert_eq!(runs[0].eval_case_id, "case-a");
    assert_eq!(runs[0].eval_slug, "case-a");
    assert_eq!(runs[0].iteration, 2);
    assert_eq!(runs[0].mirror_path, "iteration-2/eval-case-a/with_skill/");
    assert_eq!(runs[0].scenario_id, ScenarioKind::WithSkill);
    assert_eq!(runs[1].id, "run-002");
    assert_eq!(runs[1].eval_case_id, "case-a");
    assert_eq!(runs[1].scenario_id, ScenarioKind::WithoutSkill);
    assert_eq!(runs[2].id, "run-003");
    assert_eq!(runs[2].eval_case_id, "case-b");
    assert_eq!(runs[2].scenario_id, ScenarioKind::WithSkill);
    assert_eq!(runs[3].id, "run-004");
    assert_eq!(runs[3].eval_case_id, "case-b");
    assert_eq!(runs[3].scenario_id, ScenarioKind::WithoutSkill);
    assert_eq!(
        workspace_dirs,
        vec![
            "runs/run-001/workspace",
            "runs/run-002/workspace",
            "runs/run-003/workspace",
            "runs/run-004/workspace",
        ]
    );
    assert!(runs.iter().all(|run| run.status == "skipped"));
}

/// Every run is scaffolded as skipped, so more draws of a cell means more skipped
/// runs. A summary that still counted cells would undercount the pass it describes,
/// and a pass with no runner never rebuilds it, so the undercount is what ships.
fn suite_where_one_case_draws_more_than_the_other() -> EvalSuite {
    serde_json::from_value(serde_json::json!({
        "skill_name": "demo-skill",
        "evals": [
            { "id": "steady", "prompt": "prompt a", "expected_output": "output a", "attempts": 1 },
            { "id": "flaky", "prompt": "prompt b", "expected_output": "output b", "attempts": 5 }
        ]
    }))
    .unwrap()
}

/// The summary is read as the denominator of the pass, so a total multiplied out of one
/// count would claim runs the report does not contain the moment a case pins its own.
#[test]
fn a_summary_totals_the_draws_each_case_actually_takes() {
    let summaries = build_summaries(
        &[ScenarioKind::WithSkill],
        &suite_where_one_case_draws_more_than_the_other(),
        None,
    );

    assert_eq!(
        summaries.by_scenario[0].total_runs, 6,
        "one draw of the case pinned to one plus five of the case pinned to five"
    );
}

/// `mirror_path` tells a reader where a run's outputs live, so it has to be cut from the
/// count that run's own cell took: cut from the pass-wide maximum, a case drawn once
/// would be filed under `attempt-1` and a reader following the unsuffixed path finds
/// nothing.
#[test]
fn a_mirror_path_is_suffixed_only_for_the_cases_that_were_drawn_more_than_once() {
    let suite = suite_where_one_case_draws_more_than_the_other();
    let slugs = suite
        .evals
        .iter()
        .map(|case| (case.id.to_string(), super::super::layout::eval_slug(case.id.as_str())))
        .collect();

    let (runs, _) = build_runs(
        &suite,
        &[ScenarioKind::WithSkill],
        "ci-default",
        1,
        None,
        &slugs,
        SkillStaging::Symlink,
    );

    let mirror_of = |case: &str| {
        runs.iter()
            .filter(|run| run.eval_case_id == case)
            .map(|run| run.mirror_path.clone())
            .collect::<Vec<_>>()
    };

    let steady = mirror_of("steady");
    assert_eq!(steady.len(), 1, "the case pinned to one draw is drawn once");
    assert!(
        !steady[0].contains("attempt-"),
        "one draw has nothing to be told apart from: {}",
        steady[0]
    );

    let flaky = mirror_of("flaky");
    assert_eq!(flaky.len(), 5, "the case pinned to five draws is drawn five times");
    assert!(
        flaky.iter().all(|path| path.contains("attempt-")),
        "five draws need telling apart: {flaky:?}"
    );
}

#[test]
fn build_summaries_counts_every_drawn_run_as_skipped() {
    let summaries = build_summaries(
        &[ScenarioKind::WithSkill],
        &sample_suite(),
        Some(AttemptCount::parse(3).expect("three attempts")),
    );

    let scenario = &summaries.by_scenario[0];
    assert_eq!(scenario.total_runs, 6);
    assert_eq!(
        scenario.skipped_runs, scenario.total_runs,
        "nothing has run yet, so every drawn run is still skipped"
    );
}

#[test]
fn build_summaries_counts_skipped_runs_per_scenario() {
    let summaries = build_summaries(
        &[ScenarioKind::WithSkill, ScenarioKind::OldSkill],
        &sample_suite(),
        Some(AttemptCount::single()),
    );

    assert_eq!(summaries.by_scenario.len(), 2);
    assert_eq!(summaries.by_scenario[0].scenario_id, ScenarioKind::WithSkill);
    assert_eq!(summaries.by_scenario[0].total_runs, 2);
    assert_eq!(summaries.by_scenario[0].skipped_runs, 2);
    assert_eq!(summaries.by_scenario[0].passed_runs, 0);
    assert_eq!(summaries.by_scenario[1].scenario_id, ScenarioKind::OldSkill);
    assert_eq!(summaries.by_scenario[1].failed_runs, 0);
}

#[test]
fn build_dimensions_includes_old_skill_revision_when_present() {
    let suite = sample_suite();
    let eval_slugs = slugs_for_suite(&suite);
    let dimensions = build_dimensions(
        &suite,
        "sha256:current",
        Some("sha256:old"),
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::OldSkill],
        &eval_slugs,
    );

    assert_eq!(dimensions.skill_revisions.len(), 2);
    assert_eq!(dimensions.skill_revisions[0].id, "current");
    assert_eq!(dimensions.skill_revisions[0].skill_hash, "sha256:current");
    assert_eq!(dimensions.skill_revisions[1].id, "old");
    assert_eq!(dimensions.skill_revisions[1].skill_hash, "sha256:old");
}

#[test]
fn build_runs_assigns_old_revision_for_old_skill_scenario() {
    let suite = sample_suite();
    let eval_slugs = slugs_for_suite(&suite);
    let (runs, _) = build_runs(
        &suite,
        &[ScenarioKind::OldSkill],
        "ci-default",
        1,
        Some(AttemptCount::single()),
        &eval_slugs,
        SkillStaging::Symlink,
    );

    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|run| run.skill_revision_id == "old"));
}

#[test]
fn only_copy_staging_puts_the_answer_key_out_of_reach() {
    assert!(SkillStaging::Copy.withholds_the_answer_key());
    assert!(
        !SkillStaging::Symlink.withholds_the_answer_key(),
        "a staged link names the live skill directory, and the suite sits next to it"
    );
    assert!(
        SkillStaging::default().withholds_the_answer_key(),
        "an operator who says nothing about staging gets a run that cannot read its own answer key"
    );
}

#[test]
fn build_report_bundle_records_old_skill_metadata() {
    let fs = MemFS::new();
    let skill_path = Path::new("demo-skill");
    let old_skill_path = Path::new("demo-skill-old");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(
        old_skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: old\n---\n",
    );
    fs.insert(
        skill_path.join("evals/evals.json"),
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "1",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{ "type": "contains", "text": "a" }]
                    }
                ]
            }"#,
    );

    let bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("demo-skill"),
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::OldSkill],
        BuildReportOptions {
            report_id: Some("test-report-id".to_string()),
            generated_at: Some("2026-05-25T22:00:00Z".to_string()),
            iteration: Some(1),
            old_skill_path: Some(old_skill_path.to_path_buf()),
            user_old_skill_path: Some(PathBuf::from("path/to/old-skill")),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        bundle.document.suite.old_skill_path.as_deref(),
        Some("path/to/old-skill")
    );
    assert!(bundle
        .document
        .suite
        .old_skill_hash
        .as_deref()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(bundle.document.dimensions.skill_revisions.len(), 2);
    assert_eq!(bundle.document.runs[1].skill_revision_id, "old");
}

#[test]
fn build_report_bundle_hashes_and_paths() {
    let fs = MemFS::new();
    let skill_path = Path::new("demo-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(
        skill_path.join("evals/evals.json"),
        r#"{
                "skill_name": "demo-skill",
                "evals": [
                    {
                        "id": "1",
                        "prompt": "p",
                        "expected_output": "o",
                        "graders": [{ "type": "contains", "text": "a" }]
                    }
                ]
            }"#,
    );

    let bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("demo-skill"),
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            report_id: Some("test-report-id".to_string()),
            generated_at: Some("2026-05-25T22:00:00Z".to_string()),
            iteration: Some(1),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(bundle.report_id, "test-report-id");
    assert_eq!(bundle.document.report.iteration, 1);
    assert!(bundle.document.suite.skill_hash.starts_with("sha256:"));
    assert!(bundle.document.suite.evals_hash.starts_with("sha256:"));
    assert_eq!(bundle.document.suite.skill_path, "demo-skill");
    assert_eq!(bundle.document.runs.len(), 1);
    assert_eq!(bundle.workspace_dirs, vec!["runs/run-001/workspace"]);
}

#[test]
fn a_narrowed_bundle_holds_runs_for_the_covered_cases_only() {
    let fs = MemFS::new();
    let skill_path = Path::new("demo-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [
                    { "id": "parse-csv", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] },
                    { "id": "parse-json", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] },
                    { "id": "render-chart", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }
                ]
            }"#,
        );

    let bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("demo-skill"),
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            iteration: Some(1),
            cases: CaseSelection::parse(&["parse-*".to_string()], &[], None).unwrap(),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    let covered: Vec<&str> = bundle
        .document
        .runs
        .iter()
        .map(|run| run.eval_case_id.as_str())
        .collect();
    assert_eq!(covered, vec!["parse-csv", "parse-json"]);

    let record = bundle.document.suite.case_selection.as_ref().unwrap();
    assert_eq!(record.cases, vec!["parse-*".to_string()]);
    assert_eq!(record.covered, 2);
    assert_eq!(
        record.declared,
        vec![
            DeclaredEvalCase {
                id: "parse-csv".to_string(),
                split: Some(EvalSplit::Train)
            },
            DeclaredEvalCase {
                id: "parse-json".to_string(),
                split: Some(EvalSplit::Train)
            },
            DeclaredEvalCase {
                id: "render-chart".to_string(),
                split: Some(EvalSplit::Train)
            },
        ]
    );
}

#[test]
fn a_full_bundle_records_no_case_selection() {
    let fs = MemFS::new();
    let skill_path = Path::new("demo-skill");
    fs.insert(
        skill_path.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: d\n---\n",
    );
    fs.insert(
            skill_path.join("evals/evals.json"),
            r#"{
                "skill_name": "demo-skill",
                "evals": [{ "id": "1", "prompt": "p", "expected_output": "o", "graders": [{ "type": "contains", "text": "a" }] }]
            }"#,
        );

    let bundle = build_report_bundle(
        &fs,
        skill_path,
        Path::new("demo-skill"),
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill],
        BuildReportOptions {
            iteration: Some(1),
            ..BuildReportOptions::default()
        },
    )
    .unwrap();

    assert!(bundle.document.suite.case_selection.is_none());
}

#[test]
fn write_report_bundle_creates_report_and_empty_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let bundle = ReportBundle {
        report_id: "report-123".to_string(),
        skill_name: "demo-skill".to_string(),
        document: ReportDocument {
            report: ReportSection {
                id: "report-123".to_string(),
                generated_at: "2026-05-25T22:00:00Z".to_string(),
                iteration: 1,
                producer: ProducerSection {
                    name: "trg".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                runner: None,
                runner_binary: None,
                runner_version: None,
                environment: EnvironmentPolicy::default(),
                permission: PermissionOutcome::default(),
                allowed_tools: None,
                telemetry_forwarding: TelemetryForwarding::Off,
                ci: None,
                trace: None,
            },
            suite: SuiteSection {
                skill_name: "demo-skill".to_string(),
                skill_path: "demo-skill".to_string(),
                skill_hash: "sha256:deadbeef".to_string(),
                evals_path: "demo-skill/evals/evals.json".to_string(),
                evals_hash: "sha256:feedface".to_string(),
                eval_dir: EvalDirName::default(),
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
            runs: Vec::new(),
            assertion_results: Vec::new(),
            summaries: SummariesSection {
                by_scenario: Vec::new(),
                human_feedback: None,
            },
            improvement_feedback: Vec::new(),
            comparisons: Vec::new(),
            iteration_summary: None,
            budget: None,
        },
        workspace_dirs: vec!["runs/run-001/workspace".to_string()],
    };

    let report_dir = write_report_bundle(
        temp.path(),
        &bundle,
        WriteReportOptions {
            force: false,
            iteration: 1,
        },
    )
    .unwrap();
    assert!(report_dir.join("report.json").is_file());
    assert!(report_dir.join("iteration-1/benchmark.json").is_file());
    let workspace_dir = report_dir.join("runs/run-001/workspace");
    assert!(workspace_dir.is_dir());
    assert!(workspace_dir.join(OUTPUTS_DIR).is_dir());
    assert_eq!(std::fs::read_dir(&workspace_dir).unwrap().count(), 1);

    let parsed: std::collections::HashMap<String, serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    assert!(
        !parsed.contains_key("budget"),
        "a report nothing ever priced must not claim a budget section"
    );
}

mod cache_tokens_tests;

mod run_cost_tests;

mod schema_fixtures_tests;
