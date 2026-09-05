pub mod ai;
pub mod doctor;
pub mod mcp;

use clap::Subcommand;

use ai::AiCommands;
use doctor::DoctorArgs;
use mcp::McpCommands;

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
    /// Check that the configured secrets backends are reachable and usable
    Doctor(DoctorArgs),
}
