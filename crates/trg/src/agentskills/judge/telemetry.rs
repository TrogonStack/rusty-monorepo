//! The `chat {model}` inference span, its HTTP client span, and the GenAI
//! client metrics recorded for every judging turn.
//!
//! Message content reaches a span only when [`ContentCapture`] allows it: the
//! user turn quotes the transcript under evaluation.

use std::time::Instant;

use opentelemetry::metrics::Meter;
use opentelemetry::KeyValue;
use opentelemetry_semantic_conventions::attribute::{
    ERROR_TYPE, HTTP_REQUEST_METHOD, HTTP_RESPONSE_STATUS_CODE, SERVER_ADDRESS, SERVER_PORT, URL_FULL,
};
use tracing::field::Empty;

use super::{JudgeApi, JudgeEndpoint, JudgeFailure, JudgeProvider, JudgeReply, JudgeRequest, JudgeUsage};
use crate::telemetry::semconv::generated::attributes::{
    gen_ai_operation_name, gen_ai_output_type, gen_ai_provider_name, GEN_AI_INPUT_MESSAGES, GEN_AI_OPERATION_NAME,
    GEN_AI_OUTPUT_MESSAGES, GEN_AI_OUTPUT_TYPE, GEN_AI_PROVIDER_NAME, GEN_AI_REQUEST_MAX_TOKENS, GEN_AI_REQUEST_MODEL,
    GEN_AI_RESPONSE_ID, GEN_AI_RESPONSE_MODEL, GEN_AI_SYSTEM_INSTRUCTIONS, GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS,
    GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS, GEN_AI_USAGE_INPUT_TOKENS, GEN_AI_USAGE_OUTPUT_TOKENS,
};
use crate::telemetry::semconv::generated::metrics::{
    GEN_AI_CLIENT_INFERENCE_DURATION, GEN_AI_CLIENT_INFERENCE_OPERATION_INPUT_TOKENS,
    GEN_AI_CLIENT_INFERENCE_OPERATION_OUTPUT_TOKENS, GEN_AI_CLIENT_INFERENCE_USAGE_CACHE_READ_INPUT_TOKENS,
    GEN_AI_CLIENT_INFERENCE_USAGE_CACHE_WRITE_INPUT_TOKENS, GEN_AI_CLIENT_INFERENCE_USAGE_INPUT_TOKENS,
    GEN_AI_CLIENT_INFERENCE_USAGE_OUTPUT_TOKENS,
};
use crate::telemetry::ContentCapture;

const POST: &str = "POST";
const REDACTED: &str = "REDACTED";

impl JudgeProvider {
    /// The `gen_ai.provider.name` a judge turn is reported under. A compatible
    /// endpoint speaks OpenAI's protocol and is reported as OpenAI, the way
    /// OpenAI client instrumentations report any base URL they are pointed at.
    fn semconv_name(self) -> &'static str {
        match self {
            Self::OpenAi | Self::Compatible => gen_ai_provider_name::OPENAI,
            Self::Anthropic => gen_ai_provider_name::ANTHROPIC,
        }
    }
}

/// Where a judge endpoint lives, split the way the conventions name it, with
/// any credential or query value a base URL carried replaced by `REDACTED`.
struct EndpointAddress {
    host: Option<String>,
    port: Option<u16>,
    url_full: String,
}

impl EndpointAddress {
    fn of(endpoint: &JudgeEndpoint) -> Self {
        let raw = endpoint.url();
        let Ok(mut url) = reqwest::Url::parse(&raw) else {
            return Self {
                host: None,
                port: None,
                url_full: REDACTED.to_string(),
            };
        };
        if !url.username().is_empty() {
            let _ = url.set_username(REDACTED);
        }
        if url.password().is_some() {
            let _ = url.set_password(Some(REDACTED));
        }
        if url.query().is_some() {
            let keys: Vec<String> = url.query_pairs().map(|(key, _)| key.into_owned()).collect();
            url.query_pairs_mut()
                .clear()
                .extend_pairs(keys.iter().map(|key| (key.as_str(), REDACTED)));
        }
        url.set_fragment(None);
        Self {
            host: url.host_str().map(str::to_string),
            port: url.port_or_known_default(),
            url_full: url.to_string(),
        }
    }
}

/// One judging turn as a GenAI client inference span.
pub(super) struct InferenceSpan {
    span: tracing::Span,
    started: Instant,
    content: ContentCapture,
    provider: &'static str,
    request_model: String,
    address: EndpointAddress,
}

impl InferenceSpan {
    pub(super) fn start(endpoint: &JudgeEndpoint, request: &JudgeRequest, content: ContentCapture) -> Self {
        let api = endpoint.provider.api();
        let provider = endpoint.provider.semconv_name();
        let request_model = endpoint.model.as_str().to_string();
        let address = EndpointAddress::of(endpoint);
        let span = tracing::info_span!(
            "chat",
            "otel.name" = format!("{} {request_model}", gen_ai_operation_name::CHAT),
            "otel.kind" = "client",
            "otel.status_code" = Empty,
            { GEN_AI_OPERATION_NAME } = gen_ai_operation_name::CHAT,
            { GEN_AI_PROVIDER_NAME } = provider,
            { GEN_AI_REQUEST_MODEL } = request_model.as_str(),
            { GEN_AI_REQUEST_MAX_TOKENS } = Empty,
            { GEN_AI_OUTPUT_TYPE } = Empty,
            { GEN_AI_RESPONSE_MODEL } = Empty,
            { GEN_AI_RESPONSE_ID } = Empty,
            { GEN_AI_USAGE_INPUT_TOKENS } = Empty,
            { GEN_AI_USAGE_OUTPUT_TOKENS } = Empty,
            { GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS } = Empty,
            { GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS } = Empty,
            { GEN_AI_SYSTEM_INSTRUCTIONS } = Empty,
            { GEN_AI_INPUT_MESSAGES } = Empty,
            { GEN_AI_OUTPUT_MESSAGES } = Empty,
            { SERVER_ADDRESS } = Empty,
            { SERVER_PORT } = Empty,
            { ERROR_TYPE } = Empty,
        );
        match api {
            JudgeApi::OpenAiChatCompletions => {
                span.record(GEN_AI_OUTPUT_TYPE, gen_ai_output_type::JSON);
            }
            JudgeApi::AnthropicMessages => {
                span.record(GEN_AI_REQUEST_MAX_TOKENS, i64::from(request.max_tokens));
            }
        }
        if let Some(host) = &address.host {
            span.record(SERVER_ADDRESS, host.as_str());
        }
        if let Some(port) = address.port {
            span.record(SERVER_PORT, i64::from(port));
        }
        if content.includes_span() {
            span.record(GEN_AI_SYSTEM_INSTRUCTIONS, text_parts(&request.system).to_string());
            span.record(
                GEN_AI_INPUT_MESSAGES,
                serde_json::json!([{"role": "user", "parts": text_parts(&request.user)}]).to_string(),
            );
        }
        Self {
            span,
            started: Instant::now(),
            content,
            provider,
            request_model,
            address,
        }
    }

    pub(super) fn span(&self) -> &tracing::Span {
        &self.span
    }

    /// The HTTP request this turn is carried by, as a child client span.
    pub(super) fn http(&self) -> HttpSpan {
        let span = self.span.in_scope(|| {
            tracing::info_span!(
                "http",
                "otel.name" = POST,
                "otel.kind" = "client",
                "otel.status_code" = Empty,
                { HTTP_REQUEST_METHOD } = POST,
                { URL_FULL } = self.address.url_full.as_str(),
                { SERVER_ADDRESS } = Empty,
                { SERVER_PORT } = Empty,
                { HTTP_RESPONSE_STATUS_CODE } = Empty,
                { ERROR_TYPE } = Empty,
            )
        });
        if let Some(host) = &self.address.host {
            span.record(SERVER_ADDRESS, host.as_str());
        }
        if let Some(port) = self.address.port {
            span.record(SERVER_PORT, i64::from(port));
        }
        HttpSpan(span)
    }

    pub(super) fn finish(&self, result: &Result<JudgeReply, JudgeFailure>) {
        let elapsed = self.started.elapsed().as_secs_f64();
        let mut attributes = vec![
            KeyValue::new(GEN_AI_OPERATION_NAME, gen_ai_operation_name::CHAT),
            KeyValue::new(GEN_AI_PROVIDER_NAME, self.provider),
            KeyValue::new(GEN_AI_REQUEST_MODEL, self.request_model.clone()),
        ];
        if let Some(host) = &self.address.host {
            attributes.push(KeyValue::new(SERVER_ADDRESS, host.clone()));
        }
        if let Some(port) = self.address.port {
            attributes.push(KeyValue::new(SERVER_PORT, i64::from(port)));
        }
        match result {
            Ok(reply) => {
                self.replied(reply);
                if let Some(model) = &reply.response_model {
                    attributes.push(KeyValue::new(GEN_AI_RESPONSE_MODEL, model.as_str().to_string()));
                }
                let meter = opentelemetry::global::meter("trg");
                record_duration(&meter, elapsed, &attributes);
                record_usage(&meter, &reply.usage, &attributes);
            }
            Err(failure) => {
                let error_type = failure.error_type();
                record_error(&self.span, &error_type);
                attributes.push(KeyValue::new(ERROR_TYPE, error_type.into_owned()));
                record_duration(&opentelemetry::global::meter("trg"), elapsed, &attributes);
            }
        }
    }

    fn replied(&self, reply: &JudgeReply) {
        if let Some(model) = &reply.response_model {
            self.span.record(GEN_AI_RESPONSE_MODEL, model.as_str());
        }
        if let Some(id) = &reply.response_id {
            self.span.record(GEN_AI_RESPONSE_ID, id.as_str());
        }
        let usage = &reply.usage;
        for (key, value) in [
            (GEN_AI_USAGE_INPUT_TOKENS, usage.input_tokens),
            (GEN_AI_USAGE_OUTPUT_TOKENS, usage.output_tokens),
            (GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS, usage.cache_read_input_tokens),
            (GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS, usage.cache_write_input_tokens),
        ] {
            if let Some(value) = value.and_then(|value| i64::try_from(value).ok()) {
                self.span.record(key, value);
            }
        }
        if self.content.includes_span() {
            let mut message = serde_json::json!({"role": "assistant", "parts": text_parts(&reply.content)});
            if let Some(reason) = &reply.finish_reason {
                message["finish_reason"] = serde_json::Value::String(reason.clone());
            }
            self.span
                .record(GEN_AI_OUTPUT_MESSAGES, serde_json::json!([message]).to_string());
        }
    }
}

/// The HTTP request one judging turn is carried by.
pub(super) struct HttpSpan(tracing::Span);

impl HttpSpan {
    pub(super) fn span(&self) -> &tracing::Span {
        &self.0
    }

    pub(super) fn responded(&self, status: reqwest::StatusCode) {
        self.0.record(HTTP_RESPONSE_STATUS_CODE, i64::from(status.as_u16()));
    }

    pub(super) fn finish<T>(&self, result: &Result<T, JudgeFailure>) {
        if let Err(failure) = result {
            record_error(&self.0, &failure.error_type());
        }
    }
}

fn text_parts(text: &str) -> serde_json::Value {
    serde_json::json!([{"type": "text", "content": text}])
}

fn record_error(span: &tracing::Span, error_type: &str) {
    span.record(ERROR_TYPE, error_type);
    span.record("otel.status_code", "ERROR");
}

fn record_duration(meter: &Meter, seconds: f64, attributes: &[KeyValue]) {
    meter
        .f64_histogram(GEN_AI_CLIENT_INFERENCE_DURATION)
        .with_unit("s")
        .build()
        .record(seconds, attributes);
}

fn record_usage(meter: &Meter, usage: &JudgeUsage, attributes: &[KeyValue]) {
    for (name, value) in [
        (GEN_AI_CLIENT_INFERENCE_USAGE_INPUT_TOKENS, usage.input_tokens),
        (GEN_AI_CLIENT_INFERENCE_USAGE_OUTPUT_TOKENS, usage.output_tokens),
        (
            GEN_AI_CLIENT_INFERENCE_USAGE_CACHE_READ_INPUT_TOKENS,
            usage.cache_read_input_tokens,
        ),
        (
            GEN_AI_CLIENT_INFERENCE_USAGE_CACHE_WRITE_INPUT_TOKENS,
            usage.cache_write_input_tokens,
        ),
    ] {
        if let Some(value) = value {
            meter
                .u64_counter(name)
                .with_unit("{token}")
                .build()
                .add(value, attributes);
        }
    }
    for (name, value) in [
        (GEN_AI_CLIENT_INFERENCE_OPERATION_INPUT_TOKENS, usage.input_tokens),
        (GEN_AI_CLIENT_INFERENCE_OPERATION_OUTPUT_TOKENS, usage.output_tokens),
    ] {
        if let Some(value) = value {
            meter
                .u64_histogram(name)
                .with_unit("{token}")
                .build()
                .record(value, attributes);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use opentelemetry::trace::{SpanKind, Status};
    use opentelemetry::Value;

    use super::super::{judge, JudgeEnv, JudgeModel, API_KEY_ENV, BASE_URL_ENV};
    use super::*;
    use crate::telemetry::testing::capture;

    struct LocalEnv(String);

    impl JudgeEnv for LocalEnv {
        fn var(&self, key: &str) -> Option<String> {
            match key {
                BASE_URL_ENV => Some(self.0.clone()),
                API_KEY_ENV => Some("test-key".to_string()),
                _ => None,
            }
        }
    }

    /// Serves one canned HTTP response and returns the base URL to reach it.
    fn serve_once(status: &'static str, body: serde_json::Value) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut request = vec![0; length];
            reader.read_exact(&mut request).unwrap();
            let body = body.to_string();
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        format!("http://{address}/v1")
    }

    fn endpoint(provider: JudgeProvider, base_url: String) -> JudgeEndpoint {
        JudgeEndpoint::resolve_from(
            &LocalEnv(base_url),
            provider,
            &JudgeModel::new("judge-model").unwrap(),
            "judge_model",
        )
        .unwrap()
    }

    fn int(value: Option<Value>) -> Option<i64> {
        match value {
            Some(Value::I64(value)) => Some(value),
            _ => None,
        }
    }

    fn text(value: Option<Value>) -> Option<String> {
        value.map(|value| value.as_str().into_owned())
    }

    #[test]
    fn an_openai_ballot_records_a_chat_client_span_with_usage_and_an_http_child() {
        let base_url = serve_once(
            "200 OK",
            serde_json::json!({
                "id": "chatcmpl-1",
                "model": "judge-model-2026",
                "choices": [{"message": {"role": "assistant", "content": "{\"pass\":true}"}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 120, "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 100}},
            }),
        );
        let endpoint = endpoint(JudgeProvider::OpenAi, base_url);

        let (reply, trace) = capture(|| judge(&endpoint, &JudgeRequest::new("system", "user"), "judge_model"));

        assert_eq!(reply.unwrap().content, "{\"pass\":true}");
        let chat = trace.span("chat judge-model").expect("chat span exported");
        assert_eq!(chat.span_kind, SpanKind::Client);
        assert_eq!(
            text(trace.attribute("chat judge-model", GEN_AI_PROVIDER_NAME)).as_deref(),
            Some(gen_ai_provider_name::OPENAI)
        );
        assert_eq!(
            text(trace.attribute("chat judge-model", GEN_AI_RESPONSE_MODEL)).as_deref(),
            Some("judge-model-2026")
        );
        assert_eq!(
            int(trace.attribute("chat judge-model", GEN_AI_USAGE_INPUT_TOKENS)),
            Some(120)
        );
        assert_eq!(
            int(trace.attribute("chat judge-model", GEN_AI_USAGE_OUTPUT_TOKENS)),
            Some(7)
        );
        assert_eq!(
            int(trace.attribute("chat judge-model", GEN_AI_USAGE_CACHE_READ_INPUT_TOKENS)),
            Some(100)
        );
        assert!(
            trace.attribute("chat judge-model", GEN_AI_INPUT_MESSAGES).is_none(),
            "content stays off the span by default"
        );
        trace.assert_child_of(POST, "chat judge-model");
        let http = trace.span(POST).unwrap();
        assert_eq!(http.span_kind, SpanKind::Client);
        assert_eq!(int(trace.attribute(POST, HTTP_RESPONSE_STATUS_CODE)), Some(200));
    }

    #[test]
    fn an_anthropic_ballot_counts_cached_tokens_into_its_input_tokens() {
        let base_url = serve_once(
            "200 OK",
            serde_json::json!({
                "id": "msg_1",
                "model": "judge-model",
                "content": [{"type": "text", "text": "{\"pass\":false}"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 4, "cache_read_input_tokens": 30, "cache_creation_input_tokens": 5},
            }),
        );
        let endpoint = endpoint(JudgeProvider::Anthropic, base_url);

        let (reply, trace) = capture(|| judge(&endpoint, &JudgeRequest::new("system", "user"), "judge_model"));

        assert!(reply.is_ok());
        assert_eq!(
            text(trace.attribute("chat judge-model", GEN_AI_PROVIDER_NAME)).as_deref(),
            Some(gen_ai_provider_name::ANTHROPIC)
        );
        assert_eq!(
            int(trace.attribute("chat judge-model", GEN_AI_USAGE_INPUT_TOKENS)),
            Some(45)
        );
        assert_eq!(
            int(trace.attribute("chat judge-model", GEN_AI_USAGE_OUTPUT_TOKENS)),
            Some(4)
        );
        assert_eq!(
            int(trace.attribute("chat judge-model", GEN_AI_USAGE_CACHE_WRITE_INPUT_TOKENS)),
            Some(5)
        );
    }

    #[test]
    fn a_rejected_ballot_marks_both_spans_failed_with_the_status_code() {
        let base_url = serve_once("429 Too Many Requests", serde_json::json!({"error": "slow down"}));
        let endpoint = endpoint(JudgeProvider::OpenAi, base_url);

        let (reply, trace) = capture(|| judge(&endpoint, &JudgeRequest::new("system", "user"), "judge_model"));

        assert!(reply.is_err());
        assert_eq!(
            text(trace.attribute("chat judge-model", ERROR_TYPE)).as_deref(),
            Some("429")
        );
        assert!(matches!(
            trace.span("chat judge-model").unwrap().status,
            Status::Error { .. }
        ));
        assert_eq!(text(trace.attribute(POST, ERROR_TYPE)).as_deref(), Some("429"));
        assert_eq!(int(trace.attribute(POST, HTTP_RESPONSE_STATUS_CODE)), Some(429));
    }
}
