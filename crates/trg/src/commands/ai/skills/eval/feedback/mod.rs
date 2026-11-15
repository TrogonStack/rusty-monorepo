mod init;
mod label;
mod list;
mod validate;

use crate::exit_code::ExitCode;
use crate::fs::FileSystem;
use clap::{Args, Subcommand};

pub use init::FeedbackInitArgs;
pub use label::FeedbackLabelArgs;
pub use list::FeedbackListArgs;
pub use validate::FeedbackValidateArgs;

#[derive(Args)]
pub struct FeedbackArgs {
    #[command(subcommand)]
    pub command: FeedbackCommands,
}

#[derive(Subcommand)]
pub enum FeedbackCommands {
    /// Scaffold empty feedback.json for every run in a report bundle
    Init(FeedbackInitArgs),
    /// List runs that still need human review
    List(FeedbackListArgs),
    /// Schema-validate all feedback.json files in a report bundle
    Validate(FeedbackValidateArgs),
    /// Record a human verdict on one graded assertion
    Label(FeedbackLabelArgs),
}

impl FeedbackArgs {
    pub fn handle(self, _fs: &impl FileSystem) -> ExitCode {
        match self.command {
            FeedbackCommands::Init(args) => args.handle(),
            FeedbackCommands::List(args) => args.handle(),
            FeedbackCommands::Validate(args) => args.handle(),
            FeedbackCommands::Label(args) => args.handle(),
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil;
