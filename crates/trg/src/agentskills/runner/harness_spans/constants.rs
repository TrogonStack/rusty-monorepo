pub const CLAUDE_USAGE: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cache_read_input_tokens",
    "cache_creation_input_tokens",
];

pub const CODEX_USAGE: [&str; 4] = [
    "input_tokens",
    "output_tokens",
    "cached_input_tokens",
    "cache_write_input_tokens",
];

pub const CODEX_TOOL_ITEMS: [&str; 4] = ["command_execution", "file_change", "mcp_tool_call", "web_search"];
