//! The key file `-f` names, read for the public key it identifies.
//!
//! Git passes either a `.pub` file, a temporary file holding a literal key, or
//! a private key path whose `.pub` sibling holds the public half. A `.pub`
//! sibling is read in preference to the path itself, so a private key is never
//! opened when its public half is beside it: the private half is never what a
//! signer here needs, and a signing program with no business near private keys
//! should not touch one.

use std::path::{Path, PathBuf};

use crate::config::signers::{PublicKey, PublicKeyError};

#[derive(Debug, thiserror::Error)]
pub enum KeyFileError {
    #[error("Couldn't load public key {}: {}", path.display(), io_reason(source))]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Couldn't load public key {}: {cause}", path.display())]
    Invalid {
        path: PathBuf,
        #[source]
        cause: PublicKeyError,
    },
}

/// ssh-keygen words an OS error by its message alone.
fn io_reason(error: &std::io::Error) -> String {
    let text = error.to_string();
    text.split(" (os error").next().unwrap_or(&text).to_string()
}

/// The public key a `-f` argument names, and the file it was read from.
#[derive(Debug, Clone)]
pub struct KeyFile {
    path: PathBuf,
    key: PublicKey,
}

impl KeyFile {
    pub fn read(path: &Path) -> Result<Self, KeyFileError> {
        let sibling = sibling_of(path);
        if path.extension().is_none_or(|ext| ext != "pub") && sibling.is_file() {
            return Self::read_public(sibling);
        }
        let text = std::fs::read_to_string(path).map_err(|source| KeyFileError::Unreadable {
            path: path.to_path_buf(),
            source,
        })?;
        match first_key_line(&text).map(PublicKey::parse) {
            Some(Ok(key)) => Ok(Self {
                path: path.to_path_buf(),
                key,
            }),
            Some(Err(cause @ PublicKeyError::Unsupported(_))) => Err(KeyFileError::Invalid {
                path: path.to_path_buf(),
                cause,
            }),
            Some(Err(_)) | None => Self::read_public(sibling),
        }
    }

    fn read_public(path: PathBuf) -> Result<Self, KeyFileError> {
        let text = std::fs::read_to_string(&path).map_err(|source| KeyFileError::Unreadable {
            path: path.clone(),
            source,
        })?;
        let key = first_key_line(&text)
            .ok_or(PublicKeyError::Shape)
            .and_then(PublicKey::parse)
            .map_err(|cause| KeyFileError::Invalid {
                path: path.clone(),
                cause,
            })?;
        Ok(Self { path, key })
    }

    pub fn key(&self) -> &PublicKey {
        &self.key
    }

    /// A file that holds exactly this public key, which `ssh-keygen` can be
    /// pointed at.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn sibling_of(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".pub");
    PathBuf::from(name)
}

fn first_key_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .filter(|line| !line.starts_with("-----"))
}
