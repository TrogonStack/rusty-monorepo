//! Captures the spans a closure produces, for asserting span trees in unit
//! tests without touching the process-global subscriber.

use std::sync::{Arc, Mutex};

use opentelemetry::metrics::{Meter, MeterProvider as _};
use opentelemetry::trace::{SpanId, TracerProvider as _};
use opentelemetry::{KeyValue, Value};
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData, SpanExporter};
use tracing_subscriber::prelude::*;

pub(crate) struct CapturedTrace {
    pub spans: Vec<SpanData>,
}

/// While a single dispatcher exists, tracing caches a callsite's interest
/// from whichever thread reaches it first, so a test thread without a
/// subscriber can disable a span for every capturing thread. Keeping a second
/// dispatcher alive makes each callsite ask the current one instead.
pub(crate) fn consult_every_dispatcher() {
    static PINNED: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    PINNED.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
}

/// Runs `work` with a thread-local subscriber exporting to memory. Work on
/// other threads is only captured when it re-enters this subscriber, for
/// example through [`super::propagation::LaneParent`].
pub(crate) fn capture<T>(work: impl FnOnce() -> T) -> (T, CapturedTrace) {
    consult_every_dispatcher();
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("trg")));
    let output = tracing::subscriber::with_default(subscriber, work);
    let _ = provider.force_flush();
    let spans = exporter
        .get_finished_spans()
        .expect("in-memory exporter is never shut down here");
    (output, CapturedTrace { spans })
}

/// One histogram series a [`capture_metrics`] run exported.
#[derive(Debug, Clone)]
pub(crate) struct HistogramPoint {
    pub name: String,
    pub attributes: Vec<KeyValue>,
    pub count: u64,
}

impl HistogramPoint {
    pub fn attribute(&self, key: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    }
}

/// Runs `work` against a meter exporting to memory, returning every `f64`
/// histogram series it recorded.
pub(crate) fn capture_metrics<T>(work: impl FnOnce(&Meter) -> T) -> (T, Vec<HistogramPoint>) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    let output = work(&provider.meter("trg"));
    provider.force_flush().expect("in-memory metrics flush");
    let points = exporter
        .get_finished_metrics()
        .expect("in-memory exporter is never shut down here")
        .iter()
        .flat_map(|resource| resource.scope_metrics())
        .flat_map(|scope| scope.metrics())
        .flat_map(|metric| match metric.data() {
            AggregatedMetrics::F64(MetricData::Histogram(histogram)) => histogram
                .data_points()
                .map(|point| HistogramPoint {
                    name: metric.name().to_string(),
                    attributes: point.attributes().cloned().collect(),
                    count: point.count(),
                })
                .collect(),
            _ => Vec::new(),
        })
        .collect();
    (output, points)
}

/// An exporter whose spans survive the provider shutting down, unlike
/// [`InMemorySpanExporter`], for asserting on what a shutdown path exported.
#[derive(Clone, Debug, Default)]
pub(crate) struct KeptSpans(Arc<Mutex<Vec<SpanData>>>);

impl KeptSpans {
    pub fn spans(&self) -> Vec<SpanData> {
        self.0.lock().expect("not poisoned").clone()
    }
}

impl SpanExporter for KeptSpans {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        self.0.lock().expect("not poisoned").extend(batch);
        Ok(())
    }
}

impl CapturedTrace {
    pub fn span(&self, name: &str) -> Option<&SpanData> {
        self.spans.iter().find(|span| span.name == name)
    }

    pub fn spans_named(&self, name: &str) -> Vec<&SpanData> {
        self.spans.iter().filter(|span| span.name == name).collect()
    }

    /// The span named `name` whose string attribute `key` is `value`, for the spans
    /// that share one low-cardinality name and tell instances apart by attribute.
    pub fn span_where(&self, name: &str, key: &str, value: &str) -> Option<&SpanData> {
        self.spans.iter().find(|span| {
            span.name == name
                && span
                    .attributes
                    .iter()
                    .any(|kv| kv.key.as_str() == key && kv.value.as_str() == value)
        })
    }

    pub fn children_named<'a>(&'a self, parent: &SpanData, name: &str) -> Vec<&'a SpanData> {
        self.children_of(parent)
            .into_iter()
            .filter(|span| span.name == name)
            .collect()
    }

    pub fn attribute(&self, span: &str, key: &str) -> Option<Value> {
        self.span(span)?
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.clone())
    }

    pub fn children_of(&self, parent: &SpanData) -> Vec<&SpanData> {
        let id: SpanId = parent.span_context.span_id();
        self.spans.iter().filter(|span| span.parent_span_id == id).collect()
    }

    #[track_caller]
    pub fn assert_child_of(&self, child: &str, parent: &str) {
        let parent = self.span(parent).unwrap_or_else(|| panic!("no span named {parent:?}"));
        let child = self.span(child).unwrap_or_else(|| panic!("no span named {child:?}"));
        assert_eq!(
            child.parent_span_id,
            parent.span_context.span_id(),
            "{:?} is not a child of {:?}",
            child.name,
            parent.name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::capture;

    #[test]
    fn captures_names_attributes_and_parentage() {
        let ((), trace) = capture(|| {
            let parent = tracing::info_span!("parent", "trg.example" = "value");
            parent.in_scope(|| {
                tracing::info_span!("child").in_scope(|| ());
                tracing::info_span!("child").in_scope(|| ());
            });
        });
        let parent = trace.span("parent").expect("parent exported");
        assert_eq!(trace.children_of(parent).len(), 2);
        assert_eq!(trace.spans_named("child").len(), 2);
        assert_eq!(
            trace.attribute("parent", "trg.example").map(|v| v.to_string()),
            Some("value".to_string())
        );
    }
}
