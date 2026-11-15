use super::*;
use crate::agentskills::report::{
    build_report_bundle, write_report_bundle, BuildReportOptions, ScenarioKind, WriteReportOptions,
};
use crate::fs::testutil::MemFS;
use std::path::Path;

fn sample_skill(fs: &MemFS) -> PathBuf {
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
                        "id": "case-a",
                        "prompt": "prompt a",
                        "expected_output": "output a",
                        "graders": [{ "type": "contains", "text": "assert a" }]
                    },
                    {
                        "id": "case-b",
                        "prompt": "prompt b",
                        "expected_output": "output b",
                        "graders": [{ "type": "contains", "text": "assert b" }]
                    }
                ]
            }"#,
    );
    skill_path.to_path_buf()
}

fn write_workspace_output(report_dir: &Path, workspace: &str, content: &str) {
    let path = report_dir.join(workspace).join("output.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn mark_all_runs_completed(report_dir: &Path) {
    let report_path = report_dir.join("report.json");
    let mut report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    for run in report["runs"].as_array_mut().unwrap() {
        run["status"] = serde_json::json!("completed");
    }
    std::fs::write(report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
}

#[test]
fn load_scenario_output_prefers_latest_completed_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let report = serde_json::json!({
        "runs": [
            {
                "eval_case_id": "case-a",
                "scenario_id": "with_skill",
                "attempt": 1,
                "status": "failed",
                "paths": { "workspace": "runs/run-001/workspace" }
            },
            {
                "eval_case_id": "case-a",
                "scenario_id": "with_skill",
                "attempt": 2,
                "status": "completed",
                "paths": { "workspace": "runs/run-002/workspace" }
            },
            {
                "eval_case_id": "case-a",
                "scenario_id": "with_skill",
                "attempt": 3,
                "status": "skipped",
                "paths": { "workspace": "runs/run-003/workspace" }
            }
        ]
    });
    let runs: Vec<LoadedRun> = serde_json::from_value(report["runs"].clone()).unwrap();

    write_workspace_output(temp.path(), "runs/run-001/workspace", "attempt-1");
    write_workspace_output(temp.path(), "runs/run-002/workspace", "attempt-2");
    write_workspace_output(temp.path(), "runs/run-003/workspace", "attempt-3");

    let output = load_scenario_output(temp.path(), &runs, "case-a", ScenarioKind::WithSkill).unwrap();
    assert_eq!(output.text, "attempt-2");
}

#[test]
fn find_iteration_eval_dir_prefers_latest_iteration() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(temp.path().join("iteration-1/case-a")).unwrap();
    std::fs::create_dir_all(temp.path().join("iteration-2/case-a")).unwrap();
    std::fs::create_dir_all(temp.path().join("iteration-3/case-b")).unwrap();

    assert_eq!(
        find_iteration_eval_dir(temp.path(), "case-a"),
        Some(temp.path().join("iteration-2/case-a"))
    );
    assert_eq!(
        find_iteration_eval_dir(temp.path(), "case-b"),
        Some(temp.path().join("iteration-3/case-b"))
    );
    assert_eq!(find_iteration_eval_dir(temp.path(), "missing"), None);
}

#[test]
fn merge_comparisons_into_report_rejects_non_object_root() {
    let temp = tempfile::tempdir().unwrap();
    let report_path = temp.path().join("report.json");
    std::fs::write(&report_path, "[]").unwrap();

    let error = merge_comparisons_into_report(&report_path, "[]", &[]).unwrap_err();
    assert!(error.to_string().contains("root must be a JSON object"));
}

#[test]
fn shuffle_swap_is_deterministic_per_eval_id() {
    assert_eq!(shuffle_swap("case-a"), shuffle_swap("case-a"));
    assert_eq!(shuffle_swap("case-b"), shuffle_swap("case-b"));
}

#[test]
fn shuffle_swap_varies_by_eval_id() {
    let values: Vec<bool> = ["case-a", "case-b", "case-c", "case-d"]
        .into_iter()
        .map(shuffle_swap)
        .collect();
    assert!(values.contains(&true) || values.contains(&false));
}

#[test]
fn build_blind_pair_hides_scenario_identity() {
    let (mapping, outputs) = build_blind_pair(
        "case-a",
        ScenarioKind::WithSkill,
        ScenarioKind::WithoutSkill,
        "with-skill-output".to_string(),
        "without-skill-output".to_string(),
    );

    let payload = blind_judge_payload(&outputs);
    let serialized = payload.to_string();
    assert!(!serialized.contains("with_skill"));
    assert!(!serialized.contains("without_skill"));
    assert!(outputs.contains_key("A"));
    assert!(outputs.contains_key("B"));

    if mapping.label_a == ScenarioKind::WithSkill {
        assert_eq!(outputs.get("A").unwrap(), "with-skill-output");
        assert_eq!(outputs.get("B").unwrap(), "without-skill-output");
    } else {
        assert_eq!(outputs.get("A").unwrap(), "without-skill-output");
        assert_eq!(outputs.get("B").unwrap(), "with-skill-output");
    }
}

#[test]
fn run_comparisons_with_script_judge_updates_report_and_iteration_json() {
    let temp = tempfile::tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

    for run in &bundle.document.runs {
        write_workspace_output(
            &report_dir,
            &run.paths.workspace,
            &format!("{}-output", run.scenario_id.as_str()),
        );
    }
    mark_all_runs_completed(&report_dir);

    let iteration_dir = report_dir.join("iteration-1").join("case-a");
    std::fs::create_dir_all(&iteration_dir).unwrap();

    let judge_script = temp.path().join("judge.py");
    std::fs::write(
        &judge_script,
        r#"import json, sys
payload = json.load(sys.stdin)
print(json.dumps({"winner": "A", "evidence": "A is clearer"}))
"#,
    )
    .unwrap();

    let records = run_comparisons(
        &report_dir,
        CompareOptions {
            pairs: vec![ScenarioPair {
                a: ScenarioKind::WithSkill,
                b: ScenarioKind::WithoutSkill,
            }],
            judge: JudgeKind::Script,
            judge_provider: JudgeProvider::default(),
            judge_model: None,
            judge_command: Some(format!("python3 {}", judge_script.display())),
            emit_comparison_json: true,
        },
    )
    .unwrap();

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].eval_case_id, "case-a");
    assert_eq!(records[0].winner, ComparisonWinner::A);
    assert_eq!(records[0].evidence, "A is clearer");

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(report.get("comparisons").and_then(|v| v.as_array()).unwrap().len(), 2);

    let comparison_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(iteration_dir.join("comparison.json")).unwrap()).unwrap();
    assert_eq!(
        comparison_json.get("eval_case_id").and_then(|v| v.as_str()),
        Some("case-a")
    );
}

fn reference(trace_id: &str, span_id: &str) -> SpanReference {
    serde_json::from_value(serde_json::json!({ "trace_id": trace_id, "span_id": span_id })).unwrap()
}

/// A report whose case-a runs record the spans they executed and were graded under.
struct TracedReport {
    _temp: tempfile::TempDir,
    report_dir: PathBuf,
    recorded: Vec<SpanReference>,
}

fn traced_report(completed: bool) -> TracedReport {
    let temp = tempfile::tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();
    for run in &bundle.document.runs {
        write_workspace_output(&report_dir, &run.paths.workspace, run.scenario_id.as_str());
    }
    if completed {
        mark_all_runs_completed(&report_dir);
    }

    let report_path = report_dir.join("report.json");
    let mut report: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    let mut recorded = Vec::new();
    let case_a_runs = report["runs"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter(|run| run["eval_case_id"] == "case-a");
    for (index, run) in case_a_runs.enumerate() {
        let run_trace = reference(&format!("{:032x}", index + 1), &format!("{:016x}", index + 1));
        let grade_trace = reference(&format!("{:032x}", index + 11), &format!("{:016x}", index + 11));
        run["trace"] = serde_json::to_value(run_trace).unwrap();
        run["grade_trace"] = serde_json::to_value(grade_trace).unwrap();
        recorded.extend([run_trace, grade_trace]);
    }
    std::fs::write(&report_path, serde_json::to_string_pretty(&report).unwrap()).unwrap();

    TracedReport {
        _temp: temp,
        report_dir,
        recorded,
    }
}

fn script_judge_options() -> CompareOptions {
    CompareOptions {
        pairs: vec![ScenarioPair {
            a: ScenarioKind::WithSkill,
            b: ScenarioKind::WithoutSkill,
        }],
        judge: JudgeKind::Script,
        judge_provider: JudgeProvider::default(),
        judge_model: None,
        judge_command: Some(r#"cat >/dev/null; echo '{"winner": "A", "evidence": "A is clearer"}'"#.to_string()),
        emit_comparison_json: true,
    }
}

fn pair_span_of<'a>(
    trace: &'a crate::telemetry::testing::CapturedTrace,
    case_id: &str,
) -> &'a opentelemetry_sdk::trace::SpanData {
    let case = trace
        .span_where("compare case", EVAL_CASE_ID, case_id)
        .unwrap_or_else(|| panic!("no span for {case_id}"));
    let pairs = trace.children_of(case);
    assert_eq!(pairs.len(), 1, "one span per pair under {case_id}");
    pairs[0]
}

#[test]
fn a_comparison_links_both_runs_and_their_grades_and_nests_its_phases() {
    let traced = traced_report(true);

    let (records, trace) =
        crate::telemetry::testing::capture(|| run_comparisons(&traced.report_dir, script_judge_options()));

    assert_eq!(records.unwrap().len(), 2);
    let compare = trace.span("compare").unwrap();
    for case_id in ["case-a", "case-b"] {
        let case = trace.span_where("compare case", EVAL_CASE_ID, case_id).unwrap();
        assert_eq!(case.parent_span_id, compare.span_context.span_id(), "{case_id}");
    }
    trace.assert_child_of("write report", "compare");

    let pair = pair_span_of(&trace, "case-a");
    assert_eq!(pair.name, "compare pair");
    let attribute = |key: &str| {
        pair.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    };
    assert_eq!(attribute(EVAL_CASE_ID), Some(opentelemetry::Value::from("case-a")));
    assert_eq!(
        attribute(EVAL_COMPARISON_PAIR),
        Some(opentelemetry::Value::from("with_skill:without_skill"))
    );
    let phases: Vec<&str> = trace.children_of(pair).iter().map(|span| span.name.as_ref()).collect();
    for phase in ["load outputs", "build blind pair", "sh", "write comparison"] {
        assert!(
            phases.contains(&phase),
            "{phase:?} is not under the pair span: {phases:?}"
        );
    }
    let judge = trace
        .children_of(pair)
        .into_iter()
        .find(|span| span.name == "sh")
        .unwrap();
    assert_eq!(judge.span_kind, opentelemetry::trace::SpanKind::Client);

    let linked: Vec<(opentelemetry::trace::TraceId, opentelemetry::trace::SpanId)> = pair
        .links
        .iter()
        .map(|link| (link.span_context.trace_id(), link.span_context.span_id()))
        .collect();
    assert_eq!(linked.len(), traced.recorded.len());
    for reference in &traced.recorded {
        assert!(
            linked.contains(&(reference.trace_id(), reference.span_id())),
            "the pair span links to every run and grade span its outputs came from"
        );
    }
    assert!(
        pair_span_of(&trace, "case-b").links.is_empty(),
        "runs that recorded no span have nothing to link to"
    );
}

#[test]
fn a_comparison_verdict_is_reported_in_the_context_of_its_pair_span() {
    let traced = traced_report(true);

    let (records, trace, logs) =
        super::telemetry::testing::capture_with_logs(|| run_comparisons(&traced.report_dir, script_judge_options()));

    records.unwrap();
    let verdicts: Vec<_> = logs
        .iter()
        .filter(|record| record.event_name() == Some("gen_ai.evaluation.result"))
        .collect();
    assert_eq!(verdicts.len(), 2, "one verdict per case and pair");
    let pairs = [pair_span_of(&trace, "case-a"), pair_span_of(&trace, "case-b")];
    for verdict in verdicts {
        let context = verdict.trace_context().expect("a verdict carries a trace context");
        assert!(
            pairs.iter().any(|pair| context.trace_id == pair.span_context.trace_id()
                && context.span_id == pair.span_context.span_id()),
            "a verdict lands under the pair span that judged it"
        );
    }
}

#[test]
fn a_pair_without_a_completed_run_fails_its_phase_its_case_and_the_comparison() {
    let traced = traced_report(false);

    let (records, trace) =
        crate::telemetry::testing::capture(|| run_comparisons(&traced.report_dir, script_judge_options()));

    assert!(records.is_err());
    let validation = Some(opentelemetry::Value::from("validation"));
    let failed = [
        trace.span("load outputs").unwrap(),
        trace.span_where("compare case", EVAL_CASE_ID, "case-a").unwrap(),
        trace.span("compare").unwrap(),
    ];
    for span in failed {
        let error_type = span
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == ERROR_TYPE)
            .map(|kv| kv.value.clone());
        assert_eq!(error_type, validation, "{}", span.name);
        assert!(
            matches!(span.status, opentelemetry::trace::Status::Error { .. }),
            "{}",
            span.name
        );
    }
    assert_eq!(trace.attribute("compare pair", ERROR_TYPE), validation);
    assert!(
        trace.span("write report").is_none(),
        "nothing is written after a failed pair"
    );
}

#[test]
fn run_comparisons_skips_when_judge_is_none() {
    let temp = tempfile::tempdir().unwrap();
    let fs = MemFS::new();
    let skill_path = sample_skill(&fs);
    let bundle = build_report_bundle(
        &fs,
        &skill_path,
        &skill_path,
        "demo-skill",
        "ci-default",
        &[ScenarioKind::WithSkill, ScenarioKind::WithoutSkill],
        BuildReportOptions::default(),
    )
    .unwrap();
    let report_dir = write_report_bundle(temp.path(), &bundle, WriteReportOptions::default()).unwrap();

    let records = run_comparisons(
        &report_dir,
        CompareOptions {
            pairs: vec![ScenarioPair {
                a: ScenarioKind::WithSkill,
                b: ScenarioKind::WithoutSkill,
            }],
            judge: JudgeKind::None,
            judge_provider: JudgeProvider::default(),
            judge_model: None,
            judge_command: None,
            emit_comparison_json: false,
        },
    )
    .unwrap();

    assert!(records.is_empty());
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report_dir.join("report.json")).unwrap()).unwrap();
    assert_eq!(report.get("comparisons").and_then(|v| v.as_array()).unwrap().len(), 0);
}
