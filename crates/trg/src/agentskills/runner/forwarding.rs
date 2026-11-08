//! Opt-in passthrough of `trg`'s own OpenTelemetry configuration to a harness.
//!
//! A scrubbed or isolated run sees none of the operator's `OTEL_*` variables, so a
//! harness that can export its own spans stays silent unless the operator asks for it.
//! Asking is independent of the environment policy and recorded in the report beside
//! it, since a run that exported telemetry reached a collector the policy alone would
//! not have let it reach.

mod constants;
use constants::{
    CLAUDE_CODE_CONTENT_SWITCHES, CLAUDE_CODE_SWITCHES, CONTENT_SWITCHES, SERVICE_IDENTITY, TRACE_CONTEXT_VARS,
};
use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Runner;
use crate::agentskills::report::EnvironmentPolicy;
use crate::telemetry::ContentCapture;

/// Whether a run hands the harness what it needs to export its own telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryForwarding {
    #[default]
    Off,
    On,
}

impl TelemetryForwarding {
    pub fn requested(forward: bool) -> Self {
        if forward {
            Self::On
        } else {
            Self::Off
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
        }
    }

    /// The variables a run adds to the harness environment.
    ///
    /// Under [`EnvironmentPolicy::Inherited`] the operator's own `OTEL_*` variables
    /// already reach the harness, so only the switches `trg` owns are added there.
    pub(crate) fn harness_vars(
        self,
        runner: Runner,
        policy: EnvironmentPolicy,
        host: &BTreeMap<String, String>,
        content: ContentCapture,
    ) -> BTreeMap<String, String> {
        if matches!(self, Self::Off) || runner.telemetry_passthrough() != HarnessPassthrough::EnvVars {
            return BTreeMap::new();
        }

        let mut vars: BTreeMap<String, String> = if matches!(policy, EnvironmentPolicy::Inherited) {
            BTreeMap::new()
        } else {
            host.iter()
                .filter(|(key, _)| is_admitted(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        };

        for (key, value) in CLAUDE_CODE_SWITCHES {
            vars.insert(key.to_string(), value.to_string());
        }
        if host.contains_key("ANTHROPIC_BASE_URL") {
            vars.insert("CLAUDE_CODE_PROPAGATE_TRACEPARENT".to_string(), "1".to_string());
        }
        for signal in OtlpSignal::ALL {
            if signal.endpoint_configured(host) && !host.contains_key(signal.exporter_var()) {
                vars.insert(signal.exporter_var().to_string(), "otlp".to_string());
            }
        }
        if content != ContentCapture::NoContent {
            for key in CLAUDE_CODE_CONTENT_SWITCHES {
                vars.insert(key.to_string(), "1".to_string());
            }
        }
        vars
    }

    /// Whether a harness configured through its environment is handed a traces exporter.
    /// Switching its telemetry on without one exports no spans at all, and an operator
    /// who pointed `OTEL_TRACES_EXPORTER` at `console` or anything else non-OTLP keeps
    /// the harness's spans off the wire, so the runner still has to rebuild them.
    pub(crate) fn forwards_traces(self, runner: Runner, host: &BTreeMap<String, String>) -> bool {
        matches!(self, Self::On)
            && runner.telemetry_passthrough() == HarnessPassthrough::EnvVars
            && OtlpSignal::Traces.endpoint_configured(host)
            && OtlpSignal::Traces.exports_via_otlp(host)
    }
}

/// How a harness takes telemetry configuration, if at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarnessPassthrough {
    /// Configured through `OTEL_*` variables and its own switches.
    EnvVars,
    /// Configured through an `[otel]` table in its `config.toml`.
    CodexConfig,
    /// Exports nothing a run could forward.
    Unsupported,
}

impl Runner {
    pub fn telemetry_passthrough(self) -> HarnessPassthrough {
        match self {
            Self::ClaudeCode => HarnessPassthrough::EnvVars,
            Self::Codex => HarnessPassthrough::CodexConfig,
            Self::CursorAgent => HarnessPassthrough::Unsupported,
        }
    }
}

fn is_admitted(key: &str) -> bool {
    if TRACE_CONTEXT_VARS.contains(&key) {
        return true;
    }
    key.starts_with("OTEL_") && !CONTENT_SWITCHES.contains(&key) && !SERVICE_IDENTITY.contains(&key)
}

/// Whether a variable carries exporter credentials, which never belong in `env.json`.
pub(crate) fn is_exporter_header_var(key: &str) -> bool {
    key.starts_with("OTEL_EXPORTER_OTLP_") && key.ends_with("HEADERS")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OtlpSignal {
    Traces,
    Metrics,
    Logs,
}

impl OtlpSignal {
    const ALL: [Self; 3] = [Self::Traces, Self::Metrics, Self::Logs];

    fn upper(self) -> &'static str {
        match self {
            Self::Traces => "TRACES",
            Self::Metrics => "METRICS",
            Self::Logs => "LOGS",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Traces => "v1/traces",
            Self::Metrics => "v1/metrics",
            Self::Logs => "v1/logs",
        }
    }

    fn exporter_var(self) -> &'static str {
        match self {
            Self::Traces => "OTEL_TRACES_EXPORTER",
            Self::Metrics => "OTEL_METRICS_EXPORTER",
            Self::Logs => "OTEL_LOGS_EXPORTER",
        }
    }

    fn specific(self, suffix: &str) -> String {
        format!("OTEL_EXPORTER_OTLP_{}_{suffix}", self.upper())
    }

    fn endpoint_configured(self, host: &BTreeMap<String, String>) -> bool {
        host.contains_key(&self.specific("ENDPOINT")) || host.contains_key("OTEL_EXPORTER_OTLP_ENDPOINT")
    }

    fn disabled(self, host: &BTreeMap<String, String>) -> bool {
        host.get(self.exporter_var())
            .is_some_and(|value| value.eq_ignore_ascii_case("none"))
    }

    /// Whether the signal's exporter is left for `trg` to default to `otlp`, or an
    /// operator named `otlp` explicitly. Any other exporter, like `console`, never puts
    /// the signal on the wire `trg` can read from.
    fn exports_via_otlp(self, host: &BTreeMap<String, String>) -> bool {
        match host.get(self.exporter_var()) {
            None => true,
            Some(value) => value.eq_ignore_ascii_case("otlp"),
        }
    }

    fn protocol(self, host: &BTreeMap<String, String>) -> OtlpProtocol {
        let value = host
            .get(&self.specific("PROTOCOL"))
            .or_else(|| host.get("OTEL_EXPORTER_OTLP_PROTOCOL"));
        match value.map(String::as_str) {
            Some("grpc") => OtlpProtocol::Grpc,
            Some("http/json") => OtlpProtocol::HttpJson,
            _ => OtlpProtocol::HttpProtobuf,
        }
    }

    /// A signal-specific endpoint is used as given; the shared one names a base the
    /// signal path is appended to over HTTP, per the OTLP exporter specification.
    fn endpoint(self, host: &BTreeMap<String, String>, protocol: OtlpProtocol) -> Option<String> {
        if let Some(endpoint) = host.get(&self.specific("ENDPOINT")) {
            return Some(endpoint.clone());
        }
        let base = host.get("OTEL_EXPORTER_OTLP_ENDPOINT")?;
        Some(match protocol {
            OtlpProtocol::Grpc => base.clone(),
            OtlpProtocol::HttpProtobuf | OtlpProtocol::HttpJson => {
                format!("{}/{}", base.trim_end_matches('/'), self.path())
            }
        })
    }

    fn headers(self, host: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut headers = host
            .get("OTEL_EXPORTER_OTLP_HEADERS")
            .map(|value| parse_headers(value))
            .unwrap_or_default();
        if let Some(value) = host.get(&self.specific("HEADERS")) {
            headers.extend(parse_headers(value));
        }
        headers
    }

    fn codex_exporter(self, host: &BTreeMap<String, String>) -> Option<CodexExporter> {
        if self.disabled(host) {
            return None;
        }
        let protocol = self.protocol(host);
        let endpoint = self.endpoint(host, protocol)?;
        let headers = self.headers(host);
        Some(match protocol {
            OtlpProtocol::Grpc => CodexExporter::OtlpGrpc { endpoint, headers },
            OtlpProtocol::HttpProtobuf => CodexExporter::OtlpHttp {
                endpoint,
                protocol: CodexHttpProtocol::Binary,
                headers,
            },
            OtlpProtocol::HttpJson => CodexExporter::OtlpHttp {
                endpoint,
                protocol: CodexHttpProtocol::Json,
                headers,
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
    HttpJson,
}

/// `OTEL_EXPORTER_OTLP_HEADERS` is a comma-separated list of `key=value` pairs with
/// percent-encoded values.
fn parse_headers(value: &str) -> BTreeMap<String, String> {
    value
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), percent_decode(value.trim())))
        .filter(|(key, _)| !key.is_empty())
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| bytes.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// The exporter shapes codex accepts in its `config.toml`, mirroring
/// `OtelExporterKind` in `codex-rs/config/src/types.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexExporter {
    OtlpHttp {
        endpoint: String,
        protocol: CodexHttpProtocol,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
    OtlpGrpc {
        endpoint: String,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CodexHttpProtocol {
    Binary,
    Json,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CodexOtelSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    log_user_prompt: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exporter: Option<CodexExporter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_exporter: Option<CodexExporter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    metrics_exporter: Option<CodexExporter>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct CodexOtelDocument {
    otel: CodexOtelSection,
}

/// The `[otel]` table a codex run is given, rendered from the same standard `OTEL_*`
/// variables `trg` itself reads.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexOtelConfig {
    rendered: String,
    exports_traces: bool,
}

impl CodexOtelConfig {
    /// `None` when the host configures no OTLP endpoint for any signal, so a run is
    /// never handed an `[otel]` table that points nowhere.
    pub fn render(host: &BTreeMap<String, String>, content: ContentCapture) -> Option<Self> {
        let section = CodexOtelSection {
            log_user_prompt: (content != ContentCapture::NoContent).then_some(true),
            exporter: OtlpSignal::Logs.codex_exporter(host),
            trace_exporter: OtlpSignal::Traces.codex_exporter(host),
            metrics_exporter: OtlpSignal::Metrics.codex_exporter(host),
        };
        if section.exporter.is_none() && section.trace_exporter.is_none() && section.metrics_exporter.is_none() {
            return None;
        }
        let exports_traces = section.trace_exporter.is_some();
        toml::to_string(&CodexOtelDocument { otel: section })
            .ok()
            .map(|rendered| Self {
                rendered,
                exports_traces,
            })
    }

    pub fn as_str(&self) -> &str {
        &self.rendered
    }

    /// Whether codex will export spans of its own once given this table.
    pub fn exports_traces(&self) -> bool {
        self.exports_traces
    }
}

#[cfg(test)]
#[path = "forwarding_tests.rs"]
mod tests;
