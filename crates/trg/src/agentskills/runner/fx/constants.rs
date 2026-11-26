use super::UsageFieldNames;

pub const PROGRAM: &str = "trogonai-agent";

pub const INSTALL_HINT: &str = "install the fx agent and ensure `trogonai-agent` is on PATH";

/// How fx spells the counts in the `usage` block of its terminal `result` object.
pub const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "input_tokens",
    output: "output_tokens",
    cache_read: "cache_read_tokens",
    cache_write: "cache_write_tokens",
};
