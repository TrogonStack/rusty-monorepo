/// Streamable HTTP always runs over TCP.
pub const TCP: &str = "tcp";

pub const HTTP: &str = "http";

/// `error.type` for a `tools/call` the server answered with `isError: true`.
pub const TOOL_ERROR: &str = "tool_error";

/// Bucket boundaries the MCP conventions recommend for both duration
/// histograms, in seconds.
pub const DURATION_BUCKETS: [f64; 14] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];
