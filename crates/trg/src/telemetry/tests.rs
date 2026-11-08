use std::sync::Arc;
use std::thread;

use opentelemetry::trace::Status;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing::Level;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use super::*;
use crate::telemetry::env::fixed;

const EXPORTING: &[(&str, &str)] = &[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318")];

fn in_memory_tracer() -> (SdkTracerProvider, InMemorySpanExporter) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_resource(resource::build(&CommandIdentity::new("mcp proxy"), &fixed(&[])))
        .with_simple_exporter(exporter.clone())
        .build();
    (provider, exporter)
}

fn finished(provider: &SdkTracerProvider, exporter: &InMemorySpanExporter) -> Vec<SpanData> {
    let _ = provider.force_flush();
    exporter.get_finished_spans().expect("exporter not shut down")
}

/// Records the target of every event that reaches it.
#[derive(Clone, Default)]
struct EventTargets(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> Layer<S> for EventTargets {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        self.0
            .lock()
            .expect("not poisoned")
            .push(event.metadata().target().to_string());
    }
}

impl EventTargets {
    fn seen(&self) -> Vec<String> {
        self.0.lock().expect("not poisoned").clone()
    }
}

#[test]
fn root_span_carries_process_attributes() {
    let (provider, exporter) = in_memory_tracer();
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("trg")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let span = root_span();
    span.record("process.exit.code", 0);
    drop(span);

    let spans = finished(&provider, &exporter);
    let span = spans.iter().find(|s| s.name == "trg").expect("root span exported");
    let pid = span
        .attributes
        .iter()
        .find(|kv| kv.key.as_str() == "process.pid")
        .map(|kv| kv.value.clone());
    assert_eq!(pid, Some(opentelemetry::Value::I64(i64::from(std::process::id()))));
    let has_exit_code = span.attributes.iter().any(|kv| kv.key.as_str() == "process.exit.code");
    assert!(has_exit_code);
}

#[test]
fn no_exporter_configured_without_otel_env_vars() {
    let env = fixed(&[]);
    assert!(!OtelSignal::Traces.enabled(&env));
    assert!(!OtelSignal::Metrics.enabled(&env));
    assert!(!OtelSignal::Logs.enabled(&env));
}

#[test]
fn generic_endpoint_enables_every_signal() {
    let env = fixed(EXPORTING);
    assert!(OtelSignal::Traces.enabled(&env));
    assert!(OtelSignal::Metrics.enabled(&env));
    assert!(OtelSignal::Logs.enabled(&env));
}

#[test]
fn signal_endpoint_enables_only_that_signal() {
    let env = fixed(&[("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "http://127.0.0.1:4318/v1/traces")]);
    assert!(OtelSignal::Traces.enabled(&env));
    assert!(!OtelSignal::Metrics.enabled(&env));
    assert!(!OtelSignal::Logs.enabled(&env));
}

#[test]
fn exporter_none_disables_that_signal() {
    let env = fixed(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318"),
        ("OTEL_METRICS_EXPORTER", "NONE"),
    ]);
    assert!(OtelSignal::Traces.enabled(&env));
    assert!(!OtelSignal::Metrics.enabled(&env));
}

#[test]
fn sdk_disabled_env_var_is_respected() {
    assert!(sdk_disabled(&fixed(&[("OTEL_SDK_DISABLED", "TRUE")])));
    assert!(!sdk_disabled(&fixed(&[("OTEL_SDK_DISABLED", "false")])));
    assert!(!sdk_disabled(&fixed(&[])));
}

#[test]
fn log_path_prefers_xdg_cache_home() {
    let env = fixed(&[("XDG_CACHE_HOME", "/cache"), ("HOME", "/home/someone")]);
    assert_eq!(log_path(&env), Some(PathBuf::from("/cache/trg/trg.log")));
    let env = fixed(&[("HOME", "/home/someone")]);
    assert_eq!(log_path(&env), Some(PathBuf::from("/home/someone/.cache/trg/trg.log")));
    assert_eq!(log_path(&fixed(&[])), None);
}

#[test]
fn a_quiet_rust_log_still_exports_spans() {
    let (provider, exporter) = in_memory_tracer();
    let file = EventTargets::default();
    let subscriber = tracing_subscriber::registry()
        .with(file.clone().with_filter(file_filter(&fixed(&[("RUST_LOG", "warn")]))))
        .with(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("trg"))
                .with_filter(trace_filter()),
        );
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!(target: "trg::eval", "run eval").in_scope(|| {
            tracing::info!(target: "trg::eval", "progress");
        });
        tracing::info_span!(target: "rmcp::service", "serve").in_scope(|| ());
    });

    let names: Vec<_> = finished(&provider, &exporter)
        .into_iter()
        .map(|span| span.name.to_string())
        .collect();
    assert_eq!(names, vec!["run eval".to_string()]);
    assert!(file.seen().is_empty(), "RUST_LOG=warn keeps INFO out of the file log");
}

#[test]
fn log_bridge_takes_trg_info_and_dependency_warnings_only() {
    let bridge = EventTargets::default();
    let subscriber = tracing_subscriber::registry().with(bridge.clone().with_filter(log_filter()));
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "trg::mcp", "kept");
        tracing::debug!(target: "trg::mcp", "dropped");
        tracing::warn!(target: "rmcp::service", "kept");
        tracing::info!(target: "rmcp::service", "dropped");
        tracing::error!(target: "opentelemetry_sdk", "dropped");
        tracing::error!(target: "opentelemetry_otlp::exporter", "dropped");
        tracing::error!(target: "reqwest::connect", "dropped");
        tracing::error!(target: "hyper_util::client", "dropped");
        tracing::error!(target: "h2::proto", "dropped");
    });
    assert_eq!(bridge.seen(), vec!["trg::mcp".to_string(), "rmcp::service".to_string()]);
}

#[test]
fn an_invalid_rust_log_falls_back_to_the_default_file_filter() {
    let file = EventTargets::default();
    let subscriber = tracing_subscriber::registry().with(
        file.clone()
            .with_filter(file_filter(&fixed(&[("RUST_LOG", "trg=nonsense=level")]))),
    );
    tracing::subscriber::with_default(subscriber, || {
        tracing::event!(target: "trg::exec", Level::DEBUG, "kept");
    });
    assert_eq!(file.seen(), vec!["trg::exec".to_string()]);
}

#[test]
fn an_interrupted_root_span_is_exported_with_the_signal() {
    let (provider, exporter) = in_memory_tracer();
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(provider.tracer("trg")));
    let _guard = tracing::subscriber::set_default(subscriber);

    let span = root_span();
    mark_interrupted(&span.context(), TerminationSignal::Terminate);
    let spans = finished(&provider, &exporter);
    drop(span);

    let root = spans
        .iter()
        .find(|s| s.name == "trg")
        .expect("exported before the span closes");
    let attribute = |key: &str| {
        root.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.to_string())
    };
    assert_eq!(attribute(ERROR_TYPE), Some("SIGTERM".to_string()));
    assert_eq!(attribute(PROCESS_EXIT_CODE), Some("143".to_string()));
    assert!(matches!(root.status, Status::Error { .. }));
    assert_eq!(
        finished(&provider, &exporter).len(),
        1,
        "closing it later exports nothing more"
    );
}

#[test]
fn grpc_requested_skips_the_signal_and_says_why() {
    let env = fixed(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
    ]);
    let mut problems = Vec::new();
    let built = enabled_provider(OtelSignal::Traces, &env, &mut problems, || -> Result<(), _> {
        panic!("a gRPC signal must not reach the HTTP exporter")
    });
    assert!(built.is_none());
    assert!(matches!(
        problems.as_slice(),
        [ExportProblem::GrpcRequested {
            signal: OtelSignal::Traces,
            variable: "OTEL_EXPORTER_OTLP_PROTOCOL"
        }]
    ));
}

#[test]
fn an_exporter_other_than_otlp_disables_the_signal_and_says_why() {
    let env = fixed(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318"),
        ("OTEL_TRACES_EXPORTER", "console"),
        ("OTEL_LOGS_EXPORTER", "otlp,console"),
        ("OTEL_METRICS_EXPORTER", " OTLP "),
    ]);

    assert!(!OtelSignal::Traces.enabled(&env));
    assert!(!OtelSignal::Logs.enabled(&env));
    assert!(OtelSignal::Metrics.enabled(&env));

    let mut problems = Vec::new();
    let built = enabled_provider(OtelSignal::Traces, &env, &mut problems, || -> Result<(), _> {
        panic!("an unsupported exporter must not be built")
    });
    assert!(built.is_none());
    assert!(matches!(
        problems.as_slice(),
        [ExportProblem::UnsupportedExporter { signal: OtelSignal::Traces, value }] if value == "console"
    ));
}

#[test]
fn an_unsupported_exporter_is_reported_even_without_an_endpoint() {
    let env = fixed(&[("OTEL_TRACES_EXPORTER", "console")]);
    let mut problems = Vec::new();
    let built = enabled_provider(OtelSignal::Traces, &env, &mut problems, || -> Result<(), _> {
        panic!("an unsupported exporter must not be built")
    });
    assert!(built.is_none());
    assert_eq!(problems.len(), 1);
}

#[test]
fn an_empty_or_none_exporter_reports_nothing() {
    let env = fixed(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318"),
        ("OTEL_TRACES_EXPORTER", ""),
        ("OTEL_LOGS_EXPORTER", "none"),
    ]);
    assert!(OtelSignal::Traces.enabled(&env));
    assert!(!OtelSignal::Logs.enabled(&env));
    let mut problems = Vec::new();
    enabled_provider(OtelSignal::Logs, &env, &mut problems, || -> Result<(), _> {
        panic!("a disabled signal must not be built")
    });
    assert!(problems.is_empty());
}

#[test]
fn misconfigured_signals_share_one_warning_written_once() {
    let env = fixed(&[
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4317"),
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ("OTEL_LOGS_EXPORTER", "console"),
    ]);
    let mut problems = Vec::new();
    for signal in [OtelSignal::Traces, OtelSignal::Metrics, OtelSignal::Logs] {
        enabled_provider(signal, &env, &mut problems, || -> Result<(), _> {
            panic!("no signal here can be exported")
        });
    }

    let once = Once::new();
    let mut stderr = Vec::new();
    warn_once(&once, &problems, &mut stderr);
    warn_once(&once, &problems, &mut stderr);

    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "trg: not exporting traces, metrics, logs telemetry: OTEL_EXPORTER_OTLP_PROTOCOL=grpc, \
             OTEL_LOGS_EXPORTER=console unsupported; trg exports OTLP over http/protobuf only\n"
    );
}

#[test]
fn a_well_configured_process_writes_no_warning() {
    let mut problems = Vec::new();
    enabled_provider(
        OtelSignal::Traces,
        &fixed(EXPORTING),
        &mut problems,
        || -> Result<(), ExporterBuildError> { Ok(()) },
    );
    let mut stderr = Vec::new();
    warn_once(&Once::new(), &problems, &mut stderr);
    assert!(stderr.is_empty());
}

#[test]
fn a_signal_protocol_overrides_the_generic_one() {
    let env = fixed(&[
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
        ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "GRPC"),
    ]);
    assert_eq!(OtelSignal::Traces.grpc_requested_by(&env), None);
    assert_eq!(
        OtelSignal::Metrics.grpc_requested_by(&env),
        Some("OTEL_EXPORTER_OTLP_PROTOCOL")
    );
    assert_eq!(
        OtelSignal::Logs.grpc_requested_by(&env),
        Some("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL")
    );
    assert_eq!(OtelSignal::Traces.grpc_requested_by(&fixed(&[])), None);
}

#[test]
fn interrupt_before_init_does_nothing() {
    interrupt(TerminationSignal::Interrupt);
}

/// The bug this guards against: three providers shut down one after another would take
/// close to 3 * 200ms. Run concurrently, as [`Providers::shutdown`] now does, the total
/// stays close to the slowest one instead.
#[test]
fn run_concurrently_overlaps_tasks_instead_of_stacking_them() {
    let per_task = Duration::from_millis(200);
    let start = std::time::Instant::now();
    run_concurrently(vec![
        Box::new(move || thread::sleep(per_task)),
        Box::new(move || thread::sleep(per_task)),
        Box::new(move || thread::sleep(per_task)),
    ]);
    let elapsed = start.elapsed();
    assert!(
        elapsed < per_task * 2,
        "three {per_task:?} tasks took {elapsed:?}; stacked sequentially they would take ~{:?}",
        per_task * 3
    );
}

#[test]
fn shutdown_tears_down_every_configured_provider_and_returns() {
    let (tracer, _exporter) = in_memory_tracer();
    let providers = Providers {
        tracer: Some(tracer.clone()),
        ..Providers::default()
    };
    providers.shutdown(Duration::from_secs(1));
    assert!(
        matches!(
            tracer.shutdown_with_timeout(Duration::from_secs(1)),
            Err(opentelemetry_sdk::error::OTelSdkError::AlreadyShutdown)
        ),
        "Providers::shutdown must have already shut this provider down"
    );
}
