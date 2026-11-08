use super::*;
use crate::telemetry::testing::{capture, CapturedTrace};
use opentelemetry::trace::SpanKind;
use opentelemetry_sdk::trace::SpanData;

const SUITE: &str = "invoke_workflow lane-skill";

fn traced_pass(concurrency: usize) -> (serde_json::Value, CapturedTrace) {
    super::super::fake_runner::reset();
    if concurrency > 1 {
        super::super::fake_runner::expect_lanes(concurrency);
    }
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let mut args = base_run_args(&skill_dir, &temp.path().join("artifacts"));
    args.concurrency = RunConcurrency::parse(concurrency).unwrap();
    let (report_dir, trace) = capture(|| run_with_fake_runner(args));
    (read_report(&report_dir), trace)
}

fn names(spans: &[&SpanData]) -> Vec<String> {
    let mut spans = spans.to_vec();
    spans.sort_by_key(|span| span.start_time);
    spans.iter().map(|span| span.name.to_string()).collect()
}

fn string_attribute(span: &SpanData, key: &str) -> Option<String> {
    value_attribute(span, key).map(|value| value.to_string())
}

fn value_attribute(span: &SpanData, key: &str) -> Option<opentelemetry::Value> {
    span.attributes
        .iter()
        .find(|kv| kv.key.as_str() == key)
        .map(|kv| kv.value.clone())
}

#[test]
fn a_pass_puts_its_setup_steps_in_order_under_the_workflow_span() {
    let (_, trace) = traced_pass(1);
    let suite = trace.span(SUITE).expect("workflow span exported");
    assert_eq!(suite.span_kind, SpanKind::Internal);
    assert_eq!(
        string_attribute(suite, GEN_AI_OPERATION_NAME).as_deref(),
        Some(gen_ai_operation_name::INVOKE_WORKFLOW)
    );
    assert_eq!(
        string_attribute(suite, GEN_AI_WORKFLOW_NAME).as_deref(),
        Some("lane-skill")
    );
    assert_eq!(string_attribute(suite, EVAL_SCENARIOS).as_deref(), Some("with_skill"));
    assert_eq!(string_attribute(suite, EVAL_GRADING_STRATEGY).as_deref(), Some("none"));
    assert!(string_attribute(suite, EVAL_SKILL_REVISION).is_some());

    assert_eq!(
        names(&trace.children_of(suite)),
        [
            "validate skill",
            "check suite",
            "detect iteration",
            "resolve scenarios",
            "open cost ledger",
            "admit skill",
            "build report bundle",
            "write report bundle",
            "load run inputs",
            "execute runs",
            "rewrite report",
            "finish suite",
        ]
    );
}

#[test]
fn each_run_and_its_attempts_nest_under_execute_runs() {
    let (report, trace) = traced_pass(1);
    let execute = trace.span("execute runs").expect("execute runs exported");
    let runs = trace.spans_named("run");
    assert_eq!(runs.len(), 2);
    for run in &runs {
        assert_eq!(run.parent_span_id, execute.span_context.span_id());
        assert_eq!(
            names(&trace.children_of(run)),
            [
                "resolve mocks",
                "hash fixtures",
                "compute cache key",
                "look up cache",
                "admit budget",
                "hash skill",
                "attempt",
                "apply outcome",
                "check skill tampering",
            ]
        );
        assert_eq!(value_attribute(run, EVAL_LANE), Some(opentelemetry::Value::I64(0)));
        assert_eq!(string_attribute(run, EVAL_RUN_STATUS).as_deref(), Some("completed"));
    }
    let attempt = trace.span("attempt").expect("attempt exported");
    assert_eq!(
        value_attribute(attempt, EVAL_ATTEMPT),
        Some(opentelemetry::Value::I64(1))
    );
    assert_eq!(
        string_attribute(attempt, EVAL_ATTEMPT_OUTCOME).as_deref(),
        Some("completed")
    );

    let first = runs
        .iter()
        .find(|run| string_attribute(run, EVAL_CASE_ID).as_deref() == Some("one"))
        .unwrap();
    assert_eq!(
        report["runs"][0]["trace"]["span_id"],
        first.span_context.span_id().to_string()
    );
    let suite = trace.span(SUITE).unwrap();
    assert_eq!(
        report["report"]["trace"]["span_id"],
        suite.span_context.span_id().to_string()
    );
    assert_eq!(
        report["report"]["trace"]["trace_id"],
        suite.span_context.trace_id().to_string()
    );
}

#[test]
fn lanes_keep_their_runs_under_execute_runs() {
    let (_, trace) = traced_pass(2);
    assert_eq!(super::super::fake_runner::peak_lanes(), 2);
    let execute = trace.span("execute runs").expect("execute runs exported");
    let runs = trace.spans_named("run");
    assert_eq!(runs.len(), 2);
    for run in &runs {
        assert_eq!(run.parent_span_id, execute.span_context.span_id());
        assert!(trace.children_of(run).iter().any(|child| child.name == "attempt"));
    }
    let mut lanes = runs
        .iter()
        .filter_map(|run| string_attribute(run, EVAL_LANE))
        .collect::<Vec<_>>();
    lanes.sort();
    lanes.dedup();
    assert_eq!(lanes.len(), 2, "each lane records which lane it was: {lanes:?}");
    assert_eq!(
        names(&trace.children_of(execute))
            .into_iter()
            .filter(|name| name != "run")
            .collect::<Vec<_>>(),
        ["hash skills", "check skill tampering"]
    );
}

#[test]
fn the_workflow_spans_children_account_for_nearly_all_of_it() {
    // The first pass on a thread pays for registering every callsite, which is
    // not time the pass itself spends anywhere.
    traced_pass(1);
    // Scheduling noise only ever adds time no child covers, so the best of a few
    // passes is the one that shows whether the pass leaves any of its own work
    // untraced.
    let (covered, total) = (0..5)
        .map(|_| {
            let (_, trace) = traced_pass(1);
            let suite = trace.span(SUITE).unwrap();
            let total = suite.end_time.duration_since(suite.start_time).unwrap();
            let covered: std::time::Duration = trace
                .children_of(suite)
                .iter()
                .map(|child| child.end_time.duration_since(child.start_time).unwrap())
                .sum();
            (covered, total)
        })
        .max_by(|(a_covered, a_total), (b_covered, b_total)| {
            (a_covered.as_secs_f64() / a_total.as_secs_f64())
                .total_cmp(&(b_covered.as_secs_f64() / b_total.as_secs_f64()))
        })
        .unwrap();
    assert!(
        covered.as_secs_f64() >= total.as_secs_f64() * 0.9,
        "children cover {covered:?} of {total:?}"
    );
}

#[test]
fn an_untraced_pass_writes_no_trace_reference() {
    super::super::fake_runner::reset();
    let temp = tempfile::tempdir().unwrap();
    let skill_dir = write_two_case_skill(temp.path());
    let report = read_report(&run_with_fake_runner(base_run_args(
        &skill_dir,
        &temp.path().join("artifacts"),
    )));
    assert!(report["report"].get("trace").is_none());
    assert!(report["runs"][0].get("trace").is_none());
}
