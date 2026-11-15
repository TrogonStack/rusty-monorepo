use super::*;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing_subscriber::prelude::*;

#[test]
fn a_span_with_no_trace_exporter_has_nothing_to_reference() {
    assert_eq!(SpanReference::of(&tracing::info_span!("untraced")), None);
}

#[test]
fn a_traced_span_round_trips_through_its_report_encoding() {
    let (reference, trace) = crate::telemetry::testing::capture(|| {
        let span = tracing::info_span!("run");
        SpanReference::of(&span).expect("traced span has an identity")
    });
    let run = trace.span("run").expect("run exported");
    assert_eq!(reference.span_id(), run.span_context.span_id());
    assert_eq!(reference.trace_id(), run.span_context.trace_id());

    let json = serde_json::to_value(reference).unwrap();
    assert_eq!(json["span_id"], run.span_context.span_id().to_string());
    let back: SpanReference = serde_json::from_value(json).unwrap();
    assert_eq!(back, reference);
    assert!(back.span_context().is_remote());
}

#[test]
fn a_span_the_sampler_dropped_has_nothing_to_reference() {
    crate::telemetry::testing::consult_every_dispatcher();
    let provider = SdkTracerProvider::builder().with_sampler(Sampler::AlwaysOff).build();
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("trg")));

    let (valid, reference) = tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("run");
        let valid = span.context().span().span_context().is_valid();
        (valid, SpanReference::of(&span))
    });

    assert!(valid, "the dropped span still has an identity of its own");
    assert_eq!(reference, None);
}

#[test]
fn a_sampled_span_is_referenced_and_linked_to_as_sampled() {
    let (reference, _) = crate::telemetry::testing::capture(|| {
        SpanReference::of(&tracing::info_span!("run")).expect("a sampled span is referenced")
    });
    assert!(reference.span_context().is_sampled());
}

#[test]
fn a_span_context_the_sampler_dropped_has_nothing_to_reference() {
    let context = |flags| {
        SpanContext::new(
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            flags,
            false,
            TraceState::default(),
        )
    };

    assert_eq!(SpanReference::of_context(&context(TraceFlags::default())), None);
    assert!(SpanReference::of_context(&context(TraceFlags::SAMPLED)).is_some());
}

#[test]
fn an_all_zero_id_is_refused_rather_than_linked_to() {
    let json = serde_json::json!({"trace_id": "0".repeat(32), "span_id": "00f067aa0ba902b7"});
    assert!(serde_json::from_value::<SpanReference>(json).is_err());
}
