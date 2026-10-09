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
//! `std::process::exit`, and [`Telemetry::flush`] before any call that
//! replaces the process image, such as `exec(2)`, since nothing runs
//! afterward to flush on success.

mod content;
mod identity;
pub mod propagation;
mod resource;
pub mod semconv;
#[cfg(test)]
pub(crate) mod testing;

pub use content::ContentCapture;
pub use identity::CommandIdentity;

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing::field::Empty;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Bound on provider shutdown so a slow or unreachable collector can never
/// hang `trg`'s exit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// Owns every OTel provider this process created and the root span every
/// subcommand runs inside.
///
/// Dropping it flushes nothing: call [`Telemetry::shutdown`] before
/// `std::process::exit`.
pub struct Telemetry {
    tracer_provider: Option<SdkTracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    logger_provider: Option<SdkLoggerProvider>,
    root_span: tracing::Span,
}

impl Telemetry {
    /// A `Telemetry` with no providers configured, for call sites that need
    /// one to satisfy a signature but are not exercising the OTel paths
    /// (tests, mostly).
    pub fn noop() -> Self {
        Self {
            tracer_provider: None,
            meter_provider: None,
            logger_provider: None,
            root_span: tracing::Span::none(),
        }
    }

    /// The span every subcommand's work should run inside, via
    /// `.instrument(telemetry.root_span())`.
    pub fn root_span(&self) -> tracing::Span {
        self.root_span.clone()
    }

    /// Flushes every configured provider without shutting them down.
    ///
    /// Only call this from a normal thread. It takes locks and performs
    /// network IO, neither of which is async-signal-safe, so it must never
    /// run from inside a raw signal handler.
    pub fn flush(&self) {
        if let Some(provider) = &self.tracer_provider {
            let _ = provider.force_flush();
        }
        if let Some(provider) = &self.meter_provider {
            let _ = provider.force_flush();
        }
        if let Some(provider) = &self.logger_provider {
            let _ = provider.force_flush();
        }
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

        if let Some(provider) = &self.tracer_provider {
            let _ = provider.shutdown_with_timeout(SHUTDOWN_TIMEOUT);
        }
        if let Some(provider) = &self.meter_provider {
            let _ = provider.shutdown_with_timeout(SHUTDOWN_TIMEOUT);
        }
        if let Some(provider) = &self.logger_provider {
            let _ = provider.shutdown_with_timeout(SHUTDOWN_TIMEOUT);
        }
    }
}

/// Initialise `tracing` and, where the environment asks for it, OpenTelemetry
/// export, then return the guard every subcommand runs inside.
///
/// `command` becomes the `trg.command` resource attribute, so it must be
/// derived after `Cli::parse()` succeeds, not before.
pub fn init(command: CommandIdentity) -> Telemetry {
    let filter = EnvFilter::try_from_env("RUST_LOG").unwrap_or_else(|_| EnvFilter::new("info,trg=debug,rmcp=debug"));

    let file_layer = log_path()
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
        });

    if sdk_disabled() {
        let _ = tracing_subscriber::registry().with(filter).with(file_layer).try_init();
        return Telemetry {
            tracer_provider: None,
            meter_provider: None,
            logger_provider: None,
            root_span: root_span(),
        };
    }

    let resource = resource::build(&command);

    let tracer_provider = traces_enabled()
        .then(|| build_tracer_provider(resource.clone()))
        .flatten();
    let meter_provider = metrics_enabled()
        .then(|| build_meter_provider(resource.clone()))
        .flatten();
    let logger_provider = logs_enabled().then(|| build_logger_provider(resource)).flatten();

    let otel_trace_layer = tracer_provider
        .as_ref()
        .map(|provider| tracing_opentelemetry::layer().with_tracer(provider.tracer("trg")));
    let otel_log_layer = logger_provider.as_ref().map(OpenTelemetryTracingBridge::new);

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .try_init();

    if let Some(provider) = &meter_provider {
        opentelemetry::global::set_meter_provider(provider.clone());
    }

    let span = root_span();
    if tracer_provider.is_some() {
        let _ = span.set_parent(propagation::extract_from_env());
    }

    Telemetry {
        tracer_provider,
        meter_provider,
        logger_provider,
        root_span: span,
    }
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
        "process.pid" = std::process::id(),
        "process.exit.code" = Empty,
        "error.type" = Empty,
    )
}

fn sdk_disabled() -> bool {
    std::env::var("OTEL_SDK_DISABLED").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
}

fn exporter_none(var: &str) -> bool {
    std::env::var(var).is_ok_and(|v| v.eq_ignore_ascii_case("none"))
}

fn endpoint_configured(signal_var: &str) -> bool {
    std::env::var_os(signal_var).is_some() || std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
}

fn traces_enabled() -> bool {
    endpoint_configured("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") && !exporter_none("OTEL_TRACES_EXPORTER")
}

fn metrics_enabled() -> bool {
    endpoint_configured("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT") && !exporter_none("OTEL_METRICS_EXPORTER")
}

fn logs_enabled() -> bool {
    endpoint_configured("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT") && !exporter_none("OTEL_LOGS_EXPORTER")
}

/// Sampling is left to `opentelemetry_sdk`'s own defaults: it already reads
/// `OTEL_TRACES_SAMPLER`/`OTEL_TRACES_SAMPLER_ARG` and defaults to
/// `parentbased_always_on`, so no explicit sampler call is needed here.
fn build_tracer_provider(resource: Resource) -> Option<SdkTracerProvider> {
    let exporter = SpanExporter::builder().with_http().build().ok()?;
    Some(
        SdkTracerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(exporter)
            .build(),
    )
}

fn build_meter_provider(resource: Resource) -> Option<SdkMeterProvider> {
    let exporter = MetricExporter::builder().with_http().build().ok()?;
    let reader = PeriodicReader::builder(exporter).build();
    Some(
        SdkMeterProvider::builder()
            .with_resource(resource)
            .with_reader(reader)
            .build(),
    )
}

fn build_logger_provider(resource: Resource) -> Option<SdkLoggerProvider> {
    let exporter = LogExporter::builder().with_http().build().ok()?;
    Some(
        SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_batch_exporter(exporter)
            .build(),
    )
}

fn log_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("trg").join("trg.log"))
}

#[cfg(test)]
mod tests {
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

    use super::*;

    #[test]
    fn root_span_carries_process_attributes() {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_resource(resource::build(&CommandIdentity::new("mcp proxy")))
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("trg");
        let subscriber = tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        let _guard = tracing::subscriber::set_default(subscriber);

        let span = root_span();
        span.record("process.exit.code", 0);
        drop(span);
        let _ = provider.force_flush();

        let spans = exporter.get_finished_spans().expect("exporter not shut down");
        let span = spans.iter().find(|s| s.name == "trg").expect("root span exported");
        let has_pid = span
            .attributes
            .iter()
            .any(|kv| kv.key.as_str() == "process.pid" && kv.value.as_str() == std::process::id().to_string());
        assert!(has_pid);
        let has_exit_code = span.attributes.iter().any(|kv| kv.key.as_str() == "process.exit.code");
        assert!(has_exit_code);
    }

    #[test]
    fn no_exporter_configured_without_otel_env_vars() {
        // SAFETY: test-only; no other test in this process reads these vars
        // concurrently.
        unsafe {
            std::env::remove_var("OTEL_SDK_DISABLED");
            std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
            std::env::remove_var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
            std::env::remove_var("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT");
            std::env::remove_var("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT");
        }
        assert!(!traces_enabled());
        assert!(!metrics_enabled());
        assert!(!logs_enabled());
    }

    #[test]
    fn sdk_disabled_env_var_is_respected() {
        // SAFETY: test-only; no other test in this process reads this var
        // concurrently.
        unsafe {
            std::env::set_var("OTEL_SDK_DISABLED", "true");
        }
        assert!(sdk_disabled());
        unsafe {
            std::env::remove_var("OTEL_SDK_DISABLED");
        }
    }
}
