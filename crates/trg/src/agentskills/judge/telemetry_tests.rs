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
