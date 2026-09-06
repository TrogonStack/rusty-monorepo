//! How a command renders what it found.
//!
//! One spelling for the whole CLI: `--format text` for a person, `--format
//! json` for a program. The choice is the same choice everywhere, so it is one
//! type rather than a per-command enum or a bare `--json` on the commands that
//! happened to grow one first. A caller scripting two `trg` commands should not
//! have to remember which spelling each of them took.

use clap::ValueEnum;
use serde::Serialize;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// A summary meant to be read.
    #[default]
    Text,
    /// A document meant to be parsed.
    Json,
}

impl OutputFormat {
    pub fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

/// Render one document on stdout, answering with the exit code the command
/// should use.
///
/// `code` is the command's own verdict and is kept, since a run that failed
/// still has a failure to report. Only a document that cannot be rendered
/// changes it: at that point the command has done its work and produced
/// nothing a caller can read, which is a failure of its own.
///
/// Under either format the result goes to stdout and everything that stopped
/// the command from producing one goes to stderr, so `--output-format json` piped
/// into a parser never has prose in front of it.
pub fn print_json<T: Serialize>(value: &T, code: i32) -> i32 {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            println!("{rendered}");
            code
        }
        Err(e) => {
            eprintln!("could not render the result as json: {e}");
            1
        }
    }
}
