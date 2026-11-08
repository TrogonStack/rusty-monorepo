use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::{InMemoryLogExporter, SdkLogRecord, SdkLoggerProvider};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use tracing_subscriber::prelude::*;

use crate::telemetry::testing::CapturedTrace;

/// Like [`crate::telemetry::testing::capture`], also keeping the log records the tracing bridge
/// turned events into. Both layers carry the filters `trg` exports through, so what
/// reaches each signal here is what reaches it in a real process.
pub(crate) fn capture_with_logs<T>(work: impl FnOnce() -> T) -> (T, CapturedTrace, Vec<SdkLogRecord>) {
    crate::telemetry::testing::consult_every_dispatcher();
    let spans = InMemorySpanExporter::default();
    let tracer_provider = SdkTracerProvider::builder().with_simple_exporter(spans.clone()).build();
    let logs = InMemoryLogExporter::default();
    let logger_provider = SdkLoggerProvider::builder().with_simple_exporter(logs.clone()).build();
    let subscriber = tracing_subscriber::registry()
        .with(
            tracing_opentelemetry::layer()
                .with_tracer(tracer_provider.tracer("trg"))
                .with_filter(crate::telemetry::trace_filter()),
        )
        .with(OpenTelemetryTracingBridge::new(&logger_provider).with_filter(crate::telemetry::log_filter()));
    let output = tracing::subscriber::with_default(subscriber, work);
    let _ = tracer_provider.force_flush();
    let _ = logger_provider.force_flush();
    let trace = CapturedTrace {
        spans: spans
            .get_finished_spans()
            .expect("in-memory exporter is never shut down here"),
    };
    let records = logs
        .get_emitted_logs()
        .expect("in-memory exporter is never shut down here")
        .into_iter()
        .map(|log| log.record)
        .collect();
    (output, trace, records)
}
