use super::testing::capture_with_logs;
use super::*;

fn evaluation() -> Evaluation<'static> {
    Evaluation {
        name: "contains",
        label: Some("fail"),
        score: Some(0.0),
        explanation: Some("the answer quoted the customer's address"),
        grader_kind: Some("contains"),
        case_id: "case",
        run_id: Some("run"),
        provenance: Provenance::Live,
    }
}

#[test]
fn an_event_only_evaluation_carries_its_explanation_on_the_log_and_nothing_onto_the_span() {
    let ((), trace, logs) = capture_with_logs(|| {
        tracing::info_span!("grade").in_scope(|| evaluation().emit(None, ContentCapture::EventOnly));
    });

    let grade = trace.span("grade").expect("grade span exported");
    assert!(
        grade.events.is_empty(),
        "an evaluation never becomes a span event: {:?}",
        grade.events
    );
    assert!(grade
        .attributes
        .iter()
        .all(|kv| kv.key.as_str() != GEN_AI_EVALUATION_EXPLANATION));

    let record = logs
        .iter()
        .find(|record| record.event_name() == Some("gen_ai.evaluation.result"))
        .expect("the evaluation reaches the logs signal");
    let explanation = record
        .attributes_iter()
        .find(|(key, _)| key.as_str() == GEN_AI_EVALUATION_EXPLANATION)
        .map(|(_, value)| format!("{value:?}"));
    assert!(
        explanation.is_some_and(|value| value.contains("the customer's address")),
        "the log record keeps the explanation event capture allows"
    );
    assert_eq!(
        record.trace_context().map(|context| context.span_id),
        Some(grade.span_context.span_id()),
        "the log record still correlates with the span it was emitted under"
    );
}
