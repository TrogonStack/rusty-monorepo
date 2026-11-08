//! The trust gate a real, operator-declared MCP server is admitted through.
//!
//! Every other MCP server `trg` starts is its own `mock-server` binary, answering out of a
//! resolved `MockSet` a suite already declared. A real server is a different thing entirely:
//! it is an arbitrary command that runs as the operator, with whatever reach the operator
//! has, and it answers with whatever its author wrote rather than with anything the suite
//! confines. Starting one is a trust decision, not a mocking detail, so it is gated exactly
//! like the crate's other trust opt-ins (`--allow-scaffold`, `--trust-skill`): withheld by
//! default, and named in the refusal it produces rather than silently skipped.
//!
//! Nothing here is reachable from a suite or case manifest. `resolve_mock_set` never reads
//! a command from a mock declaration, and nothing in this module reads one from a manifest
//! either; the only caller is `record-mcp`'s own CLI flags, so a repo checked out and run
//! through the eval suite has nothing in it that can name a command to start.

use std::fmt;

/// A real MCP server as the operator names it on the command line: a program and its
/// arguments, kept together because a caller reporting or refusing "the server" means both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealServerCommand {
    program: String,
    args: Vec<String>,
}

impl RealServerCommand {
    pub fn new(program: String, args: Vec<String>) -> Self {
        Self { program, args }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Prints the program but never an argument value. This is what reaches refusal messages,
/// every `RecordingError` variant, the stderr passthrough, and a machine-readable summary's
/// `command` field, and a real MCP server routinely takes a credential as an argument; the
/// program name identifies which server failed, which is all any of those call sites need.
impl fmt::Display for RealServerCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({} arg{})",
            self.program,
            self.args.len(),
            if self.args.len() == 1 { "" } else { "s" }
        )
    }
}

/// Whether the operator agreed to start a real MCP server as themselves.
///
/// Withheld is the default for the same reason `ScaffoldPermission` defaults to withheld: a
/// real server runs author-supplied code with the operator's own reach, so nothing starts
/// one on the strength of a flag alone unless that flag is this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RealServerTrust {
    #[default]
    Withheld,
    Granted,
}

impl RealServerTrust {
    pub fn granted(allowed: bool) -> Self {
        if allowed {
            Self::Granted
        } else {
            Self::Withheld
        }
    }

    pub fn is_granted(self) -> bool {
        matches!(self, Self::Granted)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RealServerError {
    #[error(
        "recording against a real MCP server ('{command}') starts it as you, outside anything a run confines; \
         pass --allow-real-mcp-server to start it"
    )]
    NotGranted { command: String },
}

/// Refuse a real server outright unless the operator opted in for this invocation.
pub fn admit_real_server(command: &RealServerCommand, trust: RealServerTrust) -> Result<(), RealServerError> {
    if trust.is_granted() {
        Ok(())
    } else {
        Err(RealServerError::NotGranted {
            command: command.to_string(),
        })
    }
}

#[cfg(test)]
#[path = "real_mcp_server_tests.rs"]
mod tests;
