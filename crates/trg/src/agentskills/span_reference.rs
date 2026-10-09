//! Where a span recorded by an earlier invocation lives, kept in a report so a later
//! invocation (`grade`, `compare`, export) can link its own spans to it.

use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// A span's W3C trace and span id, serialized as lowercase hex.
///
/// Only ever written when tracing was active for the invocation that produced the report,
/// so its absence means "nothing to link to" rather than a broken reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpanReference {
    #[serde(serialize_with = "hex_trace_id", deserialize_with = "parse_trace_id")]
    #[schemars(with = "String", extend("pattern" = "^[0-9a-f]{32}$"))]
    trace_id: TraceId,
    #[serde(serialize_with = "hex_span_id", deserialize_with = "parse_span_id")]
    #[schemars(with = "String", extend("pattern" = "^[0-9a-f]{16}$"))]
    span_id: SpanId,
}

impl SpanReference {
    /// The span's OTel identity, or `None` when no trace exporter gave it one.
    pub fn of(span: &tracing::Span) -> Option<Self> {
        let context = span.context();
        let span_context = context.span().span_context().clone();
        span_context.is_valid().then(|| Self {
            trace_id: span_context.trace_id(),
            span_id: span_context.span_id(),
        })
    }

    /// The referenced span as a remote, sampled context, ready to be handed to a span link.
    pub fn span_context(&self) -> SpanContext {
        SpanContext::new(
            self.trace_id,
            self.span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        )
    }

    pub fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    pub fn span_id(&self) -> SpanId {
        self.span_id
    }
}

fn hex_trace_id<S: Serializer>(id: &TraceId, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(id)
}

fn hex_span_id<S: Serializer>(id: &SpanId, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(id)
}

fn parse_trace_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<TraceId, D::Error> {
    let hex = String::deserialize(deserializer)?;
    TraceId::from_hex(&hex)
        .ok()
        .filter(|id| *id != TraceId::INVALID)
        .ok_or_else(|| serde::de::Error::custom(format!("`{hex}` is not a W3C trace id")))
}

fn parse_span_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SpanId, D::Error> {
    let hex = String::deserialize(deserializer)?;
    SpanId::from_hex(&hex)
        .ok()
        .filter(|id| *id != SpanId::INVALID)
        .ok_or_else(|| serde::de::Error::custom(format!("`{hex}` is not a W3C span id")))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_all_zero_id_is_refused_rather_than_linked_to() {
        let json = serde_json::json!({"trace_id": "0".repeat(32), "span_id": "00f067aa0ba902b7"});
        assert!(serde_json::from_value::<SpanReference>(json).is_err());
    }
}
