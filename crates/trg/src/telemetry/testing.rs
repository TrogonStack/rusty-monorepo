//! Captures the spans a closure produces, for asserting span trees in unit
//! tests without touching the process-global subscriber.

use std::sync::{Arc, Mutex};

use opentelemetry::trace::{SpanId, TracerProvider as _};
use opentelemetry::Value;
use opentelemetry_sdk::error::OTelSdkResult;
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
