use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::exit_code::ExitCode;
use crate::git::ssh::{self, SignRequest};

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
    /// Sign files the way `ssh-keygen -Y sign` does, with a declared signer
    ///
    /// The signer is the `[git.ssh.signers.<name>]` whose `public_key` is the
    /// key in `--key-file`. Each file is signed to `<file>.sig`.
    Sign(SignArgs),
}

#[derive(Args)]
pub struct SignArgs {
    /// What the signature is for, such as `git`
    #[arg(short = 'n', long)]
    pub namespace: String,

    /// A public key file, or a private key path with a `.pub` beside it
    #[arg(short = 'f', long = "key-file")]
    pub key_file: PathBuf,

    /// Accepted for `ssh-keygen` compatibility; the signer decides how to sign
    #[arg(short = 'U')]
    pub use_agent: bool,

    /// An `ssh-keygen` option, such as `hashalg=sha256`
    #[arg(short = 'O', long = "option")]
    pub options: Vec<String>,

    /// The files to sign
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
}

impl GitCommands {
    pub async fn handle(self) -> ExitCode {
        match self {
            Self::Ssh {
                command: SshCommands::Sign(args),
            } => {
                let request = SignRequest::new(Some(args.namespace), Some(args.key_file), args.options, args.files);
                ExitCode::from_success(ssh::run(request).await == 0)
            }
        }
    }
}
