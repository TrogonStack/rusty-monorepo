//! What one `ssh-keygen -Y sign` invocation asks for, and how its command line
//! is read.
//!
//! Git runs `gpg.ssh.program` directly and speaks only the `ssh-keygen -Y`
//! vocabulary to it, so this reads exactly that vocabulary and nothing more.
//! The same request is built from the `trg git ssh sign` flags, so the two
//! entry points cannot disagree about what a signature is.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use super::constants::PROGRAM_NAME;
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

/// How `trg` was invoked when its name is the signing program's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Sign(Result<SignRequest, RequestError>),
    /// Every other `ssh-keygen -Y` mode, which is `ssh-keygen`'s to answer.
    Passthrough(Vec<OsString>),
}

/// Reads a command line as the signing program, or `None` when `trg` was run
/// under its own name and the ordinary command tree applies.
pub fn detect(args: &[OsString]) -> Option<Invocation> {
    let program = std::path::Path::new(args.first()?).file_name()?;
    if program != OsStr::new(PROGRAM_NAME) {
        return None;
    }
    let rest = &args[1..];
    Some(match mode(rest).as_deref() {
        Some("sign") => Invocation::Sign(parse_sign(rest)),
        _ => Invocation::Passthrough(rest.to_vec()),
    })
}

/// The value of `-Y`, in either `-Y sign` or `-Ysign` spelling.
fn mode(args: &[OsString]) -> Option<String> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let text = arg.to_str()?;
        if text == "--" {
            return None;
        }
        if text == "-Y" {
            return args.next()?.to_str().map(str::to_string);
        }
        if let Some(value) = text.strip_prefix("-Y") {
            return Some(value.to_string());
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
            _ if text.starts_with("-f") => key_file = Some(PathBuf::from(value_of("-f", &text[2..], &mut args)?)),
            _ if text.starts_with("-O") => options.push(value_of("-O", &text[2..], &mut args)?),
            _ if text.starts_with('-') && text.len() > 1 => return Err(RequestError::Unrecognized(text.into_owned())),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    SignRequest::new(namespace, key_file, options, files)
}
