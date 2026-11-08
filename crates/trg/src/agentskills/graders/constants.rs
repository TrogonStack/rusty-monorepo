pub const HYPHEN_VARIANTS: [char; 7] = [
    '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
];

pub const SINGLE_QUOTE_VARIANTS: [char; 4] = ['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

pub const DOUBLE_QUOTE_VARIANTS: [char; 4] = ['\u{201C}', '\u{201D}', '\u{201E}', '\u{201F}'];

/// The markup a model writes when it imitates a tool call in its reply instead
/// of making one, matched case-insensitively.
pub const TOOL_CALL_MARKUP: &[&str] = &[
    "<tool>",
    "<tool ",
    "<tool_call",
    "<tool_use",
    "<tool_code",
    "<tool_response",
    "<tool_result",
    "<function_call",
    "<function=",
    "<functioncall",
    "<invoke",
    "<|channel|>",
    "<|tool_call",
    "<|python_tag|>",
    "[tool_calls]",
    "```tool_call",
    "```tool_code",
    "```tool_use",
    "```function_call",
];

/// Said of a run that left no call log at all, which is a run no mock server ever came
/// up for.
pub const NO_MOCK_SERVER_REASON: &str =
    "this run hosted no mcp mock server, so there is no record of what the agent asked one for";
