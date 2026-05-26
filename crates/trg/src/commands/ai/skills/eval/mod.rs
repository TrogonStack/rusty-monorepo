mod run;
mod verify;

use crate::fs::FileSystem;
use clap::{Args, Subcommand};

pub use run::RunArgs;
pub use verify::VerifyArgs;

#[derive(Args)]
pub struct EvalArgs {
    #[command(subcommand)]
    pub command: EvalCommands,
}

#[derive(Subcommand)]
pub enum EvalCommands {
    /// Run skill evals and write an artifact bundle
    Run(RunArgs),
    /// Verify a generated eval bundle
    Verify(VerifyArgs),
}

impl EvalArgs {
    pub fn handle(self, fs: &impl FileSystem) -> i32 {
        match self.command {
            EvalCommands::Run(args) => args.handle(fs),
            EvalCommands::Verify(args) => args.handle(fs),
        }
    }
}
