use opentelemetry::trace::Status;
use opentelemetry::Value;

use super::*;
use crate::telemetry::testing::capture;

fn request(method: &str, uri: &str) -> HttpRequest {
    http::Request::builder()
        .method(method)
        .uri(uri)
        .body(b"code=secret".to_vec())
        .expect("valid request")
}

fn response(status: u16) -> HttpResponse {
    http::Response::builder()
        .status(status)
        .body(Vec::new())
        .expect("valid response")
}

#[test]
fn an_oauth_request_is_an_http_client_span_without_its_query() {
    let ((), trace) = capture(|| {
        let span = http_span(&request("POST", "https://auth.example.com/token?code=secret"));
        record_response::<()>(&span, &Ok(response(200)));
    });

    assert_eq!(
        trace.attribute("POST", URL_FULL).map(|v| v.to_string()).as_deref(),
        Some("https://auth.example.com/token")
    );
    assert_eq!(trace.attribute("POST", SERVER_PORT), Some(Value::I64(443)));
    assert_eq!(
        trace.attribute("POST", HTTP_RESPONSE_STATUS_CODE),
        Some(Value::I64(200))
    );
    assert!(trace.attribute("POST", ERROR_TYPE).is_none());
    assert_eq!(
        trace.span("POST").map(|s| s.span_kind.clone()),
        Some(opentelemetry::trace::SpanKind::Client)
    );
}

#[test]
fn an_explicit_port_is_kept_in_the_url() {
    let ((), trace) = capture(|| {
        http_span(&request(
            "GET",
            "http://127.0.0.1:8080/.well-known/oauth-authorization-server",
        ));
    });

    assert_eq!(
        trace.attribute("GET", URL_FULL).map(|v| v.to_string()).as_deref(),
        Some("http://127.0.0.1:8080/.well-known/oauth-authorization-server")
    );
    assert_eq!(trace.attribute("GET", SERVER_PORT), Some(Value::I64(8080)));
}

#[test]
fn an_error_status_or_a_lost_request_fails_the_span() {
    let ((), trace) = capture(|| {
        record_response::<()>(
            &http_span(&request("POST", "https://a.example/token")),
            &Ok(response(401)),
        );
        record_response(&http_span(&request("GET", "https://a.example/meta")), &Err(()));
    });

    assert_eq!(
        trace.attribute("POST", ERROR_TYPE).map(|v| v.to_string()).as_deref(),
        Some("401")
    );
    assert_eq!(
        trace.attribute("GET", ERROR_TYPE).map(|v| v.to_string()).as_deref(),
        Some(TRANSPORT_ERROR)
    );
    assert!(matches!(
        trace.span("GET").map(|s| &s.status),
        Some(Status::Error { .. })
    ));
}

#[test]
fn a_failed_credential_access_records_its_error_type() {
    let (result, trace) = capture(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(CredentialAccess::Load.traced("weather", async {
                Err::<(), _>(AuthError::CredentialStoreError("backend refused".to_string()))
            }))
    });

    assert!(result.is_err());
    assert_eq!(
        trace
            .attribute("load credentials", ERROR_TYPE)
            .map(|v| v.to_string())
            .as_deref(),
        Some("credential_store_error")
    );
    assert_eq!(
        trace
            .attribute("load credentials", MCP_SERVER_NAME)
            .map(|v| v.to_string())
            .as_deref(),
        Some("weather")
    );
}
