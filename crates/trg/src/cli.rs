//! The `trg` command tree.
//!
//! Defined in the library rather than in `main.rs` so the parser the binary
//! uses is the same one tests can hand a command line to. Every `trg ...`
//! invocation printed in the docs is checked against it, which is only
//! meaningful if there is one definition rather than two.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser};

use crate::commands::git::{GitCommands, SshCommands};
use crate::commands::Commands;
use crate::telemetry::CommandIdentity;

#[derive(Parser)]
#[command(name = "trg")]
#[command(version)]
#[command(about = "TrogonStack tools and utilities")]
pub struct Cli {
    /// The config file to read instead of `$XDG_CONFIG_HOME/trg/config.toml`.
    /// A path that does not exist is an error, not a reason to read the default
    #[arg(long, global = true, env = "TRG_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// The config file this invocation reads: `--config`, else `TRG_CONFIG`,
    /// else the default location. A named path is never swapped for the
    /// default, even when it does not exist.
    pub fn config_path(&self) -> PathBuf {
        self.config.clone().unwrap_or_else(crate::config::trg_config_path)
    }

    /// The `ssh-keygen` arguments when git is running `trg` as its
    /// `gpg.ssh.program`, which is answered before anything else starts.
    pub fn ssh_signing_args(&self) -> Option<&[OsString]> {
        match &self.command {
            Commands::Git {
                command: GitCommands::Ssh {
                    command: SshCommands::Sign(sign),
                },
            } => Some(&sign.args),
            _ => None,
        }
    }

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
