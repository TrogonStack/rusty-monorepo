use std::collections::HashMap;

use secrecy::SecretString;

use super::*;
use crate::secrets::fake::FakeBackend;
use crate::telemetry::testing::capture;

#[test]
fn a_static_authorization_header_needs_no_oauth_and_says_so_on_the_span() {
    let profile = ResolvedMcpServer {
        secrets: None,
        url: SecretString::from("https://example.com/mcp".to_string()),
        transport: None,
        max_disconnected_time: None,
        initial_retry_interval: None,
        override_protocol_version: None,
        http_headers: HashMap::from([(AUTHORIZATION, SecretString::from("Bearer token".to_string()))]),
    };
    let backend = Backend::Fake(FakeBackend::new());
    let path = SecretPath::parse("mcp/weather").expect("path");
    let http = OAuthHttp::traced().expect("client");

    let (outcome, trace) = capture(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(ensure_credentials_for(
                &profile, "weather", &backend, &path, None, &http,
            ))
    });

    assert!(matches!(outcome, Ok(EnsureOutcome::NoAuthRequired)));
    assert_eq!(
        trace
            .attribute("ensure credentials", MCP_AUTH_OUTCOME)
            .map(|value| value.to_string())
            .as_deref(),
        Some("none")
    );
    assert_eq!(trace.spans.len(), 1);
}
