//! OpenTelemetry foundation for `trg`.
//!
//! `trg mcp proxy` runs as a headless child of an MCP host (Cursor, Claude
//! Code) which swallows stdout and stderr, so every subcommand gets a
//! file-based `tracing` sink regardless of anything else in this module
//! (unchanged from before OTel support existed; see `log_path`). Only when
//! the standard `OTEL_*` environment variables ask for it does `trg` also
//! export traces, metrics and logs over OTLP. With no `OTEL_*` variables
//! set, behaviour is identical to before this module existed: nothing is
//! exported, and nothing reaches stdout, which the proxy needs free for
//! JSON-RPC.
//!
//! [`init`] returns a [`Telemetry`] guard carrying the root span every
//! subcommand runs inside. [`Telemetry::shutdown`] must run before
//! `std::process::exit`, and [`Telemetry::end_before_exec`] before any call
//! that replaces the process image, such as `exec(2)`, since nothing runs
//! afterward to flush on success.

mod content;
mod env;
mod identity;
pub mod propagation;
mod resource;
pub mod semconv;
#[cfg(test)]
pub(crate) mod testing;

pub use content::ContentCapture;
pub use env::{EnvLookup, ProcessEnv};
pub use identity::CommandIdentity;

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;

use opentelemetry::trace::{Status, TraceContextExt as _, TracerProvider as _};
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{ExporterBuildError, LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use opentelemetry_semantic_conventions::attribute::{ERROR_TYPE, PROCESS_EXIT_CODE};
use tracing::field::Empty;
use tracing::level_filters::LevelFilter;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::agentskills::exit_code::{ExitCode, TerminationSignal};
use crate::agentskills::runner::group::{install_termination_handler, with_termination_signals_blocked};

/// Bound on provider shutdown so a slow or unreachable collector can never
/// hang `trg`'s exit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a termination signal may hold the process open while the
/// providers flush, before the signal is re-raised regardless.
pub const INTERRUPT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

const DEFAULT_FILE_FILTER: &str = "info,trg=debug,rmcp=debug";

/// Crates the OTLP export path itself runs through. An event from any of
/// them reaching the OTel log bridge would be exported by the very pipeline
/// that produced it, and an export failure would then feed itself.
const EXPORT_PIPELINE_TARGETS: [&str; 7] = ["opentelemetry", "reqwest", "hyper", "h2", "tower", "rustls", "tonic"];

/// Events emitted under this target reach the logs signal and never become span
/// events, for log-based events whose attributes may carry content a trace must not.
pub(crate) const LOGS_ONLY_TARGET: &str = "trg::logs_only";

/// The providers [`init`] built, reachable from the termination path so a
/// signal can flush them before re-raising.
static INTERRUPT_TARGET: OnceLock<InterruptTarget> = OnceLock::new();

/// Holds the root span's OTel context rather than the `tracing` span itself:
/// a `tracing::Span` handle kept here would stop the root span from ever
/// closing, and so from ever exporting, on an ordinary exit.
struct InterruptTarget {
    providers: Providers,
    root: opentelemetry::Context,
}

#[derive(Clone, Default)]
struct Providers {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
    logger: Option<SdkLoggerProvider>,
}

impl Providers {
    /// `force_flush` takes no timeout of its own, so the flush runs on its
    /// own thread and is abandoned, not awaited, once `timeout` passes.
    fn flush_within(&self, timeout: Duration) {
        let providers = self.clone();
        let (done, finished) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("trg-telemetry-flush".to_string())
            .spawn(move || {
                providers.flush();
                let _ = done.send(());
            });
        if spawned.is_ok() {
            let _ = finished.recv_timeout(timeout);
        }
    }

    fn flush(&self) {
        if let Some(provider) = &self.tracer {
            let _ = provider.force_flush();
        }
        if let Some(provider) = &self.meter {
            let _ = provider.force_flush();
        }
        if let Some(provider) = &self.logger {
            let _ = provider.force_flush();
        }
    }

    /// Shuts every configured provider down within `timeout` total, not `timeout` apiece:
    /// each provider's own `shutdown_with_timeout` runs on its own thread, so their bounds
    /// overlap instead of stacking into a multiple of the budget a caller such as
    /// [`interrupt`] is holding the process open for.
    fn shutdown(&self, timeout: Duration) {
        let providers = self.clone();
        let (done, finished) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("trg-telemetry-shutdown".to_string())
            .spawn(move || {
                providers.shutdown_each(timeout);
                let _ = done.send(());
            });
        if spawned.is_ok() {
            let _ = finished.recv_timeout(timeout);
        }
    }

    fn shutdown_each(&self, timeout: Duration) {
        let mut tasks: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
        if let Some(provider) = self.tracer.clone() {
            tasks.push(Box::new(move || {
                let _ = provider.shutdown_with_timeout(timeout);
            }));
        }
        if let Some(provider) = self.meter.clone() {
            tasks.push(Box::new(move || {
                let _ = provider.shutdown_with_timeout(timeout);
            }));
        }
        if let Some(provider) = self.logger.clone() {
            tasks.push(Box::new(move || {
                let _ = provider.shutdown_with_timeout(timeout);
            }));
        }
        run_concurrently(tasks);
    }

    fn any(&self) -> bool {
        self.tracer.is_some() || self.meter.is_some() || self.logger.is_some()
    }
}

/// Runs each task on its own thread and waits for all of them, so their individual
/// bounds overlap instead of adding up.
fn run_concurrently(tasks: Vec<Box<dyn FnOnce() + Send>>) {
    let handles: Vec<_> = tasks
        .into_iter()
        .filter_map(|task| {
            std::thread::Builder::new()
                .name("trg-telemetry-shutdown-one".to_string())
                .spawn(task)
                .ok()
        })
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
}

/// One of the OTLP signals `trg` can export, each switched on by its own
/// standard environment variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OtelSignal {
    Traces,
    Metrics,
    Logs,
}

impl OtelSignal {
    fn as_str(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Metrics => "metrics",
            Self::Logs => "logs",
        }
    }

    fn endpoint_var(self) -> &'static str {
        match self {
            Self::Traces => "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            Self::Metrics => "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            Self::Logs => "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
        }
    }

    fn exporter_var(self) -> &'static str {
        match self {
            Self::Traces => "OTEL_TRACES_EXPORTER",
            Self::Metrics => "OTEL_METRICS_EXPORTER",
            Self::Logs => "OTEL_LOGS_EXPORTER",
        }
    }

    fn protocol_var(self) -> &'static str {
        match self {
            Self::Traces => "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
            Self::Metrics => "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
            Self::Logs => "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
        }
    }

    /// The variable asking for OTLP over gRPC for this signal, if one does.
    /// The per-signal variable wins over the generic one, as in the spec.
    fn grpc_requested_by(self, env: &impl EnvLookup) -> Option<&'static str> {
        [self.protocol_var(), "OTEL_EXPORTER_OTLP_PROTOCOL"]
            .into_iter()
            .find_map(|var| env.get(var).map(|value| (var, value)))
            .filter(|(_, value)| value.trim().eq_ignore_ascii_case("grpc"))
            .map(|(var, _)| var)
    }

    fn exporter_requested(self, env: &impl EnvLookup) -> ExporterRequest {
        let Some(value) = env.get(self.exporter_var()) else {
            return ExporterRequest::Otlp;
        };
        let entries: Vec<String> = value
            .split(',')
            .map(|entry| entry.trim().to_ascii_lowercase())
            .filter(|entry| !entry.is_empty())
            .collect();
        if entries.iter().all(|entry| entry == "otlp") {
            ExporterRequest::Otlp
        } else if entries.iter().all(|entry| entry == "none") {
            ExporterRequest::None
        } else {
            ExporterRequest::Unsupported(value.trim().to_string())
        }
    }

    fn endpoint_configured(self, env: &impl EnvLookup) -> bool {
        env.get(self.endpoint_var()).is_some() || env.get("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
    }

    #[cfg(test)]
    fn enabled(self, env: &impl EnvLookup) -> bool {
        self.endpoint_configured(env) && self.exporter_requested(env) == ExporterRequest::Otlp
    }
}

/// What `OTEL_{SIGNAL}_EXPORTER` asks for. Unset or empty means the spec's
/// default, `otlp`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExporterRequest {
    Otlp,
    None,
    /// Any exporter but OTLP, such as `console`, which would write to the
    /// stdout trg owns for its own output.
    Unsupported(String),
}

/// An OTLP signal that was asked for but will not be exported, reported to
/// the file log once the subscriber exists to carry it, and on stderr when a
/// setting is to blame.
enum ExportProblem {
    BuildFailed {
        signal: OtelSignal,
        error: ExporterBuildError,
    },
    /// Only the `http-proto` transport is compiled in. The signal is skipped
    /// rather than sent over HTTP anyway: an endpoint configured for gRPC
    /// (port 4317, typically) would not accept it.
    GrpcRequested {
        signal: OtelSignal,
        variable: &'static str,
    },
    UnsupportedExporter {
        signal: OtelSignal,
        value: String,
    },
}

impl ExportProblem {
    /// The setting behind a problem the person running trg can fix, or
    /// `None` when the setting was fine and the exporter failed anyway.
    fn misconfigured_setting(&self) -> Option<(OtelSignal, String)> {
        match self {
            Self::BuildFailed { .. } => None,
            Self::GrpcRequested { signal, variable } => Some((*signal, format!("{variable}=grpc"))),
            Self::UnsupportedExporter { signal, value } => {
                Some((*signal, format!("{}={value}", signal.exporter_var())))
            }
        }
    }

    fn report(&self) {
        match self {
            Self::UnsupportedExporter { signal, value } => tracing::warn!(
                signal = signal.as_str(),
                "trg exports OTLP only; {}={value} is not supported, so this signal will not be exported",
                signal.exporter_var()
            ),
            Self::BuildFailed { signal, error } => tracing::warn!(
                signal = signal.as_str(),
                error = %error,
                "OTLP exporter failed to build; this signal will not be exported"
            ),
            Self::GrpcRequested { signal, variable } => tracing::warn!(
                signal = signal.as_str(),
                "trg exports OTLP over http/protobuf only; {variable}=grpc is not supported, so this signal will not be exported"
            ),
        }
    }
}

/// One line naming every signal a setting kept from exporting, or `None`
/// when nothing was misconfigured.
fn misconfiguration_warning(problems: &[ExportProblem]) -> Option<String> {
    let mut signals = Vec::new();
    let mut settings = Vec::new();
    for (signal, setting) in problems.iter().filter_map(ExportProblem::misconfigured_setting) {
        if !signals.contains(&signal.as_str()) {
            signals.push(signal.as_str());
        }
        if !settings.contains(&setting) {
            settings.push(setting);
        }
    }
    (!signals.is_empty()).then(|| {
        format!(
            "trg: not exporting {} telemetry: {} unsupported; trg exports OTLP over http/protobuf only",
            signals.join(", "),
            settings.join(", ")
        )
    })
}

/// Tells the person running trg, on stderr since the file log is somewhere
/// they may never look, at most once however often `once` is consulted.
fn warn_once(once: &Once, problems: &[ExportProblem], out: &mut impl Write) {
    if let Some(warning) = misconfiguration_warning(problems) {
        once.call_once(|| {
            let _ = writeln!(out, "{warning}");
        });
    }
}

static MISCONFIGURATION_WARNED: Once = Once::new();

/// Owns every OTel provider this process created and the root span every
/// subcommand runs inside.
///
/// Dropping it flushes nothing: call [`Telemetry::shutdown`] before
/// `std::process::exit`.
pub struct Telemetry {
    providers: Providers,
    root_span: tracing::Span,
}

impl Telemetry {
    /// A `Telemetry` with no providers configured, for call sites that need
    /// one to satisfy a signature but are not exercising the OTel paths
    /// (tests, mostly).
    pub fn noop() -> Self {
        Self {
            providers: Providers::default(),
            root_span: tracing::Span::none(),
        }
    }

    #[cfg(test)]
    pub(crate) fn exporting_traces_to(provider: SdkTracerProvider, root_span: tracing::Span) -> Self {
        Self {
            providers: Providers {
                tracer: Some(provider),
                ..Providers::default()
            },
            root_span,
        }
    }

    /// The span every subcommand's work should run inside, via
    /// `.instrument(telemetry.root_span())`.
    pub fn root_span(&self) -> tracing::Span {
        self.root_span.clone()
    }

    /// Flushes every configured provider without shutting them down, giving
    /// up after the same bound as [`Telemetry::shutdown`] so a black-holed
    /// collector cannot stall the caller.
    ///
    /// Only call this from a normal thread. It takes locks and performs
    /// network IO, neither of which is async-signal-safe, so it must never
    /// run from inside a raw signal handler; [`interrupt`] is the path for
    /// signals.
    pub fn flush(&self) {
        self.providers.flush_within(SHUTDOWN_TIMEOUT);
    }

    /// Ends the root span and shuts every provider down, bounded like
    /// [`Telemetry::shutdown`], for a process about to replace its own image
    /// with `exec(2)`.
    ///
    /// Records no exit code: the process does not exit, it becomes the
    /// launched command. The root span only exports once it ends, which is
    /// once every handle to it is gone, so the caller must already have
    /// dropped any clone it took from [`Telemetry::root_span`], including
    /// the one inside an `.instrument(...)` future.
    pub fn end_before_exec(self) {
        drop(self.root_span);
        self.providers.shutdown(SHUTDOWN_TIMEOUT);
    }

    /// Records `exit_code` on the root span, then flushes and shuts down
    /// every configured provider with a bounded timeout.
    ///
    /// Never panics and never writes to stdout: an unreachable collector or
    /// a export failure only ever reaches the file log, since `trg mcp
    /// proxy` needs stdout free for JSON-RPC.
    pub fn shutdown(self, exit_code: i32) {
        self.root_span.record("process.exit.code", exit_code);
        if exit_code != 0 {
            self.root_span.record("error.type", "nonzero_exit");
        }
        drop(self.root_span);
        self.providers.shutdown(SHUTDOWN_TIMEOUT);
    }
}

/// Ends the root span as interrupted by `signal`, then flushes and shuts
/// down every provider [`init`] built, each bounded by
/// [`INTERRUPT_FLUSH_TIMEOUT`].
///
/// For the termination path only, and only from a normal thread: it takes
/// locks and performs network IO, so it must never run inside a signal
/// handler. A no-op before [`init`] or when nothing is exported.
pub fn interrupt(signal: TerminationSignal) {
    let Some(target) = INTERRUPT_TARGET.get() else {
        return;
    };
    mark_interrupted(&target.root, signal);
    target.providers.shutdown(INTERRUPT_FLUSH_TIMEOUT);
}

/// The root span would otherwise never close: the thread holding it is the
/// one the signal is about to take down. Ending the OTel span directly
/// exports it now; the later close of the `tracing` span finds it already
/// ended and does nothing.
fn mark_interrupted(root: &opentelemetry::Context, signal: TerminationSignal) {
    let span = root.span();
    span.set_attribute(KeyValue::new(
        PROCESS_EXIT_CODE,
        i64::from(ExitCode::Interrupted(signal).code()),
    ));
    span.set_attribute(KeyValue::new(ERROR_TYPE, signal_name(signal)));
    span.set_status(Status::error(format!("interrupted by {}", signal_name(signal))));
    span.end();
}

fn signal_name(signal: TerminationSignal) -> &'static str {
    match signal {
        TerminationSignal::Interrupt => "SIGINT",
        TerminationSignal::Terminate => "SIGTERM",
        TerminationSignal::Hangup => "SIGHUP",
    }
}

/// Initialise `tracing` and, where the environment asks for it, OpenTelemetry
/// export, then return the guard every subcommand runs inside.
///
/// `command` becomes the `trg.command` resource attribute, so it must be
/// derived after `Cli::parse()` succeeds, not before.
pub fn init(command: CommandIdentity) -> Telemetry {
    init_from(command, &ProcessEnv)
}

/// Every layer carries its own filter, so `RUST_LOG` only tunes the file log:
/// `RUST_LOG=warn` must not silently drop every exported span.
///
/// The providers are built with the termination signals blocked, so the
/// exporter threads they spawn are never the one a signal handler holds
/// while the flush it is waiting on needs them.
fn init_from(command: CommandIdentity, env: &impl EnvLookup) -> Telemetry {
    let file_layer = log_path(env)
        .and_then(|path| {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            OpenOptions::new().create(true).append(true).open(&path).ok()
        })
        .map(|file| {
            fmt::layer()
                .with_writer(Mutex::new(file))
                .with_ansi(false)
                .with_target(true)
                .with_filter(file_filter(env))
        });

    if sdk_disabled(env) {
        let _ = tracing_subscriber::registry().with(file_layer).try_init();
        return Telemetry {
            providers: Providers::default(),
            root_span: root_span(),
        };
    }

    let resource = resource::build(&command, env);
    let mut problems = Vec::new();

    let providers = with_termination_signals_blocked(|| Providers {
        tracer: enabled_provider(OtelSignal::Traces, env, &mut problems, || {
            build_tracer_provider(resource.clone())
        }),
        meter: enabled_provider(OtelSignal::Metrics, env, &mut problems, || {
            build_meter_provider(resource.clone())
        }),
        logger: enabled_provider(OtelSignal::Logs, env, &mut problems, || {
            build_logger_provider(resource.clone())
        }),
    });

    let otel_trace_layer = providers.tracer.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("trg"))
            .with_filter(trace_filter())
    });
    let otel_log_layer = providers
        .logger
        .as_ref()
        .map(|provider| OpenTelemetryTracingBridge::new(provider).with_filter(log_filter()));

    let _ = tracing_subscriber::registry()
        .with(file_layer)
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .try_init();

    for problem in &problems {
        problem.report();
    }
    warn_once(&MISCONFIGURATION_WARNED, &problems, &mut std::io::stderr());

    if let Some(provider) = &providers.tracer {
        opentelemetry::global::set_tracer_provider(provider.clone());
    }
    if let Some(provider) = &providers.meter {
        opentelemetry::global::set_meter_provider(provider.clone());
    }

    let span = root_span();
    if providers.tracer.is_some() {
        let _ = span.set_parent(propagation::extract_from_env(env));
    }

    if providers.any() {
        let _ = INTERRUPT_TARGET.set(InterruptTarget {
            providers: providers.clone(),
            root: span.context(),
        });
        install_termination_handler();
    }

    Telemetry {
        providers,
        root_span: span,
    }
}

fn enabled_provider<P>(
    signal: OtelSignal,
    env: &impl EnvLookup,
    problems: &mut Vec<ExportProblem>,
    build: impl FnOnce() -> Result<P, ExporterBuildError>,
) -> Option<P> {
    match signal.exporter_requested(env) {
        ExporterRequest::Otlp => {}
        ExporterRequest::None => return None,
        ExporterRequest::Unsupported(value) => {
            problems.push(ExportProblem::UnsupportedExporter { signal, value });
            return None;
        }
    }
    if !signal.endpoint_configured(env) {
        return None;
    }
    if let Some(variable) = signal.grpc_requested_by(env) {
        problems.push(ExportProblem::GrpcRequested { signal, variable });
        return None;
    }
    build()
        .map_err(|error| problems.push(ExportProblem::BuildFailed { signal, error }))
        .ok()
}

fn file_filter(env: &impl EnvLookup) -> EnvFilter {
    env.get("RUST_LOG")
        .and_then(|directives| EnvFilter::try_new(directives).ok())
        .unwrap_or_else(|| EnvFilter::new(DEFAULT_FILE_FILTER))
}

pub(crate) fn trace_filter() -> Targets {
    Targets::new()
        .with_target("trg", LevelFilter::INFO)
        .with_target(LOGS_ONLY_TARGET, LevelFilter::OFF)
}

pub(crate) fn log_filter() -> Targets {
    EXPORT_PIPELINE_TARGETS.into_iter().fold(
        Targets::new()
            .with_default(LevelFilter::WARN)
            .with_target("trg", LevelFilter::INFO),
        |targets, target| targets.with_target(target, LevelFilter::OFF),
    )
}

/// The root span every subcommand runs inside, named for
/// `process.executable.name` per OTel CLI conventions. Deliberately excludes
/// `process.command_args`: argv may carry secrets (`trg secret set NAME
/// value`), and the subcommand path is already on the `trg.command` resource
/// attribute.
fn root_span() -> tracing::Span {
    tracing::info_span!(
        "trg",
        "process.executable.name" = "trg",
        "process.pid" = i64::from(std::process::id()),
        "process.exit.code" = Empty,
        "error.type" = Empty,
    )
}

fn sdk_disabled(env: &impl EnvLookup) -> bool {
    env.get("OTEL_SDK_DISABLED")
        .is_some_and(|v| v.eq_ignore_ascii_case("true"))
}

/// Sampling is left to `opentelemetry_sdk`'s own defaults: it already reads
/// `OTEL_TRACES_SAMPLER`/`OTEL_TRACES_SAMPLER_ARG` and defaults to
/// `parentbased_always_on`, so no explicit sampler call is needed here.
fn build_tracer_provider(resource: Resource) -> Result<SdkTracerProvider, ExporterBuildError> {
    let exporter = SpanExporter::builder().with_http().build()?;
    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}

fn build_meter_provider(resource: Resource) -> Result<SdkMeterProvider, ExporterBuildError> {
    let exporter = MetricExporter::builder().with_http().build()?;
    let reader = PeriodicReader::builder(exporter).build();
    Ok(SdkMeterProvider::builder()
        .with_resource(resource)
        .with_reader(reader)
        .build())
}

fn build_logger_provider(resource: Resource) -> Result<SdkLoggerProvider, ExporterBuildError> {
    let exporter = LogExporter::builder().with_http().build()?;
    Ok(SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}

fn log_path(env: &impl EnvLookup) -> Option<PathBuf> {
    let base = env
        .get("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env.get("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("trg").join("trg.log"))
}

#[cfg(test)]
mod tests {
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
}
