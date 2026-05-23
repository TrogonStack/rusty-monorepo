//! `trg mcp proxy`: stdio MCP bridge backed by RMCP streamable-http transport.

mod cli;
mod run;

pub use cli::ProxyArgs;
pub use run::{run_mcp_daemon, ProxyError};
