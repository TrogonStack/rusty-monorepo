pub mod ai;
pub mod mcp;

use clap::Subcommand;

use ai::AiCommands;
use mcp::McpCommands;

#[derive(Subcommand)]
pub enum Commands {
    /// AI agent tooling
    Ai {
        #[command(subcommand)]
        command: AiCommands,
    },
    /// MCP stdio ⇄ configured HTTP MCP bridge (`proxy` subcommand)
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
}
