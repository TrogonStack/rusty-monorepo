pub mod ai;

use clap::Subcommand;

use ai::AiCommands;

#[derive(Subcommand)]
pub enum Commands {
    /// AI agent tooling
    Ai {
        #[command(subcommand)]
        command: AiCommands,
    },
}
