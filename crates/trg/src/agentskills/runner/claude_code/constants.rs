use super::UsageFieldNames;

pub const PROGRAM: &str = "claude";

pub const INSTALL_HINT: &str = "install Claude Code and ensure `claude` is on PATH";

/// How claude-code spells the counts in its terminal `result` event usage block.
pub const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "input_tokens",
    output: "output_tokens",
    cache_read: "cache_read_input_tokens",
    cache_write: "cache_creation_input_tokens",
};

/// The `result` subtype claude-code ends a run with when `--max-turns` stopped it.
pub const MAX_TURNS_SUBTYPE: &str = "error_max_turns";
