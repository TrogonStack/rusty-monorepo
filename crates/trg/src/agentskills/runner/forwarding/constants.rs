pub const CLAUDE_CODE_SWITCHES: [(&str, &str); 2] = [
    ("CLAUDE_CODE_ENABLE_TELEMETRY", "1"),
    ("CLAUDE_CODE_ENHANCED_TELEMETRY_BETA", "1"),
];

pub const CLAUDE_CODE_CONTENT_SWITCHES: [&str; 2] = ["OTEL_LOG_USER_PROMPTS", "OTEL_LOG_TOOL_DETAILS"];

/// Content switches a harness reads from `OTEL_*`. They never pass on the strength of
/// the prefix: only an operator who opted `trg` itself into content capture gets them.
pub const CONTENT_SWITCHES: [&str; 3] = [
    "OTEL_LOG_USER_PROMPTS",
    "OTEL_LOG_TOOL_DETAILS",
    "OTEL_LOG_TOOL_CONTENT",
];

/// `trg`'s own service identity, which would relabel the harness's spans as `trg`.
pub const SERVICE_IDENTITY: [&str; 1] = ["OTEL_SERVICE_NAME"];

pub const TRACE_CONTEXT_VARS: [&str; 3] = ["TRACEPARENT", "TRACESTATE", "BAGGAGE"];
