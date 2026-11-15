//! Spans and metrics for `trg mcp proxy`, after the MCP client semantic
//! conventions: one session span for the bridge, one client span per request
//! the host sends through it.
//!
//! Trace context rides in `params._meta` (SEP-414). A request span adopts the
//! host's context when it sent one, and the remote server sees the proxy's
//! span in its place. With no tracer installed the proxy's span context is
//! invalid and the request is forwarded untouched.

mod constants;
use constants::{DURATION_BUCKETS, HTTP, TCP, TOOL_ERROR};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use http::{HeaderName, HeaderValue};
use opentelemetry::metrics::{Histogram, Meter};
use opentelemetry::propagation::{Extractor, Injector, TextMapCompositePropagator, TextMapPropagator};
use opentelemetry::trace::TraceContextExt;
use opentelemetry::{Context, KeyValue};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, JSONRPC_REQUEST_ID, NETWORK_PROTOCOL_NAME, NETWORK_TRANSPORT, RPC_RESPONSE_STATUS_CODE, SERVER_ADDRESS,
    SERVER_PORT,
};
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, ErrorData, GetMeta, JsonRpcMessage, MetaObject, RequestId};
use rmcp::service::RxJsonRpcMessage;
use rmcp::transport::common::client_side_sse::BoxedSseResponse;
use rmcp::transport::streamable_http_client::{StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse};
use rmcp::{RoleClient, RoleServer};
use serde::Serialize;
use serde_json::Value;
use tracing::field::Empty;
use tracing::{Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::oauth::telemetry::{record_error, AuthOutcome};
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, mcp_method_name, GEN_AI_OPERATION_NAME, GEN_AI_PROMPT_NAME, GEN_AI_TOOL_CALL_ARGUMENTS,
    GEN_AI_TOOL_CALL_RESULT, GEN_AI_TOOL_NAME, MCP_METHOD_NAME, MCP_PROTOCOL_VERSION, MCP_RESOURCE_URI, MCP_SESSION_ID,
};
use crate::telemetry::semconv::generated::metrics::{MCP_CLIENT_OPERATION_DURATION, MCP_CLIENT_SESSION_DURATION};
use crate::telemetry::semconv::trg::{MCP_AUTH_OUTCOME, MCP_EXIT_REASON, MCP_MESSAGE_DIRECTION, MCP_SERVER_NAME};
use crate::telemetry::ContentCapture;

/// Why a bridge stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExitReason {
    HostEof,
    RemoteClosed,
    LocalClosed,
}

impl ExitReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::HostEof => "host_eof",
            Self::RemoteClosed => "remote_closed",
            Self::LocalClosed => "local_closed",
        }
    }
}

/// How a session ended: its bridge stopped, or it never got one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionEnd {
    Exited(ExitReason),
    Failed(&'static str),
}

impl SessionEnd {
    fn error_type(self) -> Option<&'static str> {
        match self {
            Self::Exited(ExitReason::HostEof) => None,
            Self::Exited(reason) => Some(reason.as_str()),
            Self::Failed(error_type) => Some(error_type),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    HostToRemote,
    RemoteToHost,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::HostToRemote => "host_to_remote",
            Self::RemoteToHost => "remote_to_host",
        }
    }
}

/// Where the remote MCP server listens, as the conventions split it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Endpoint {
    address: String,
    port: Option<u16>,
}

impl Endpoint {
    pub(super) fn parse(url: &str) -> Option<Self> {
        let url = reqwest::Url::parse(url).ok()?;
        Some(Self {
            address: url.host_str()?.to_string(),
            port: url.port_or_known_default(),
        })
    }
}

/// The `Mcp-Session-Id` the remote server assigned, once the transport sees
/// one. Only stateful servers assign one.
#[derive(Debug, Clone, Default)]
pub(super) struct SessionIdSlot(Arc<Mutex<Option<String>>>);

impl SessionIdSlot {
    fn observe(&self, id: &str) {
        if let Ok(mut slot) = self.0.lock() {
            if slot.as_deref() != Some(id) {
                *slot = Some(id.to_string());
            }
        }
    }

    fn get(&self) -> Option<String> {
        self.0.lock().ok().and_then(|slot| slot.clone())
    }
}

/// What a request span knows about the connection it travels over.
#[derive(Debug, Clone)]
pub(super) struct Connection {
    server_name: String,
    endpoint: Option<Endpoint>,
    protocol_version: Option<String>,
    session_id: SessionIdSlot,
}

impl Connection {
    pub(super) fn new(server_name: &str, endpoint: Option<Endpoint>) -> Self {
        Self {
            server_name: server_name.to_string(),
            endpoint,
            protocol_version: None,
            session_id: SessionIdSlot::default(),
        }
    }

    fn record_on(&self, span: &Span) {
        if let Some(endpoint) = &self.endpoint {
            span.record(NETWORK_TRANSPORT, TCP);
            span.record(NETWORK_PROTOCOL_NAME, HTTP);
            span.record(SERVER_ADDRESS, endpoint.address.as_str());
            if let Some(port) = endpoint.port {
                span.record(SERVER_PORT, i64::from(port));
            }
        }
        if let Some(version) = &self.protocol_version {
            span.record(MCP_PROTOCOL_VERSION, version.as_str());
        }
        if let Some(id) = self.session_id.get() {
            span.record(MCP_SESSION_ID, id.as_str());
        }
    }
}

/// What a request is aimed at, beyond its method.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Tool(String),
    Prompt(String),
    Resource(String),
}

impl Target {
    fn of(request: &ClientRequest) -> Option<Self> {
        match request {
            ClientRequest::CallToolRequest(r) => Some(Self::Tool(r.params.name.to_string())),
            ClientRequest::GetPromptRequest(r) => Some(Self::Prompt(r.params.name.clone())),
            ClientRequest::ReadResourceRequest(r) => Some(Self::Resource(r.params.uri.clone())),
            ClientRequest::SubscribeRequest(r) => Some(Self::Resource(r.params.uri.clone())),
            ClientRequest::UnsubscribeRequest(r) => Some(Self::Resource(r.params.uri.clone())),
            _ => None,
        }
    }

    fn named(&self) -> Option<&str> {
        match self {
            Self::Tool(name) | Self::Prompt(name) => Some(name),
            Self::Resource(_) => None,
        }
    }
}

/// `_meta` read as a W3C trace context carrier.
struct MetaExtractor<'a>(&'a MetaObject);

impl Extractor for MetaExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0 .0.get(key).and_then(Value::as_str)
    }

    fn keys(&self) -> Vec<&str> {
        self.0 .0.keys().map(String::as_str).collect()
    }
}

/// `_meta` written as a W3C trace context carrier. An empty `tracestate`
/// is left out rather than written as `""`.
struct MetaInjector<'a>(&'a mut MetaObject);

impl Injector for MetaInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if !value.is_empty() {
            self.0 .0.insert(key.to_string(), Value::String(value));
        }
    }
}

fn host_context(request: &ClientRequest) -> Context {
    let propagator = TextMapCompositePropagator::new(vec![
        Box::new(TraceContextPropagator::new()),
        Box::new(BaggagePropagator::new()),
    ]);
    propagator.extract(&MetaExtractor(&request.get_meta().0))
}

/// One proxied request, as an MCP client span.
pub(super) struct RequestSpan {
    span: Span,
    method: String,
    target: Option<Target>,
    opened: Instant,
}

impl RequestSpan {
    /// Opens the span for `request`, a child of the host's trace context
    /// when `_meta` carried one and of `parent` otherwise.
    pub(super) fn open(
        request: &ClientRequest,
        id: &RequestId,
        parent: &Span,
        connection: &Connection,
        content: ContentCapture,
    ) -> Self {
        let method = request.method().to_string();
        let target = Target::of(request);
        let name = match target.as_ref().and_then(Target::named) {
            Some(target) => format!("{method} {target}"),
            None => method.clone(),
        };
        let span = tracing::info_span!(
            parent: parent,
            "mcp request",
            "otel.name" = name.as_str(),
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { MCP_METHOD_NAME } = method.as_str(),
            { JSONRPC_REQUEST_ID } = id.to_string().as_str(),
            { GEN_AI_OPERATION_NAME } = Empty,
            { GEN_AI_TOOL_NAME } = Empty,
            { GEN_AI_PROMPT_NAME } = Empty,
            { MCP_RESOURCE_URI } = Empty,
            { GEN_AI_TOOL_CALL_ARGUMENTS } = Empty,
            { GEN_AI_TOOL_CALL_RESULT } = Empty,
            { RPC_RESPONSE_STATUS_CODE } = Empty,
            { ERROR_TYPE } = Empty,
            { MCP_SESSION_ID } = Empty,
            { MCP_PROTOCOL_VERSION } = Empty,
            { NETWORK_TRANSPORT } = Empty,
            { NETWORK_PROTOCOL_NAME } = Empty,
            { SERVER_ADDRESS } = Empty,
            { SERVER_PORT } = Empty,
        );

        match &target {
            Some(Target::Tool(tool)) => {
                span.record(GEN_AI_OPERATION_NAME, gen_ai_operation_name::EXECUTE_TOOL);
                span.record(GEN_AI_TOOL_NAME, tool.as_str());
            }
            Some(Target::Prompt(prompt)) => {
                span.record(GEN_AI_PROMPT_NAME, prompt.as_str());
            }
            Some(Target::Resource(uri)) => {
                span.record(MCP_RESOURCE_URI, uri.as_str());
            }
            None => {}
        }
        if content.includes_span() {
            if let ClientRequest::CallToolRequest(call) = request {
                if let Some(arguments) = call.params.arguments.as_ref().and_then(to_json) {
                    span.record(GEN_AI_TOOL_CALL_ARGUMENTS, arguments.as_str());
                }
            }
        }
        connection.record_on(&span);

        let host = host_context(request);
        if host.span().span_context().is_valid() {
            let _ = span.set_parent(host);
            span.add_link(parent.context().span().span_context().clone());
        }

        Self {
            span,
            method,
            target,
            opened: Instant::now(),
        }
    }

    /// Points `_meta.traceparent` at this span. Leaves the request untouched
    /// when no tracer is installed, so the host's own context, if any, still
    /// reaches the server as it was sent.
    pub(super) fn inject(&self, request: &mut ClientRequest) {
        let context = self.span.context();
        if !context.span().span_context().is_valid() {
            return;
        }
        TraceContextPropagator::new().inject_context(&context, &mut MetaInjector(&mut request.get_meta_mut().0));
    }

    fn fail(&self, error_type: &str) {
        record_error(&self.span, error_type);
    }

    fn fail_rpc(&self, error: &ErrorData) -> String {
        let code = error.code.0.to_string();
        self.span.record(RPC_RESPONSE_STATUS_CODE, code.as_str());
        self.fail(&code);
        code
    }

    /// Records a refusal the proxy answered itself, on the span and on the
    /// operation duration a forwarded request would have fed.
    pub(super) fn refuse(self, error: &ErrorData, connection: &Connection, operation: &Histogram<f64>) {
        let error_type = self.fail_rpc(error);
        self.record_duration(operation, connection, Some(&error_type));
    }

    fn record_duration(&self, operation: &Histogram<f64>, connection: &Connection, error_type: Option<&str>) {
        operation.record(
            self.opened.elapsed().as_secs_f64(),
            &self.metric_attributes(connection, error_type),
        );
    }

    fn metric_attributes(&self, connection: &Connection, error_type: Option<&str>) -> Vec<KeyValue> {
        let mut attributes = vec![KeyValue::new(MCP_METHOD_NAME, self.method.clone())];
        match &self.target {
            Some(Target::Tool(tool)) => attributes.push(KeyValue::new(GEN_AI_TOOL_NAME, tool.clone())),
            Some(Target::Prompt(prompt)) => attributes.push(KeyValue::new(GEN_AI_PROMPT_NAME, prompt.clone())),
            Some(Target::Resource(_)) | None => {}
        }
        if let Some(error_type) = error_type {
            attributes.push(KeyValue::new(ERROR_TYPE, error_type.to_string()));
        }
        session_attributes(connection, &mut attributes);
        attributes
    }
}

fn session_attributes(connection: &Connection, attributes: &mut Vec<KeyValue>) {
    attributes.push(KeyValue::new(MCP_SERVER_NAME, connection.server_name.clone()));
    if let Some(version) = &connection.protocol_version {
        attributes.push(KeyValue::new(MCP_PROTOCOL_VERSION, version.clone()));
    }
    if connection.endpoint.is_some() {
        attributes.push(KeyValue::new(NETWORK_TRANSPORT, TCP));
    }
}

fn to_json(value: &impl Serialize) -> Option<String> {
    serde_json::to_string(value).ok()
}

struct ProxyMetrics {
    operation: Histogram<f64>,
    session: Histogram<f64>,
}

/// `mcp.client.operation.duration`, which every request the proxy answers
/// feeds, forwarded or refused.
pub(super) fn operation_duration(meter: &Meter) -> Histogram<f64> {
    meter
        .f64_histogram(MCP_CLIENT_OPERATION_DURATION)
        .with_unit("s")
        .with_description("Duration of an MCP request as observed by the sender of the request.")
        .with_boundaries(DURATION_BUCKETS.to_vec())
        .build()
}

impl ProxyMetrics {
    fn new() -> Self {
        let meter = opentelemetry::global::meter("trg");
        Self {
            operation: operation_duration(&meter),
            session: meter
                .f64_histogram(MCP_CLIENT_SESSION_DURATION)
                .with_unit("s")
                .with_description("The duration of the MCP session as observed on the MCP client.")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
        }
    }
}

/// One proxy run: its session span, the requests still waiting for an
/// answer, and the metrics both feed.
pub(super) struct Session {
    span: Span,
    started: Instant,
    connection: Connection,
    in_flight: HashMap<RequestId, RequestSpan>,
    content: ContentCapture,
    metrics: ProxyMetrics,
}

impl Session {
    pub(super) fn start(server_name: &str, endpoint: Option<Endpoint>, content: ContentCapture) -> Self {
        let span = tracing::info_span!(
            "mcp session",
            "otel.status_code" = Empty,
            { MCP_SERVER_NAME } = server_name,
            { MCP_AUTH_OUTCOME } = Empty,
            { MCP_EXIT_REASON } = Empty,
            { MCP_PROTOCOL_VERSION } = Empty,
            { MCP_SESSION_ID } = Empty,
            { NETWORK_TRANSPORT } = Empty,
            { NETWORK_PROTOCOL_NAME } = Empty,
            { SERVER_ADDRESS } = Empty,
            { SERVER_PORT } = Empty,
            { ERROR_TYPE } = Empty,
        );
        let connection = Connection::new(server_name, endpoint);
        connection.record_on(&span);
        Self {
            span,
            started: Instant::now(),
            connection,
            in_flight: HashMap::new(),
            content,
            metrics: ProxyMetrics::new(),
        }
    }

    pub(super) fn span(&self) -> &Span {
        &self.span
    }

    pub(super) fn session_id(&self) -> SessionIdSlot {
        self.connection.session_id.clone()
    }

    pub(super) fn record_auth(&self, outcome: AuthOutcome) {
        self.span.record(MCP_AUTH_OUTCOME, outcome.as_str());
    }

    /// Opens a request span for a host request about to be forwarded and
    /// points its `_meta` trace context at that span.
    pub(super) fn on_host_message(&mut self, message: &mut RxJsonRpcMessage<RoleServer>) {
        match message {
            JsonRpcMessage::Request(request) => {
                let operation = RequestSpan::open(
                    &request.request,
                    &request.id,
                    &self.span,
                    &self.connection,
                    self.content,
                );
                operation.inject(&mut request.request);
                self.in_flight.insert(request.id.clone(), operation);
            }
            JsonRpcMessage::Notification(notification) => {
                self.notification(&notification.notification, Direction::HostToRemote);
            }
            JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_) => {}
        }
    }

    /// Closes the request span a remote answer belongs to.
    pub(super) fn on_remote_message(&mut self, message: &RxJsonRpcMessage<RoleClient>) {
        match message {
            JsonRpcMessage::Response(response) => {
                let Some(operation) = self.in_flight.remove(&response.id) else {
                    return;
                };
                let error_type = self.succeeded(&operation, &response.result);
                self.close(operation, error_type.as_deref());
            }
            JsonRpcMessage::Error(error) => {
                let Some(operation) = error.id.as_ref().and_then(|id| self.in_flight.remove(id)) else {
                    return;
                };
                let error_type = operation.fail_rpc(&error.error);
                self.close(operation, Some(&error_type));
            }
            JsonRpcMessage::Notification(notification) => {
                self.notification(&notification.notification, Direction::RemoteToHost);
            }
            JsonRpcMessage::Request(_) => {}
        }
    }

    fn succeeded(&mut self, request: &RequestSpan, result: &impl Serialize) -> Option<String> {
        let is_tool_call = request.method == mcp_method_name::TOOLS_CALL;
        let is_initialize = request.method == mcp_method_name::INITIALIZE;
        if !is_tool_call && !is_initialize {
            return None;
        }
        let result = serde_json::to_value(result).ok()?;
        if is_initialize {
            if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
                self.connection.protocol_version = Some(version.to_string());
                self.span.record(MCP_PROTOCOL_VERSION, version);
                request.span.record(MCP_PROTOCOL_VERSION, version);
            }
            return None;
        }
        if self.content.includes_span() {
            if let Some(serialized) = to_json(&result) {
                request.span.record(GEN_AI_TOOL_CALL_RESULT, serialized.as_str());
            }
        }
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            request.fail(TOOL_ERROR);
            return Some(TOOL_ERROR.to_string());
        }
        None
    }

    fn close(&self, operation: RequestSpan, error_type: Option<&str>) {
        if let Some(id) = self.connection.session_id.get() {
            operation.span.record(MCP_SESSION_ID, id.as_str());
        }
        operation.record_duration(&self.metrics.operation, &self.connection, error_type);
    }

    fn notification(&self, notification: &impl Serialize, direction: Direction) {
        let method = serde_json::to_value(notification)
            .ok()
            .and_then(|value| value.get("method").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        self.span.add_event(
            "mcp notification",
            vec![
                KeyValue::new(MCP_METHOD_NAME, method),
                KeyValue::new(MCP_MESSAGE_DIRECTION, direction.as_str()),
            ],
        );
    }

    /// Ends every request still waiting for an answer, then the session.
    pub(super) fn finish(mut self, end: SessionEnd) {
        let abandoned = match end {
            SessionEnd::Exited(reason) => reason.as_str(),
            SessionEnd::Failed(error_type) => error_type,
        };
        for (_, operation) in std::mem::take(&mut self.in_flight) {
            operation.fail(abandoned);
            self.close(operation, Some(abandoned));
        }

        if let SessionEnd::Exited(reason) = end {
            self.span.record(MCP_EXIT_REASON, reason.as_str());
        }
        if let Some(id) = self.connection.session_id.get() {
            self.span.record(MCP_SESSION_ID, id.as_str());
        }
        let error_type = end.error_type();
        if let Some(error_type) = error_type {
            record_error(&self.span, error_type);
        }

        let mut attributes = Vec::new();
        if let Some(error_type) = error_type {
            attributes.push(KeyValue::new(ERROR_TYPE, error_type));
        }
        session_attributes(&self.connection, &mut attributes);
        self.metrics
            .session
            .record(self.started.elapsed().as_secs_f64(), &attributes);
    }
}

/// A streamable HTTP client that notes the session id the server assigns and
/// runs every request inside the session span, so the token refreshes and
/// credential reads rmcp makes from its transport worker land in the
/// session's trace.
#[derive(Clone)]
pub(super) struct ObservedHttpClient<C> {
    inner: C,
    session_id: SessionIdSlot,
    span: Span,
}

impl<C> ObservedHttpClient<C> {
    pub(super) fn new(inner: C, session: &Session) -> Self {
        Self {
            inner,
            session_id: session.session_id(),
            span: session.span().clone(),
        }
    }

    fn observe(&self, session_id: Option<&str>) {
        if let Some(id) = session_id {
            self.session_id.observe(id);
        }
    }

    fn observe_post<'a, E>(
        &'a self,
        response: impl Future<Output = Result<StreamableHttpPostResponse, E>> + Send + 'a,
    ) -> impl Future<Output = Result<StreamableHttpPostResponse, E>> + Send + 'a
    where
        C: Sync,
    {
        async move {
            let response = response.await;
            if let Ok(StreamableHttpPostResponse::Json(_, Some(id)) | StreamableHttpPostResponse::Sse(_, Some(id))) =
                &response
            {
                self.session_id.observe(id);
            }
            response
        }
        .instrument(self.span.clone())
    }
}

impl<C> StreamableHttpClient for ObservedHttpClient<C>
where
    C: StreamableHttpClient + Sync,
{
    type Error = C::Error;

    fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>>> + Send + '_ {
        self.observe(session_id.as_deref());
        self.observe_post(
            self.inner
                .post_message(uri, message, session_id, auth_header, custom_headers),
        )
    }

    fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> impl Future<Output = Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>>> + Send + '_ {
        self.observe(session_id.as_deref());
        self.observe_post(self.inner.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            max_sse_event_size,
        ))
    }

    fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<(), StreamableHttpError<Self::Error>>> + Send + '_ {
        self.observe(Some(&session_id));
        self.inner
            .delete_session(uri, session_id, auth_header, custom_headers)
            .instrument(self.span.clone())
    }

    fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<BoxedSseResponse, StreamableHttpError<Self::Error>>> + Send + '_ {
        self.observe(session_id.as_deref());
        self.inner
            .get_stream(uri, session_id, last_event_id, auth_header, custom_headers)
            .instrument(self.span.clone())
    }

    fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> impl Future<Output = Result<BoxedSseResponse, StreamableHttpError<Self::Error>>> + Send + '_ {
        self.observe(session_id.as_deref());
        self.inner
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                custom_headers,
                max_sse_event_size,
            )
            .instrument(self.span.clone())
    }
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
