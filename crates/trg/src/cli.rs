//! The `trg` command tree.
//!
//! Defined in the library rather than in `main.rs` so the parser the binary
//! uses is the same one tests can hand a command line to. Every `trg ...`
//! invocation printed in the docs is checked against it, which is only
//! meaningful if there is one definition rather than two.

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use crate::commands::Commands;
use crate::telemetry::CommandIdentity;

#[derive(Parser)]
#[command(name = "trg")]
#[command(version)]
#[command(about = "TrogonStack tools and utilities")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// Parses the real process arguments once, returning both the typed
    /// `Cli` and the subcommand path it resolved to (`mcp proxy`, `ai skills
    /// eval run`, ...), for the `trg.command` resource attribute.
    ///
    /// `Cli::parse()` followed by a second `Cli::command().get_matches()`
    /// would parse argv twice: `--help`/`--version`/a parse error would be
    /// acted on twice over, and the two parses could in principle disagree.
    /// Parsing once into `ArgMatches`, reading the subcommand path off it,
    /// then building `Cli` from those same matches keeps exactly
    /// `Cli::parse()`'s error/help/exit behaviour while touching argv once.
    pub fn parse_with_command_identity() -> (Self, CommandIdentity) {
        Self::from_matches(Self::command().get_matches()).unwrap_or_else(|e| e.exit())
    }

    /// The half of [`Cli::parse_with_command_identity`] after argv is read.
    /// Errors are formatted against the command the way `Cli::parse()`
    /// formats them, so the message and usage printed are unchanged.
    fn from_matches(mut matches: ArgMatches) -> Result<(Self, CommandIdentity), clap::Error> {
        let identity = Self::identity_from_matches(&matches);
        let cli = Self::from_arg_matches_mut(&mut matches).map_err(|e| e.format(&mut Self::command()))?;
        Ok((cli, identity))
    }

    /// Walks `ArgMatches` rather than matching `Commands` by hand, so a new
    /// subcommand doesn't also need a case added here to be named correctly.
    fn identity_from_matches(matches: &ArgMatches) -> CommandIdentity {
        let mut parts = Vec::new();
        let mut current = matches;
        while let Some((name, sub_matches)) = current.subcommand() {
            parts.push(name);
            current = sub_matches;
        }
        CommandIdentity::new(parts.join(" "))
    }
}

#[cfg(test)]
mod tests;
