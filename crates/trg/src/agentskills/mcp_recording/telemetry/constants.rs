/// The protocol revision the recording client offers in `initialize`.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC over a child's stdin and stdout.
pub const PIPE: &str = "pipe";

/// `error.type` for a `tools/call` the server answered with `isError: true`.
pub const TOOL_ERROR: &str = "tool_error";

pub const DURATION_BUCKETS: [f64; 14] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];
