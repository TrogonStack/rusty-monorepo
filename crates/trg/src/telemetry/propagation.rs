//! W3C Trace Context and Baggage propagation across process and thread
//! boundaries.
//!
//! Only meaningful once a trace exporter is active: without one, no span
//! gets a real OTel span context, and injecting it would emit an all-zero
//! `traceparent` that is worse than emitting nothing. Every function here
//! returns an empty carrier in that case instead.

mod constants;
use constants::CARRIER_KEYS;
use std::collections::HashMap;

use opentelemetry::propagation::{Extractor, Injector, TextMapCompositePropagator, TextMapPropagator};
use opentelemetry::trace::TraceContextExt;
use opentelemetry::Context;
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use super::env::EnvLookup;

fn propagator() -> TextMapCompositePropagator {
    TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ])
}

struct MapCarrier(HashMap<String, String>);

impl Injector for MapCarrier {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
}

impl Extractor for MapCarrier {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
}

/// The current span's trace context as env-variable-ready pairs
/// (`TRACEPARENT`, `TRACESTATE`, `BAGGAGE`). Empty when no trace exporter is
/// active, so callers can always inject these unconditionally without
/// checking first.
pub fn carrier_pairs() -> Vec<(String, String)> {
    let context = tracing::Span::current().context();
    if !context.span().span_context().is_valid() {
        return Vec::new();
    }
    let mut carrier = MapCarrier(HashMap::new());
    propagator().inject_context(&context, &mut carrier);
    carrier
        .0
        .into_iter()
        .map(|(key, value)| (key.to_uppercase(), value))
        .collect()
}

/// Injects the current trace context into a [`std::process::Command`]'s
/// environment. A no-op when no trace exporter is active.
pub fn inject_std_command(command: &mut std::process::Command) {
    for (key, value) in carrier_pairs() {
        command.env(key, value);
    }
}

/// Injects the current trace context into a [`tokio::process::Command`]'s
/// environment. A no-op when no trace exporter is active.
pub fn inject_tokio_command(command: &mut tokio::process::Command) {
    for (key, value) in carrier_pairs() {
        command.env(key, value);
    }
}

/// Extracts an inbound trace context from this process's own environment
/// (`TRACEPARENT`/`TRACESTATE`/`BAGGAGE`), for use as the root span's parent.
pub(super) fn extract_from_env(env: &impl EnvLookup) -> Context {
    let mut carrier = MapCarrier(HashMap::new());
    for key in CARRIER_KEYS {
        if let Some(value) = env.get(&key.to_uppercase()) {
            carrier.0.insert(key.to_string(), value);
        }
    }
    propagator().extract(&carrier)
}

/// The spawning thread's subscriber and current span, captured so work on
/// another thread (a `std::thread::scope` lane) nests under it. Neither
/// crosses a thread boundary on its own: a new thread starts with the global
/// subscriber and an empty span stack.
#[derive(Clone)]
pub struct LaneParent {
    dispatch: tracing::Dispatch,
    span: tracing::Span,
}

impl LaneParent {
    pub fn capture() -> Self {
        Self {
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            span: tracing::Span::current(),
        }
    }

    pub fn in_scope<T>(&self, work: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, || self.span.in_scope(work))
    }
}

#[cfg(test)]
#[path = "propagation_tests.rs"]
mod tests;
