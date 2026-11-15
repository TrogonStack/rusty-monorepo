use super::*;

fn host(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn otel_host() -> BTreeMap<String, String> {
    host(&[
        ("PATH", "/usr/bin"),
        ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
        ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer%20abc,x-team=evals"),
        ("OTEL_SERVICE_NAME", "trg"),
        ("OTEL_LOG_USER_PROMPTS", "1"),
        ("TRACEPARENT", "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"),
    ])
}

#[test]
fn off_forwards_nothing() {
    let vars = TelemetryForwarding::Off.harness_vars(
        Runner::ClaudeCode,
        EnvironmentPolicy::Isolated,
        &otel_host(),
        ContentCapture::SpanAndEvent,
    );
    assert!(vars.is_empty());
}

#[test]
fn claude_code_gets_exporter_vars_and_its_switches_but_not_content_or_identity() {
    let vars = TelemetryForwarding::On.harness_vars(
        Runner::ClaudeCode,
        EnvironmentPolicy::Scrubbed,
        &otel_host(),
        ContentCapture::NoContent,
    );
    assert_eq!(
        vars.get("OTEL_EXPORTER_OTLP_ENDPOINT").map(String::as_str),
        Some("http://collector:4318")
    );
    assert!(vars.contains_key("TRACEPARENT"));
    assert_eq!(vars.get("CLAUDE_CODE_ENABLE_TELEMETRY").map(String::as_str), Some("1"));
    assert_eq!(
        vars.get("CLAUDE_CODE_ENHANCED_TELEMETRY_BETA").map(String::as_str),
        Some("1")
    );
    assert_eq!(vars.get("OTEL_TRACES_EXPORTER").map(String::as_str), Some("otlp"));
    assert!(!vars.contains_key("CLAUDE_CODE_PROPAGATE_TRACEPARENT"));
    assert!(!vars.contains_key("OTEL_LOG_USER_PROMPTS"));
    assert!(!vars.contains_key("OTEL_SERVICE_NAME"));
    assert!(!vars.contains_key("PATH"));
}

#[test]
fn content_switches_follow_trg_s_own_content_capture() {
    let vars = TelemetryForwarding::On.harness_vars(
        Runner::ClaudeCode,
        EnvironmentPolicy::Isolated,
        &otel_host(),
        ContentCapture::SpanOnly,
    );
    assert_eq!(vars.get("OTEL_LOG_USER_PROMPTS").map(String::as_str), Some("1"));
    assert_eq!(vars.get("OTEL_LOG_TOOL_DETAILS").map(String::as_str), Some("1"));
}

#[test]
fn a_custom_base_url_also_propagates_traceparent_to_the_provider() {
    let mut host = otel_host();
    host.insert("ANTHROPIC_BASE_URL".to_string(), "https://gateway".to_string());
    let vars = TelemetryForwarding::On.harness_vars(
        Runner::ClaudeCode,
        EnvironmentPolicy::Isolated,
        &host,
        ContentCapture::NoContent,
    );
    assert_eq!(
        vars.get("CLAUDE_CODE_PROPAGATE_TRACEPARENT").map(String::as_str),
        Some("1")
    );
}

#[test]
fn inherited_adds_only_the_switches_trg_owns() {
    let vars = TelemetryForwarding::On.harness_vars(
        Runner::ClaudeCode,
        EnvironmentPolicy::Inherited,
        &otel_host(),
        ContentCapture::NoContent,
    );
    assert!(!vars.contains_key("OTEL_EXPORTER_OTLP_ENDPOINT"));
    assert!(vars.contains_key("CLAUDE_CODE_ENABLE_TELEMETRY"));
}

#[test]
fn an_operator_chosen_exporter_is_left_alone() {
    let mut host = otel_host();
    host.insert("OTEL_TRACES_EXPORTER".to_string(), "console".to_string());
    let vars = TelemetryForwarding::On.harness_vars(
        Runner::ClaudeCode,
        EnvironmentPolicy::Isolated,
        &host,
        ContentCapture::NoContent,
    );
    assert_eq!(vars.get("OTEL_TRACES_EXPORTER").map(String::as_str), Some("console"));
}

#[test]
fn codex_and_cursor_take_no_variables() {
    for runner in [Runner::Codex, Runner::CursorAgent] {
        let vars = TelemetryForwarding::On.harness_vars(
            runner,
            EnvironmentPolicy::Isolated,
            &otel_host(),
            ContentCapture::NoContent,
        );
        assert!(vars.is_empty(), "{runner:?}");
    }
}

#[test]
fn exporter_headers_are_never_recorded() {
    assert!(is_exporter_header_var("OTEL_EXPORTER_OTLP_HEADERS"));
    assert!(is_exporter_header_var("OTEL_EXPORTER_OTLP_TRACES_HEADERS"));
    assert!(!is_exporter_header_var("OTEL_EXPORTER_OTLP_ENDPOINT"));
}

#[test]
fn codex_otel_table_derives_signal_endpoints_from_the_shared_one() {
    let rendered = CodexOtelConfig::render(&otel_host(), ContentCapture::NoContent).unwrap();
    let parsed: toml::Value = toml::from_str(rendered.as_str()).unwrap();
    let otel = &parsed["otel"];

    let traces = &otel["trace_exporter"]["otlp-http"];
    assert_eq!(traces["endpoint"].as_str(), Some("http://collector:4318/v1/traces"));
    assert_eq!(traces["protocol"].as_str(), Some("binary"));
    assert_eq!(traces["headers"]["authorization"].as_str(), Some("Bearer abc"));
    assert_eq!(traces["headers"]["x-team"].as_str(), Some("evals"));
    assert_eq!(
        otel["exporter"]["otlp-http"]["endpoint"].as_str(),
        Some("http://collector:4318/v1/logs")
    );
    assert_eq!(
        otel["metrics_exporter"]["otlp-http"]["endpoint"].as_str(),
        Some("http://collector:4318/v1/metrics")
    );
    assert!(otel.get("log_user_prompt").is_none());
    assert!(rendered.exports_traces());
}

#[test]
fn codex_otel_table_honours_grpc_signal_endpoints_disabled_signals_and_content() {
    let host = host(&[
        ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
        ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "http://collector:4317"),
        ("OTEL_METRICS_EXPORTER", "none"),
    ]);
    let rendered = CodexOtelConfig::render(&host, ContentCapture::SpanAndEvent).unwrap();
    let parsed: toml::Value = toml::from_str(rendered.as_str()).unwrap();
    let otel = &parsed["otel"];

    assert_eq!(
        otel["trace_exporter"]["otlp-grpc"]["endpoint"].as_str(),
        Some("http://collector:4317")
    );
    assert!(otel.get("exporter").is_none());
    assert!(otel.get("metrics_exporter").is_none());
    assert_eq!(otel["log_user_prompt"].as_bool(), Some(true));

    let logs_only = host_logs_only();
    assert!(!CodexOtelConfig::render(&logs_only, ContentCapture::NoContent)
        .unwrap()
        .exports_traces());
}

fn host_logs_only() -> BTreeMap<String, String> {
    host(&[("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT", "http://collector:4318/v1/logs")])
}

#[test]
fn claude_code_forwards_traces_only_when_handed_a_traces_exporter() {
    let forwards = |host: &BTreeMap<String, String>| TelemetryForwarding::On.forwards_traces(Runner::ClaudeCode, host);
    assert!(forwards(&otel_host()));
    assert!(forwards(&host(&[(
        "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
        "http://collector:4318/v1/traces"
    )])));

    assert!(!forwards(&host(&[("PATH", "/usr/bin")])));
    assert!(!forwards(&host_logs_only()));
    let mut disabled = otel_host();
    disabled.insert("OTEL_TRACES_EXPORTER".to_string(), "none".to_string());
    assert!(!forwards(&disabled));

    let mut console = otel_host();
    console.insert("OTEL_TRACES_EXPORTER".to_string(), "console".to_string());
    assert!(!forwards(&console));
}

#[test]
fn traces_are_forwarded_only_when_asked_and_only_to_a_harness_that_reads_otel_vars() {
    assert!(!TelemetryForwarding::Off.forwards_traces(Runner::ClaudeCode, &otel_host()));
    assert!(!TelemetryForwarding::On.forwards_traces(Runner::Codex, &otel_host()));
    assert!(!TelemetryForwarding::On.forwards_traces(Runner::CursorAgent, &otel_host()));
}

#[test]
fn codex_gets_no_otel_table_when_nothing_is_configured() {
    assert_eq!(
        CodexOtelConfig::render(&host(&[("PATH", "/usr/bin")]), ContentCapture::NoContent),
        None
    );
}
