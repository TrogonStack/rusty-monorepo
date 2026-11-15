use super::*;
use crate::agentskills::evals::{EvalDirName, EvalSplit};
use crate::agentskills::grading::{GraderInfo, GraderKind};
use crate::agentskills::report::{
    CiSection, DimensionsSection, ProducerSection, ReportSection, RunMetrics, RunPaths, SuiteSection, SummariesSection,
};
use crate::agentskills::transcript_metrics::{ObservedToolCall, TurnCount};

const SCRIPT_PAYLOAD: &str = "<script>alert(1)</script>";
const IMG_PAYLOAD: &str = "\"><img src=x onerror=alert(1)>";

fn minimal_document() -> ReportDocument {
    ReportDocument {
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
            telemetry_forwarding: crate::agentskills::runner::TelemetryForwarding::Off,
            ci: None::<CiSection>,
            trace: None,
        },
        suite: SuiteSection {
            skill_name: "demo-skill".to_string(),
            skill_path: "demo-skill".to_string(),
            skill_hash: "sha256:abc".to_string(),
            evals_path: "demo-skill/evals/evals.json".to_string(),
            evals_hash: "sha256:def".to_string(),
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
        runs: vec![RunRecord {
            runner_model: None,
            runner_model_source: None,
            id: "run-001".to_string(),
            eval_case_id: SCRIPT_PAYLOAD.to_string(),
            eval_slug: "case".to_string(),
            split: EvalSplit::Train,
            scenario_id: ScenarioKind::WithSkill,
            iteration: 1,
            model_config_id: "default".to_string(),
            skill_revision_id: "current".to_string(),
            attempt: 1,
            status: "completed".to_string(),
            runner_invocations: 1,
            failure_kind: None,
            paths: RunPaths {
                workspace: "runs/run-001/workspace".to_string(),
                outputs: "runs/run-001/workspace/outputs".to_string(),
            },
            mirror_path: "iteration-1/eval-case/with_skill/".to_string(),
            tool_grant: None,
            artifacts: vec![serde_json::json!({
                "kind": "output",
                "path": IMG_PAYLOAD,
            })],
            metrics: RunMetrics::default(),
            cache: None,
            skill_integrity: None,
            read_only_fixture_violations: Vec::new(),
            warnings: Vec::new(),
            mock_violations: Vec::new(),
            case_score: None,
            trace: None,
            grade_trace: None,
        }],
        assertion_results: Vec::new(),
        summaries: SummariesSection {
            by_scenario: Vec::new(),
            human_feedback: None,
        },
        improvement_feedback: Vec::new(),
        comparisons: Vec::new(),
        iteration_summary: None,
        budget: None,
    }
}

#[test]
fn escapes_the_five_reserved_characters() {
    assert_eq!(esc("a & b").to_string(), "a &amp; b");
    assert_eq!(esc("<tag>").to_string(), "&lt;tag&gt;");
    assert_eq!(esc("\"quoted\"").to_string(), "&quot;quoted&quot;");
    assert_eq!(esc("it's").to_string(), "it&#39;s");
}

#[test]
fn an_href_percent_encodes_what_a_browser_would_otherwise_read_as_url_syntax() {
    assert_eq!(Href::new("outputs/a#b.md").to_string(), "outputs/a%23b.md");
    assert_eq!(Href::new("outputs/a?b.md").to_string(), "outputs/a%3Fb.md");
    assert_eq!(Href::new("outputs/a%2Fb.md").to_string(), "outputs/a%252Fb.md");
    assert_eq!(Href::new("outputs/my report.md").to_string(), "outputs/my%20report.md");
}

#[test]
fn an_href_keeps_separators_and_unreserved_characters_intact() {
    assert_eq!(
        Href::new("outputs/sub-dir/final_v1.2~draft.md").to_string(),
        "outputs/sub-dir/final_v1.2~draft.md"
    );
}

#[test]
fn an_href_cannot_close_the_attribute_it_sits_in() {
    let rendered = Href::new(IMG_PAYLOAD).to_string();
    for reserved in ['"', '<', '>', '\'', '&'] {
        assert!(!rendered.contains(reserved), "href still carries {reserved}");
    }
}

#[test]
fn injected_markup_in_an_assertion_evidence_string_does_not_survive_rendering() {
    let assertion = AssertionGradeResult {
        assertion: "outputs/report.md exists".to_string(),
        passed: true,
        evidence: SCRIPT_PAYLOAD.to_string(),
        grader: GraderInfo {
            kind: GraderKind::Mechanical,
            model: None,
            command: None,
        },
        name: None,
        rationale: Some(IMG_PAYLOAD.to_string()),
        unsupported: None,
        excluded: None,
        ungraded: None,
        votes: None,
        weight: None,
    };

    let html = render_assertion(&assertion);

    assert!(!html.contains(SCRIPT_PAYLOAD));
    assert!(!html.contains(IMG_PAYLOAD));
    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;"));
}

#[test]
fn injected_markup_in_a_file_path_does_not_survive_rendering() {
    let mut run = minimal_run();
    run.artifacts = vec![serde_json::json!({ "kind": "output", "path": IMG_PAYLOAD })];

    let html = render_run_artifacts(&run);

    assert!(!html.contains(IMG_PAYLOAD));
    assert!(html.contains("&quot;&gt;&lt;img src=x onerror=alert(1)&gt;"));
}

#[test]
fn injected_markup_in_a_runs_final_text_does_not_survive_rendering() {
    let report_dir = tempfile::tempdir().unwrap();
    let run = minimal_run();
    let outputs_dir = report_dir.path().join(&run.paths.workspace).join(OUTPUTS_DIR);
    fs::create_dir_all(&outputs_dir).unwrap();
    fs::write(outputs_dir.join(FINAL_MD), SCRIPT_PAYLOAD).unwrap();

    let final_text = load_run_final_text(report_dir.path(), &run).expect("final.md was written");
    let html = format!("<pre>{}</pre>", esc(&final_text));

    assert!(!html.contains(SCRIPT_PAYLOAD));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[test]
fn a_full_report_with_adversarial_ids_never_leaks_raw_markup() {
    let document = minimal_document();
    let temp = tempfile::tempdir().unwrap();
    let html = render_report_html(&document, temp.path());

    assert!(!html.contains(SCRIPT_PAYLOAD));
    assert!(!html.contains(IMG_PAYLOAD));
}

#[test]
fn the_summary_table_shows_pass_at_k_separately_from_pass_pow_k() {
    let mut document = minimal_document();
    document.summaries.by_scenario = vec![ScenarioSummary {
        scenario_id: ScenarioKind::WithSkill,
        total_runs: 4,
        passed_runs: 3,
        skipped_runs: 0,
        failed_runs: 1,
        pass_at_k: ScoredRate::from_counts(2, 2),
        pass_pow_k: ScoredRate::from_counts(1, 2),
    }];

    let html = render_summary(&document, ReportTotals::default());

    assert!(html.contains("<th>pass@k</th><th>pass^k</th>"));
    assert!(html.contains("<td>100% (2/2)</td><td>50% (1/2)</td></tr>"));
}

#[test]
fn a_scenario_with_no_graded_attempts_shows_not_scored_rather_than_a_rate() {
    let row = render_scenario_summary_row(&ScenarioSummary {
        scenario_id: ScenarioKind::WithSkill,
        total_runs: 0,
        passed_runs: 0,
        skipped_runs: 0,
        failed_runs: 0,
        pass_at_k: ScoredRate::default(),
        pass_pow_k: ScoredRate::default(),
    });

    assert!(row.contains("<td>not scored</td><td>not scored</td></tr>"));
}

#[test]
fn an_arm_where_every_attempt_passed_reports_yes_for_both_k_readings() {
    let mut document = minimal_document();
    document.runs[0].case_score = Some(1.0);

    let html = render_report_html(&document, tempfile::tempdir().unwrap().path());

    assert!(html.contains("pass@k: yes"));
    assert!(html.contains("pass^k: yes"));
}

/// A draw the harness stopped at a cap did not pass, even when grading scored the
/// answer it left behind, so a case that passed once and was then capped is not one
/// where every attempt passed, in the HTML report as much as in the iteration summary.
#[test]
fn a_capped_attempt_counts_against_pass_pow_k() {
    let mut passed = minimal_run();
    passed.case_score = Some(1.0);
    let mut capped = minimal_run();
    capped.case_score = Some(1.0);
    capped.status = "budget_exhausted".to_string();
    let arm = ArmGroup {
        scenario: ScenarioKind::WithSkill,
        runs: vec![&passed, &capped],
    };

    let line = render_case_k_outcome(&arm).unwrap();

    assert!(line.contains("pass@k: yes"), "{line}");
    assert!(line.contains("pass^k: no"), "{line}");
}

/// The report's own scenario rates read a capped draw the same way the arm line does.
#[test]
fn recomputed_scenario_rates_count_a_capped_attempt_as_a_failed_draw() {
    let mut document = minimal_document();
    document.runs[0].case_score = Some(1.0);
    let mut capped = minimal_run();
    capped.eval_case_id = document.runs[0].eval_case_id.clone();
    capped.case_score = None;
    capped.status = "budget_exhausted".to_string();
    document.runs.push(capped);
    document.summaries.by_scenario.push(ScenarioSummary {
        scenario_id: ScenarioKind::WithSkill,
        total_runs: 0,
        passed_runs: 0,
        skipped_runs: 0,
        failed_runs: 0,
        pass_at_k: ScoredRate::default(),
        pass_pow_k: ScoredRate::default(),
    });

    crate::agentskills::report::recompute_scenario_summaries(&mut document);

    let summary = &document.summaries.by_scenario[0];
    assert_eq!(summary.failed_runs, 1);
    assert_eq!((summary.pass_at_k.successes, summary.pass_at_k.trials), (1, 1));
    assert_eq!((summary.pass_pow_k.successes, summary.pass_pow_k.trials), (0, 1));
}

#[test]
fn an_arm_with_no_graded_attempt_renders_no_k_outcome_line() {
    let mut run = minimal_run();
    run.case_score = None;
    let arm = ArmGroup {
        scenario: ScenarioKind::WithSkill,
        runs: vec![&run],
    };

    assert_eq!(render_case_k_outcome(&arm), None);
}

#[test]
fn tool_usage_is_tabled_per_scenario_and_per_run_with_tool_names_escaped() {
    let mut document = minimal_document();
    let mut run = minimal_run();
    run.metrics.transcript = Some(TranscriptMetrics::measure(
        TurnCount::new(2),
        Some(76_000),
        [
            ObservedToolCall {
                tool: SCRIPT_PAYLOAD,
                arguments: Some("{}"),
                result: Some("12345"),
            },
            ObservedToolCall {
                tool: SCRIPT_PAYLOAD,
                arguments: Some("{}"),
                result: None,
            },
        ],
    ));
    document.runs = vec![run.clone()];

    let section = render_tool_usage(&document);
    let metrics = render_run_metrics(&run);

    assert!(section.contains("<h2>Tool usage</h2>"));
    assert!(section.contains(
        "<td>with_skill</td><td>&lt;script&gt;alert(1)&lt;/script&gt;</td><td>1</td><td>2</td><td>1</td><td>5</td>"
    ));
    assert!(!section.contains(SCRIPT_PAYLOAD));
    assert!(metrics.contains("2 tool calls (1 repeated)"));
    assert!(metrics.contains("peak turn input 76000 tokens"));
}

#[test]
fn a_report_with_no_measured_runs_has_no_tool_usage_section() {
    assert_eq!(render_tool_usage(&minimal_document()), "");
}

fn minimal_run() -> RunRecord {
    RunRecord {
        runner_model: None,
        runner_model_source: None,
        id: "run-001".to_string(),
        eval_case_id: "case".to_string(),
        eval_slug: "case".to_string(),
        split: EvalSplit::Train,
        scenario_id: ScenarioKind::WithSkill,
        iteration: 1,
        model_config_id: "default".to_string(),
        skill_revision_id: "current".to_string(),
        attempt: 1,
        status: "completed".to_string(),
        runner_invocations: 1,
        failure_kind: None,
        paths: RunPaths {
            workspace: "runs/run-001/workspace".to_string(),
            outputs: "runs/run-001/workspace/outputs".to_string(),
        },
        mirror_path: "iteration-1/eval-case/with_skill/".to_string(),
        tool_grant: None,
        artifacts: Vec::new(),
        metrics: RunMetrics::default(),
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

#[test]
fn write_html_report_reads_the_bundle_and_writes_report_html() {
    let report_dir = tempfile::tempdir().unwrap();
    let document = minimal_document();
    fs::write(
        report_dir.path().join("report.json"),
        serde_json::to_string_pretty(&document).unwrap(),
    )
    .unwrap();

    let out_path = write_html_report(report_dir.path()).unwrap();

    assert_eq!(out_path, report_dir.path().join(HTML_REPORT_FILENAME));
    let html = fs::read_to_string(&out_path).unwrap();
    assert!(html.contains("demo-skill"));
    assert!(!html.contains(SCRIPT_PAYLOAD));
}

#[test]
fn render_report_html_references_nothing_over_the_network() {
    let document = minimal_document();
    let temp = tempfile::tempdir().unwrap();
    let html = render_report_html(&document, temp.path());

    for marker in ["http://", "https://", "//cdn", "<script"] {
        assert!(!html.contains(marker), "found network or script marker: {marker}");
    }
}
