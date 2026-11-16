//! What one `ssh-keygen -Y sign` invocation asks for, and how its command line
//! is read.
//!
//! Git runs `gpg.ssh.program` directly and speaks only the `ssh-keygen -Y`
//! vocabulary to it, so this reads exactly that vocabulary and nothing more.
//! `trg git ssh sign` is handed git's arguments untouched and reads them here.

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::constants::SSH_KEYGEN_VALUE_OPTIONS;
use super::sshsig::{EmptyNamespaceError, HashAlgorithm, Namespace};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("Too few arguments for sign: missing namespace")]
    MissingNamespace,

    #[error("Too few arguments for sign: missing key")]
    MissingKey,

    #[error("Too few arguments for sign: missing file to sign")]
    MissingFile,

    #[error("Invalid option \"{0}\"")]
    InvalidOption(String),

    #[error("sshsig_check_hashalg: unsupported hash algorithm \"{0}\"")]
    UnsupportedHash(String),

    #[error("unrecognized argument `{0}`")]
    Unrecognized(String),
}

/// The `-O` options of a sign, validated.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignOptions {
    hash: HashAlgorithm,
    raw: Vec<String>,
}

impl SignOptions {
    pub fn parse(options: impl IntoIterator<Item = String>) -> Result<Self, RequestError> {
        let mut parsed = Self::default();
        for option in options {
            match option.split_once('=') {
                Some(("hashalg", name)) => {
                    parsed.hash =
                        HashAlgorithm::parse(name).ok_or_else(|| RequestError::UnsupportedHash(name.to_string()))?;
                }
                _ => return Err(RequestError::InvalidOption(option)),
            }
            parsed.raw.push(option);
        }
        Ok(parsed)
    }

    pub fn hash(&self) -> HashAlgorithm {
        self.hash
    }

    /// The options as given, to hand on to `ssh-keygen`.
    pub fn raw(&self) -> &[String] {
        &self.raw
    }
}

/// A validated sign request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignRequest {
    pub namespace: Namespace,
    pub key_file: PathBuf,
    pub options: SignOptions,
    pub files: Vec<PathBuf>,
}

impl SignRequest {
    pub fn new(
        namespace: Option<String>,
        key_file: Option<PathBuf>,
        options: Vec<String>,
        files: Vec<PathBuf>,
    ) -> Result<Self, RequestError> {
        let namespace = Namespace::new(namespace.ok_or(RequestError::MissingNamespace)?)
            .map_err(|EmptyNamespaceError| RequestError::MissingNamespace)?;
        let key_file = key_file.ok_or(RequestError::MissingKey)?;
        if files.is_empty() {
            return Err(RequestError::MissingFile);
        }
        Ok(Self {
            namespace,
            key_file,
            options: SignOptions::parse(options)?,
            files,
        })
    }
}

/// What `trg git ssh sign` was asked to do with the arguments git gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Sign(Result<SignRequest, RequestError>),
    /// Every other `ssh-keygen -Y` mode, which is `ssh-keygen`'s to answer.
    Passthrough(Vec<OsString>),
}

/// Reads the arguments as `ssh-keygen` would. A command line with no `-Y` is
/// a sign, so the flags can be typed by hand.
pub fn read(args: &[OsString]) -> Invocation {
    match mode(args).as_deref() {
        None | Some("sign") => Invocation::Sign(parse_sign(args)),
        Some(_) => Invocation::Passthrough(args.to_vec()),
    }
}

/// The value of `-Y`, in either `-Y sign` or `-Ysign` spelling.
///
/// Options are read the way `getopt` reads them: clustered flags are split,
/// and the value of an option that takes one is skipped, so `-f -Yverify`
/// names a key file rather than a mode.
fn mode(args: &[OsString]) -> Option<String> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let bytes = arg.as_encoded_bytes();
        if bytes == b"--" {
            return None;
        }
        let Some(cluster) = bytes.strip_prefix(b"-").filter(|rest| !rest.is_empty()) else {
            continue;
        };
        for (index, &option) in cluster.iter().enumerate() {
            if !SSH_KEYGEN_VALUE_OPTIONS.contains(&option) {
                continue;
            }
            let attached = &cluster[index + 1..];
            let value = if attached.is_empty() {
                args.next()?.as_encoded_bytes()
            } else {
                attached
            };
            if option == b'Y' {
                return Some(String::from_utf8_lossy(value).into_owned());
            }
            break;
        }
    }
    None
}

fn parse_sign(args: &[OsString]) -> Result<SignRequest, RequestError> {
    let mut namespace = None;
    let mut key_file = None;
    let mut options = Vec::new();
    let mut files = Vec::new();

    let mut args = args.iter();
    let value_of = |flag: &str, attached: &str, args: &mut std::slice::Iter<'_, OsString>| {
        if attached.is_empty() {
            args.next()
                .map(|v| v.to_string_lossy().into_owned())
                .ok_or_else(|| RequestError::Unrecognized(flag.to_string()))
        } else {
            Ok(attached.to_string())
        }
    };

    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        if text == "--" {
            files.extend(args.by_ref().map(PathBuf::from));
            break;
        }
        match text.as_ref() {
            "-U" => continue,
            _ if text.starts_with("-Y") => {
                value_of("-Y", &text[2..], &mut args)?;
            }
            _ if text.starts_with("-n") => namespace = Some(value_of("-n", &text[2..], &mut args)?),
            _ if text.starts_with("-f") => key_file = Some(PathBuf::from(path_value_of(arg, &mut args)?)),
            _ if text.starts_with("-O") => options.push(value_of("-O", &text[2..], &mut args)?),
            _ if text.starts_with('-') && text.len() > 1 => return Err(RequestError::Unrecognized(text.into_owned())),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    SignRequest::new(namespace, key_file, options, files)
}

/// The value of `-f`, kept as the operating system spelled it: a lossy
/// conversion would name a different file.
fn path_value_of(arg: &OsStr, args: &mut std::slice::Iter<'_, OsString>) -> Result<OsString, RequestError> {
    if arg.len() == 2 {
        return args
            .next()
            .cloned()
            .ok_or_else(|| RequestError::Unrecognized("-f".to_string()));
    }
    Ok(after_flag(arg))
}

#[cfg(unix)]
fn after_flag(arg: &OsStr) -> OsString {
    OsStr::from_bytes(&arg.as_bytes()[2..]).to_os_string()
}

#[cfg(not(unix))]
fn after_flag(arg: &OsStr) -> OsString {
    OsString::from(&arg.to_string_lossy()[2..])
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
