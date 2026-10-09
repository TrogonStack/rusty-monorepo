//! Spans and metrics for `trg mcp proxy`, after the MCP client semantic
//! conventions: one session span for the bridge, one client span per request
//! the host sends through it.
//!
//! Trace context rides in `params._meta` (SEP-414). A request span adopts the
//! host's context when it sent one, and the remote server sees the proxy's
//! span in its place. With no tracer installed the proxy's span context is
//! invalid and the request is forwarded untouched.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use http::{HeaderName, HeaderValue};
use opentelemetry::{
    metrics::Histogram,
    propagation::{Extractor, Injector, TextMapCompositePropagator, TextMapPropagator},
    trace::TraceContextExt,
    Context, KeyValue,
};
use opentelemetry_sdk::propagation::{BaggagePropagator, TraceContextPropagator};
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, JSONRPC_REQUEST_ID, NETWORK_PROTOCOL_NAME, NETWORK_TRANSPORT, RPC_RESPONSE_STATUS_CODE, SERVER_ADDRESS,
    SERVER_PORT,
};
use rmcp::{
    model::{ClientJsonRpcMessage, ClientRequest, ErrorData, GetMeta, JsonRpcMessage, MetaObject, RequestId},
    service::RxJsonRpcMessage,
    transport::{
        common::client_side_sse::BoxedSseResponse,
        streamable_http_client::{StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse},
    },
    RoleClient, RoleServer,
};
use serde::Serialize;
use serde_json::Value;
use tracing::{field::Empty, Instrument, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::{
    oauth::telemetry::{record_error, AuthOutcome},
    telemetry::{
        semconv::{
            generated::{
                attributes::{
                    gen_ai_operation_name, mcp_method_name, GEN_AI_OPERATION_NAME, GEN_AI_PROMPT_NAME,
                    GEN_AI_TOOL_CALL_ARGUMENTS, GEN_AI_TOOL_CALL_RESULT, GEN_AI_TOOL_NAME, MCP_METHOD_NAME,
                    MCP_PROTOCOL_VERSION, MCP_RESOURCE_URI, MCP_SESSION_ID,
                },
                metrics::{MCP_CLIENT_OPERATION_DURATION, MCP_CLIENT_SESSION_DURATION},
            },
            trg::{MCP_AUTH_OUTCOME, MCP_EXIT_REASON, MCP_MESSAGE_DIRECTION, MCP_SERVER_NAME},
        },
        ContentCapture,
    },
};

/// Streamable HTTP always runs over TCP.
const TCP: &str = "tcp";
const HTTP: &str = "http";

/// `error.type` for a `tools/call` the server answered with `isError: true`.
const TOOL_ERROR: &str = "tool_error";

/// Bucket boundaries the MCP conventions recommend for both duration
/// histograms, in seconds.
const DURATION_BUCKETS: [f64; 14] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

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
#[derive(Debug, Clone, Default)]
pub(super) struct Connection {
    endpoint: Option<Endpoint>,
    protocol_version: Option<String>,
    session_id: SessionIdSlot,
}

impl Connection {
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

        Self { span, method, target }
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

    /// Records a refusal the proxy answered itself.
    pub(super) fn refuse(self, error: &ErrorData) {
        self.fail_rpc(error);
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

struct Operation {
    request: RequestSpan,
    started: Instant,
}

struct ProxyMetrics {
    operation: Histogram<f64>,
    session: Histogram<f64>,
}

impl ProxyMetrics {
    fn new() -> Self {
        let meter = opentelemetry::global::meter("trg");
        Self {
            operation: meter
                .f64_histogram(MCP_CLIENT_OPERATION_DURATION)
                .with_unit("s")
                .with_description("Duration of an MCP request as observed by the sender of the request.")
                .with_boundaries(DURATION_BUCKETS.to_vec())
                .build(),
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
    in_flight: HashMap<RequestId, Operation>,
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
        let connection = Connection {
            endpoint,
            ..Connection::default()
        };
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
                self.in_flight.insert(
                    request.id.clone(),
                    Operation {
                        request: operation,
                        started: Instant::now(),
                    },
                );
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
                let error_type = self.succeeded(&operation.request, &response.result);
                self.close(operation, error_type.as_deref());
            }
            JsonRpcMessage::Error(error) => {
                let Some(operation) = error.id.as_ref().and_then(|id| self.in_flight.remove(id)) else {
                    return;
                };
                let error_type = operation.request.fail_rpc(&error.error);
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

    fn close(&self, operation: Operation, error_type: Option<&str>) {
        if let Some(id) = self.connection.session_id.get() {
            operation.request.span.record(MCP_SESSION_ID, id.as_str());
        }
        self.metrics.operation.record(
            operation.started.elapsed().as_secs_f64(),
            &operation.request.metric_attributes(&self.connection, error_type),
        );
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
            operation.request.fail(abandoned);
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
mod tests {
    use opentelemetry::trace::{SpanId, Status, TraceId};
    use opentelemetry::Value as OtelValue;
    use serde_json::json;

    use super::*;
    use crate::telemetry::testing::{capture, CapturedTrace};

    const HOST_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";
    const HOST_SPAN: &str = "b7ad6b7169203331";

    fn host(message: Value) -> RxJsonRpcMessage<RoleServer> {
        serde_json::from_value(message).expect("valid host message")
    }

    fn remote(message: Value) -> RxJsonRpcMessage<RoleClient> {
        serde_json::from_value(message).expect("valid remote message")
    }

    fn tool_call(id: u32) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": "get_weather", "arguments": { "city": "Lima" } },
        })
    }

    fn session(content: ContentCapture) -> Session {
        Session::start("weather", Endpoint::parse("https://mcp.example.com/mcp"), content)
    }

    fn string(trace: &CapturedTrace, span: &str, key: &str) -> Option<String> {
        trace.attribute(span, key).map(|value| value.to_string())
    }

    fn failed(trace: &CapturedTrace, span: &str) -> bool {
        matches!(trace.span(span).map(|s| &s.status), Some(Status::Error { .. }))
    }

    #[test]
    fn a_tools_call_pairs_with_its_response_under_the_session() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(tool_call(7)));
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0",
                "id": 7,
                "result": { "content": [{ "type": "text", "text": "sunny" }], "isError": false },
            })));
            assert!(session.in_flight.is_empty());
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        let span = "tools/call get_weather";
        trace.assert_child_of(span, "mcp session");
        assert_eq!(string(&trace, span, MCP_METHOD_NAME).as_deref(), Some("tools/call"));
        assert_eq!(string(&trace, span, JSONRPC_REQUEST_ID).as_deref(), Some("7"));
        assert_eq!(string(&trace, span, GEN_AI_TOOL_NAME).as_deref(), Some("get_weather"));
        assert_eq!(
            string(&trace, span, GEN_AI_OPERATION_NAME).as_deref(),
            Some(gen_ai_operation_name::EXECUTE_TOOL)
        );
        assert_eq!(string(&trace, span, SERVER_ADDRESS).as_deref(), Some("mcp.example.com"));
        assert_eq!(trace.attribute(span, SERVER_PORT), Some(OtelValue::I64(443)));
        assert_eq!(string(&trace, span, NETWORK_TRANSPORT).as_deref(), Some(TCP));
        assert_eq!(
            trace.span(span).map(|s| s.span_kind.clone()),
            Some(opentelemetry::trace::SpanKind::Client)
        );
        assert!(trace.attribute(span, ERROR_TYPE).is_none());
        assert!(trace.attribute(span, GEN_AI_TOOL_CALL_ARGUMENTS).is_none());
        assert!(trace.attribute(span, GEN_AI_TOOL_CALL_RESULT).is_none());
        assert_eq!(
            string(&trace, "mcp session", MCP_EXIT_REASON).as_deref(),
            Some("host_eof")
        );
        assert!(!failed(&trace, "mcp session"));
    }

    #[test]
    fn a_request_with_no_named_target_is_named_by_its_method_alone() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(json!({
                "jsonrpc": "2.0", "id": "a", "method": "resources/read",
                "params": { "uri": "file:///notes.md" },
            })));
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0", "id": "a", "result": { "contents": [] },
            })));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        assert_eq!(
            string(&trace, "resources/read", MCP_RESOURCE_URI).as_deref(),
            Some("file:///notes.md")
        );
        assert_eq!(
            string(&trace, "resources/read", JSONRPC_REQUEST_ID).as_deref(),
            Some("a")
        );
    }

    #[test]
    fn a_prompt_request_is_named_after_its_prompt() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(json!({
                "jsonrpc": "2.0", "id": 1, "method": "prompts/get", "params": { "name": "summarize" },
            })));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        assert_eq!(
            string(&trace, "prompts/get summarize", GEN_AI_PROMPT_NAME).as_deref(),
            Some("summarize")
        );
    }

    #[test]
    fn tool_arguments_and_results_are_captured_only_when_content_capture_allows() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::SpanOnly);
            session.on_host_message(&mut host(tool_call(1)));
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "content": [{ "type": "text", "text": "sunny" }] },
            })));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        let span = "tools/call get_weather";
        assert_eq!(
            string(&trace, span, GEN_AI_TOOL_CALL_ARGUMENTS).as_deref(),
            Some(r#"{"city":"Lima"}"#)
        );
        assert!(string(&trace, span, GEN_AI_TOOL_CALL_RESULT).is_some_and(|result| result.contains("sunny")));
    }

    #[test]
    fn a_tool_result_flagged_as_an_error_fails_the_span() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(tool_call(1)));
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "content": [{ "type": "text", "text": "no such city" }], "isError": true },
            })));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        assert_eq!(
            string(&trace, "tools/call get_weather", ERROR_TYPE).as_deref(),
            Some(TOOL_ERROR)
        );
        assert!(failed(&trace, "tools/call get_weather"));
    }

    #[test]
    fn a_json_rpc_error_records_its_code() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(tool_call(3)));
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0", "id": 3, "error": { "code": -32602, "message": "unknown tool" },
            })));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        let span = "tools/call get_weather";
        assert_eq!(
            string(&trace, span, RPC_RESPONSE_STATUS_CODE).as_deref(),
            Some("-32602")
        );
        assert_eq!(string(&trace, span, ERROR_TYPE).as_deref(), Some("-32602"));
        assert!(failed(&trace, span));
    }

    #[test]
    fn the_negotiated_protocol_version_lands_on_the_session_and_later_requests() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(json!({
                "jsonrpc": "2.0", "id": 0, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "host", "version": "1" },
                },
            })));
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0", "id": 0,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "serverInfo": { "name": "weather", "version": "1" },
                },
            })));
            session.on_host_message(&mut host(tool_call(1)));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        assert_eq!(
            string(&trace, "mcp session", MCP_PROTOCOL_VERSION).as_deref(),
            Some("2025-06-18")
        );
        assert_eq!(
            string(&trace, "tools/call get_weather", MCP_PROTOCOL_VERSION).as_deref(),
            Some("2025-06-18")
        );
    }

    #[test]
    fn the_session_id_the_transport_saw_lands_on_the_session() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(tool_call(1)));
            session.session_id().observe("abc-123");
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0", "id": 1, "result": { "content": [] },
            })));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        assert_eq!(
            string(&trace, "mcp session", MCP_SESSION_ID).as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            string(&trace, "tools/call get_weather", MCP_SESSION_ID).as_deref(),
            Some("abc-123")
        );
    }

    #[test]
    fn host_trace_context_is_adopted_and_rewritten_to_the_proxy_span() {
        let (forwarded, trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            let mut message = host(json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/list",
                "params": { "_meta": {
                    "traceparent": format!("00-{HOST_TRACE}-{HOST_SPAN}-01"),
                    "tracestate": "vendor=value",
                    "baggage": "tenant=acme",
                    "progressToken": 9,
                } },
            }));
            session.on_host_message(&mut message);
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
            serde_json::to_value(&message).expect("serializable")
        });

        let span = trace.span("tools/list").expect("request span");
        assert_eq!(span.span_context.trace_id(), TraceId::from_hex(HOST_TRACE).unwrap());
        assert_eq!(span.parent_span_id, SpanId::from_hex(HOST_SPAN).unwrap());
        let session = trace.span("mcp session").expect("session span");
        assert!(span
            .links
            .iter()
            .any(|link| link.span_context.span_id() == session.span_context.span_id()));

        let meta = &forwarded["params"]["_meta"];
        assert_eq!(
            meta["traceparent"],
            json!(format!("00-{HOST_TRACE}-{}-01", span.span_context.span_id()))
        );
        assert_eq!(meta["tracestate"], json!("vendor=value"));
        assert_eq!(meta["baggage"], json!("tenant=acme"));
        assert_eq!(meta["progressToken"], json!(9));
    }

    #[test]
    fn a_request_without_trace_context_gains_one_pointing_at_the_proxy_span() {
        let (forwarded, trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            let mut message = host(tool_call(1));
            session.on_host_message(&mut message);
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
            serde_json::to_value(&message).expect("serializable")
        });

        let span = trace.span("tools/call get_weather").expect("request span");
        let meta = &forwarded["params"]["_meta"];
        assert_eq!(
            meta["traceparent"],
            json!(format!(
                "00-{}-{}-01",
                span.span_context.trace_id(),
                span.span_context.span_id()
            ))
        );
        assert!(meta.get("tracestate").is_none());
    }

    #[test]
    fn requests_pass_through_unchanged_when_no_tracer_is_installed() {
        let original = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {
                "name": "get_weather",
                "arguments": { "city": "Lima" },
                "_meta": { "traceparent": format!("00-{HOST_TRACE}-{HOST_SPAN}-01") },
            },
        });
        let without_meta = tool_call(2);

        let forwarded = tracing::subscriber::with_default(tracing_subscriber::registry(), || {
            let mut session = session(ContentCapture::SpanAndEvent);
            let mut with_meta = host(original.clone());
            let mut bare = host(without_meta.clone());
            session.on_host_message(&mut with_meta);
            session.on_host_message(&mut bare);
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
            (
                serde_json::to_value(&with_meta).expect("serializable"),
                serde_json::to_value(&bare).expect("serializable"),
            )
        });

        assert_eq!(forwarded.0, original);
        assert_eq!(forwarded.1, without_meta);
    }

    #[test]
    fn requests_still_in_flight_at_exit_end_with_the_exit_reason() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(tool_call(1)));
            session.finish(SessionEnd::Exited(ExitReason::RemoteClosed));
        });

        assert_eq!(
            string(&trace, "tools/call get_weather", ERROR_TYPE).as_deref(),
            Some("remote_closed")
        );
        assert!(failed(&trace, "tools/call get_weather"));
        assert_eq!(
            string(&trace, "mcp session", MCP_EXIT_REASON).as_deref(),
            Some("remote_closed")
        );
        assert_eq!(
            string(&trace, "mcp session", ERROR_TYPE).as_deref(),
            Some("remote_closed")
        );
        assert!(failed(&trace, "mcp session"));
    }

    #[test]
    fn a_session_that_never_bridged_fails_without_an_exit_reason() {
        let ((), trace) = capture(|| {
            let session = session(ContentCapture::NoContent);
            session.record_auth(AuthOutcome::AlreadyAuthorized);
            session.finish(SessionEnd::Failed("not_a_terminal"));
        });

        assert!(trace.attribute("mcp session", MCP_EXIT_REASON).is_none());
        assert_eq!(
            string(&trace, "mcp session", ERROR_TYPE).as_deref(),
            Some("not_a_terminal")
        );
        assert_eq!(
            string(&trace, "mcp session", MCP_AUTH_OUTCOME).as_deref(),
            Some("already_authorized")
        );
    }

    #[test]
    fn notifications_become_session_events() {
        let ((), trace) = capture(|| {
            let mut session = session(ContentCapture::NoContent);
            session.on_host_message(&mut host(json!({
                "jsonrpc": "2.0", "method": "notifications/initialized",
            })));
            session.on_remote_message(&remote(json!({
                "jsonrpc": "2.0", "method": "notifications/tools/list_changed",
            })));
            session.finish(SessionEnd::Exited(ExitReason::HostEof));
        });

        let session = trace.span("mcp session").expect("session span");
        let methods: Vec<String> = session
            .events
            .iter()
            .filter(|event| event.name == "mcp notification")
            .filter_map(|event| {
                event
                    .attributes
                    .iter()
                    .find(|kv| kv.key.as_str() == MCP_METHOD_NAME)
                    .map(|kv| kv.value.to_string())
            })
            .collect();
        assert_eq!(
            methods,
            vec!["notifications/initialized", "notifications/tools/list_changed"]
        );
        assert_eq!(trace.spans.len(), 1);
    }

    #[test]
    fn endpoints_carry_the_scheme_default_port() {
        assert_eq!(
            Endpoint::parse("http://localhost/mcp"),
            Some(Endpoint {
                address: "localhost".to_string(),
                port: Some(80),
            })
        );
        assert_eq!(Endpoint::parse("not a url"), None);
    }
}
