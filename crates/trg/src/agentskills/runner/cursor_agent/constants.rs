use super::UsageFieldNames;

pub const PROGRAM: &str = "cursor-agent";

pub const INSTALL_HINT: &str = "install Cursor Agent CLI and ensure `cursor-agent` is on PATH";

/// How cursor-agent spells the counts in its terminal `result` event usage block.
pub const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "inputTokens",
    output: "outputTokens",
    cache_read: "cacheReadTokens",
    cache_write: "cacheWriteTokens",
};
