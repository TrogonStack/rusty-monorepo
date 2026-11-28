use super::UsageFieldNames;

pub const PROGRAM: &str = "opencode";

pub const INSTALL_HINT: &str = "install opencode and ensure `opencode` is on PATH";

/// The names `parse_outcome` gives the four counts once it has added up every step;
/// opencode itself reports them per step, under `part.tokens`.
pub const USAGE_FIELDS: UsageFieldNames = UsageFieldNames {
    input: "input",
    output: "output",
    cache_read: "cache_read",
    cache_write: "cache_write",
};
