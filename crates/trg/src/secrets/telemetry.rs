//! Client spans for the processes and HTTP requests the backends make.
//!
//! What goes on a span here is what describes the call, never what it
//! carried: no argv, no URL beyond its template, no header, no payload. A
//! backend's arguments name the secret it addresses and, for the keychain's
//! `add-generic-password`, carry the secret itself.

mod constants;
pub(super) use constants::TIMEOUT;
use constants::{KNOWN_METHODS, NONZERO_EXIT, OTHER_METHOD, SIGNALED};
use std::process::ExitStatus;

use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, HTTP_REQUEST_METHOD, HTTP_REQUEST_METHOD_ORIGINAL, HTTP_RESPONSE_STATUS_CODE, PROCESS_EXECUTABLE_NAME,
    PROCESS_EXIT_CODE, PROCESS_PID, SERVER_ADDRESS, SERVER_PORT, URL_TEMPLATE,
};
use tracing::field::Empty;

use super::{BackendKind, SecretsError};
use crate::telemetry::semconv::trg::{SECRETS_BACKEND_KIND, SECRETS_OPERATION};

/// Marks `span` failed with a low-cardinality `error_type`.
pub(crate) fn record_error(span: &tracing::Span, error_type: &str) {
    span.record(ERROR_TYPE, error_type);
    span.record("otel.status_code", "ERROR");
}

/// One invocation of a backend's CLI (`security`, `op`), as an OTel CLI
/// client span.
pub(super) struct CliSpan(tracing::Span);

impl CliSpan {
    /// `operation` is the fixed subcommand the backend chose, which is safe to
    /// record; the arguments after it never are.
    pub(super) fn start(kind: BackendKind, executable: &'static str, operation: &'static str) -> Self {
        Self(tracing::info_span!(
            "backend cli",
            "otel.name" = executable,
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { PROCESS_EXECUTABLE_NAME } = executable,
            { PROCESS_PID } = Empty,
            { PROCESS_EXIT_CODE } = Empty,
            { ERROR_TYPE } = Empty,
            { SECRETS_BACKEND_KIND } = kind.as_str(),
            { SECRETS_OPERATION } = operation,
        ))
    }

    pub(super) fn span(&self) -> &tracing::Span {
        &self.0
    }

    pub(super) fn spawned(&self, pid: Option<u32>) {
        if let Some(pid) = pid {
            self.0.record(PROCESS_PID, i64::from(pid));
        }
    }

    pub(super) fn exited(&self, status: &ExitStatus) {
        match status.code() {
            Some(code) => {
                self.0.record(PROCESS_EXIT_CODE, code);
                if code != 0 {
                    record_error(&self.0, NONZERO_EXIT);
                }
            }
            None => record_error(&self.0, SIGNALED),
        }
    }

    pub(super) fn failed(&self, error_type: &str) {
        record_error(&self.0, error_type);
    }
}

/// Where an HTTP backend lives, as the conventions split it.
#[derive(Clone, Debug)]
pub(super) struct ServerAddress {
    host: String,
    port: Option<u16>,
}

impl ServerAddress {
    pub(super) fn of(url: &reqwest::Url) -> Self {
        Self {
            host: url.host_str().unwrap_or_default().to_string(),
            port: url.port_or_known_default(),
        }
    }
}

/// A route on an HTTP backend, named by its template so neither a mount nor a
/// secret path ever reaches a span.
#[derive(Clone, Copy, Debug)]
pub(super) struct UrlTemplate(&'static str);

impl UrlTemplate {
    pub(super) const fn new(template: &'static str) -> Self {
        Self(template)
    }
}

/// One request to an HTTP backend, as an OTel HTTP client span.
pub(super) struct HttpSpan(tracing::Span);

impl HttpSpan {
    pub(super) fn start(
        kind: BackendKind,
        method: &reqwest::Method,
        template: UrlTemplate,
        server: &ServerAddress,
    ) -> Self {
        let original = method.as_str();
        let known = KNOWN_METHODS.contains(&original);
        let (label, name_prefix) = if known {
            (original, original)
        } else {
            (OTHER_METHOD, "HTTP")
        };
        let span = tracing::info_span!(
            "backend http",
            "otel.name" = format!("{name_prefix} {}", template.0),
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { HTTP_REQUEST_METHOD } = label,
            { HTTP_REQUEST_METHOD_ORIGINAL } = Empty,
            { URL_TEMPLATE } = template.0,
            { SERVER_ADDRESS } = server.host.as_str(),
            { SERVER_PORT } = Empty,
            { HTTP_RESPONSE_STATUS_CODE } = Empty,
            { ERROR_TYPE } = Empty,
            { SECRETS_BACKEND_KIND } = kind.as_str(),
        );
        if !known {
            span.record(HTTP_REQUEST_METHOD_ORIGINAL, original);
        }
        if let Some(port) = server.port {
            span.record(SERVER_PORT, i64::from(port));
        }
        Self(span)
    }

    pub(super) fn span(&self) -> &tracing::Span {
        &self.0
    }

    pub(super) fn responded(&self, status: reqwest::StatusCode) {
        self.0.record(HTTP_RESPONSE_STATUS_CODE, i64::from(status.as_u16()));
    }

    pub(super) fn failed(&self, error_type: &str) {
        record_error(&self.0, error_type);
    }

    /// A status of 400 or above is the failure, per the HTTP conventions, even
    /// when the caller reads it as an answer (a 404 that means "nothing
    /// stored"); otherwise the error the call produced is.
    pub(super) fn finish<T>(&self, status: Option<reqwest::StatusCode>, result: &Result<T, SecretsError>) {
        match (status, result) {
            (Some(status), _) if status.as_u16() >= 400 => record_error(&self.0, status.as_str()),
            (_, Err(e)) => record_error(&self.0, e.error_type()),
            _ => {}
        }
    }
}

#[cfg(test)]
pub(crate) mod testing;
