pub mod skills;

use clap::Subcommand;

use skills::SkillsCommands;

#[derive(Subcommand)]
pub enum AiCommands {
    /// Agent Skills management
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
    },
}
