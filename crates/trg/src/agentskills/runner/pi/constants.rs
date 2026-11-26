use super::UsageFieldNames;

pub const PROGRAM: &str = "pi";

pub const INSTALL_HINT: &str = "install pi and ensure `pi` is on PATH";

/// The names `parse_outcome` gives the four counts once it has added up every model call;
/// pi itself reports them per assistant message, under `message.usage`.
pub const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "input",
    output: "output",
    cache_read: "cache_read",
    cache_write: "cache_write",
};
