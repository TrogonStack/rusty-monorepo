//! The subcommand path a `trg` process is running, e.g. `mcp proxy` or
//! `ai skills eval run`.
//!
//! A newtype rather than a bare `String` so a resource attribute and an
//! arbitrary label can't be swapped by accident at a call site.

/// One `trg` process runs exactly one command, so this is a resource fact
/// (see [`crate::telemetry::semconv::trg::COMMAND`]), not a span attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandIdentity(String);

impl CommandIdentity {
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
