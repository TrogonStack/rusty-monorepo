//! Signing through an SSH agent.
//!
//! The signature itself is made by `ssh-keygen`, which already speaks the agent
//! protocol and writes exactly the file git expects. What this module adds is
//! the question asked first: does the agent hold the key at all? An agent that
//! is down, or does not hold the key, is a reason to try the next address. An
//! agent that holds it and then declines to sign is an answer, and it is the
//! one thing that must never be routed around, because a person pressing
//! "deny" on an approval prompt meant it.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};

use super::constants::{
    AGENT_IDENTITIES_ANSWER as IDENTITIES_ANSWER, AGENT_IO_TIMEOUT as IO_TIMEOUT,
    AGENT_MAX_ANSWER_LEN as MAX_ANSWER_LEN, AGENT_REQUEST_IDENTITIES as REQUEST_IDENTITIES, SSH_KEYGEN,
};
use crate::config::signers::{read_string, PublicKey};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// The socket could not be reached, which is a property of this machine at
    /// this moment and says nothing about the key.
    #[error("could not reach the agent: {0}")]
    Unreachable(#[source] std::io::Error),

    #[error("the agent does not hold this public key")]
    KeyNotHeld,

    #[error("the agent answered with something that is not an identities list: {0}")]
    Malformed(&'static str),

    /// The agent holds the key and signing still failed.
    #[error("the agent holds the key but `ssh-keygen` did not sign ({0})")]
    SigningFailed(String),

    #[error("could not run `ssh-keygen`: {0}")]
    Spawn(#[source] std::io::Error),
}

impl AgentError {
    /// Whether the next address may be tried.
    ///
    /// Only the two ways of not being able to ask qualify: the agent is not
    /// there, or it is there and does not have the key. A refusal after the key
    /// was listed does not.
    pub fn allows_fallback(&self) -> bool {
        matches!(self, Self::Unreachable(_) | Self::KeyNotHeld)
    }
}

/// Whether the agent at `socket` lists `key`.
pub fn require_key(socket: &Path, key: &PublicKey) -> Result<(), AgentError> {
    let mut stream = UnixStream::connect(socket).map_err(AgentError::Unreachable)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(AgentError::Unreachable)?;

    let mut request = 1u32.to_be_bytes().to_vec();
    request.push(REQUEST_IDENTITIES);
    stream.write_all(&request).map_err(AgentError::Unreachable)?;

    let mut length = [0u8; 4];
    stream.read_exact(&mut length).map_err(AgentError::Unreachable)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_ANSWER_LEN {
        return Err(AgentError::Malformed("implausible length"));
    }
    let mut answer = vec![0u8; length];
    stream.read_exact(&mut answer).map_err(AgentError::Unreachable)?;

    if answer[0] != IDENTITIES_ANSWER {
        return Err(AgentError::Malformed("unexpected message type"));
    }
    let mut rest = &answer[1..];
    let (count, tail) = rest
        .split_at_checked(4)
        .ok_or(AgentError::Malformed("truncated count"))?;
    let count = u32::from_be_bytes(count.try_into().expect("four bytes"));
    rest = tail;

    let wanted = key.blob();
    for _ in 0..count {
        let blob = read_string(&mut rest).ok_or(AgentError::Malformed("truncated key"))?;
        read_string(&mut rest).ok_or(AgentError::Malformed("truncated comment"))?;
        if blob == wanted.as_slice() {
            return Ok(());
        }
    }
    Err(AgentError::KeyNotHeld)
}

/// What `ssh-keygen -Y sign` is asked to do for one file.
pub struct AgentSigning<'a> {
    pub socket: &'a Path,
    pub namespace: &'a str,
    pub key_file: &'a Path,
    pub options: &'a [String],
    pub file: &'a Path,
}

/// Sign `file` through the agent, leaving `<file>.sig` beside it.
pub fn sign(request: &AgentSigning<'_>) -> Result<(), AgentError> {
    let status = Command::new(SSH_KEYGEN)
        .args(["-Y", "sign", "-n", request.namespace, "-f"])
        .arg(request.key_file)
        .arg("-U")
        .args(request.options.iter().map(|o| format!("-O{o}")))
        .arg(request.file)
        .env("SSH_AUTH_SOCK", request.socket)
        .stdin(Stdio::null())
        .status()
        .map_err(AgentError::Spawn)?;
    if status.success() {
        Ok(())
    } else {
        Err(AgentError::SigningFailed(status.to_string()))
    }
}
