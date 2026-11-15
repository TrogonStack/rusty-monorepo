use super::UsageFieldNames;

pub const PROGRAM: &str = "codex";

pub const INSTALL_HINT: &str = "install Codex CLI and ensure `codex` is on PATH";

/// How codex spells the counts in its `turn.completed` usage block.
pub const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "input_tokens",
    output: "output_tokens",
    cache_read: "cached_input_tokens",
    cache_write: "cache_write_input_tokens",
};
