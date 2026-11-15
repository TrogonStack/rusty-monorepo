use std::ffi::OsString;

use clap::{Args, Subcommand};

use crate::exit_code::ExitCode;
use crate::git::ssh::{self, Invocation};

#[derive(Subcommand)]
pub enum GitCommands {
    /// Git signing over SSH
    Ssh {
        #[command(subcommand)]
        command: SshCommands,
    },
}

#[derive(Subcommand)]
pub enum SshCommands {
    /// Stand in for `ssh-keygen -Y ...` as git's `gpg.ssh.program`
    ///
    /// Takes the arguments git passes. `-Y sign -n <namespace> -f <key> [-U]
    /// [-O <option>]... <file>...` signs with the `[git.ssh.signers.<name>]`
    /// whose `public_key` is the key in `-f`, writing `<file>.sig`; without
    /// `-Y` the same flags sign too. Every other `-Y` mode is run by the real
    /// `ssh-keygen` with the same arguments.
    Sign(SignArgs),
}

#[derive(Args)]
pub struct SignArgs {
    /// The `ssh-keygen` arguments, passed through as given
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "SSH_KEYGEN_ARGS")]
    pub args: Vec<OsString>,
}

impl GitCommands {
    pub async fn handle(self) -> ExitCode {
        match self {
            Self::Ssh {
                command: SshCommands::Sign(args),
            } => match ssh::read(&args.args) {
                Invocation::Sign(request) => ExitCode::from_success(ssh::run(request).await == 0),
                Invocation::Passthrough(args) => ExitCode::from_success(ssh::pass_through(&args) == 0),
            },
        }
    }
}
