pub mod ai;
pub mod mcp;
pub mod secrets;

use clap::Subcommand;

use ai::AiCommands;
use mcp::McpCommands;
use secrets::SecretsCommands;

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
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
    /// Configured secrets backends (`doctor` subcommand)
    Secrets {
        #[command(subcommand)]
        command: SecretsCommands,
    },
}
