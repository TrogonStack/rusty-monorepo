/// JSON-RPC over this process's stdin and stdout.
pub const PIPE: &str = "pipe";

/// `error.type` for a `tools/call` answered with `isError: true`.
pub const TOOL_ERROR: &str = "tool_error";

pub const DURATION_BUCKETS: [f64; 14] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0,
];

pub const METHOD_NOT_FOUND: i64 = -32601;

pub const INVALID_PARAMS: i64 = -32602;

pub const MOCK_SUBSTITUTION_FAILED: i64 = -32000;
