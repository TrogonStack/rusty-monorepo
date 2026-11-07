//! Where a span recorded by an earlier invocation lives, kept in a report so a later
//! invocation (`grade`, `compare`, export) can link its own spans to it.

use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// A span's W3C trace and span id, serialized as lowercase hex.
///
/// Only ever written when the invocation that produced the report exported the span, so
/// its absence means "nothing to link to" rather than a broken reference, and a link built
/// from it always names a span a backend received.
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
    /// The span's OTel identity, or `None` when no trace exporter gave it one or the
    /// sampler dropped it, since a link to an unexported span leads nowhere.
    pub fn of(span: &tracing::Span) -> Option<Self> {
        Self::of_context(span.context().span().span_context())
    }

    /// A span context's identity, under the same rule as [`SpanReference::of`].
    pub fn of_context(span_context: &SpanContext) -> Option<Self> {
        (span_context.is_valid() && span_context.is_sampled()).then(|| Self {
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
#[path = "span_reference_tests.rs"]
mod tests;
