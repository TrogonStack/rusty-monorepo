//! Signing for git over SSH, without the private key on this machine.
//!
//! Git signs a commit by running `gpg.ssh.program` as `ssh-keygen -Y sign`.
//! `trg` answers to that name, signs with an OpenBao Transit key or an SSH
//! agent, and hands every other `-Y` mode back to the real `ssh-keygen`.

pub mod agent;
mod constants;
pub mod key_file;
pub mod request;
pub mod sign;
pub mod sshsig;

pub use constants::{PROGRAM_NAME, SSH_KEYGEN};
pub use request::{detect, Invocation, SignRequest};
pub use sign::{run, SignError, SshSigner};
