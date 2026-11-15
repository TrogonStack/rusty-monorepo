use super::Duration;

/// Bound on provider shutdown so a slow or unreachable collector can never
/// hang `trg`'s exit.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// How long a termination signal may hold the process open while the
/// providers flush, before the signal is re-raised regardless.
pub const INTERRUPT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

pub const DEFAULT_FILE_FILTER: &str = "info,trg=debug,rmcp=debug";

/// Crates the OTLP export path itself runs through. An event from any of
/// them reaching the OTel log bridge would be exported by the very pipeline
/// that produced it, and an export failure would then feed itself.
pub const EXPORT_PIPELINE_TARGETS: [&str; 7] = ["opentelemetry", "reqwest", "hyper", "h2", "tower", "rustls", "tonic"];

/// Events emitted under this target reach the logs signal and never become span
/// events, for log-based events whose attributes may carry content a trace must not.
pub(crate) const LOGS_ONLY_TARGET: &str = "trg::logs_only";
