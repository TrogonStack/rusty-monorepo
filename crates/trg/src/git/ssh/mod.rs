//! Signing for git over SSH, without the private key on this machine.
//!
//! Git signs a commit by running `gpg.ssh.program` as `ssh-keygen -Y sign`.
//! `trg git ssh sign` takes those arguments, signs with an OpenBao Transit key
//! or an SSH agent, and hands every other `-Y` mode back to the real
//! `ssh-keygen`.

pub mod agent;
mod constants;
mod der;
pub mod key_file;
pub mod request;
pub mod sign;
pub mod sshsig;
mod transit_key;

pub use request::{read, Invocation, SignRequest};
pub use sign::{pass_through, run, SignError, SshSigner};
