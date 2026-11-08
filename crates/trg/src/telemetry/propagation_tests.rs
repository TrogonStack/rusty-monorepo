use super::*;

#[test]
fn carrier_pairs_empty_when_traces_inactive() {
    assert!(carrier_pairs().is_empty());
}

#[test]
fn carrier_pairs_carry_the_current_span() {
    let ((), trace) = crate::telemetry::testing::capture(|| {
        let span = tracing::info_span!("parent");
        let _entered = span.enter();
        let pairs = carrier_pairs();
        let traceparent = pairs
            .iter()
            .find(|(key, _)| key == "TRACEPARENT")
            .map(|(_, value)| value.clone());
        assert!(traceparent.is_some_and(|value| value.starts_with("00-")));
    });
    assert!(trace.span("parent").is_some());
}

#[test]
fn lane_parent_nests_work_on_another_thread() {
    let ((), trace) = crate::telemetry::testing::capture(|| {
        let span = tracing::info_span!("execute");
        let _entered = span.enter();
        let parent = LaneParent::capture();
        std::thread::scope(|scope| {
            scope.spawn(|| parent.in_scope(|| tracing::info_span!("lane work").in_scope(|| ())));
        });
    });
    trace.assert_child_of("lane work", "execute");
}

#[test]
fn extract_from_env_ignores_absent_vars() {
    let context = extract_from_env(&crate::telemetry::env::fixed(&[]));
    assert!(!context.span().span_context().is_valid());
}

#[test]
fn extract_from_env_reads_an_inbound_traceparent() {
    let env =
        crate::telemetry::env::fixed(&[("TRACEPARENT", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")]);
    let context = extract_from_env(&env);
    let span_context = context.span().span_context().clone();
    assert!(span_context.is_remote());
    assert_eq!(span_context.trace_id().to_string(), "4bf92f3577b34da6a3ce929d0e0e4736");
}
