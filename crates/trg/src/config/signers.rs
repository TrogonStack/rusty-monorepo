//! `[git.ssh.signers.<name>]`: who may sign, and where each signature comes from.
//!
//! A signer is picked by the public key git hands the signing program, so the
//! public key is the identity and the addresses are only where its private half
//! can be reached. Each signer lists one address and, optionally, fallbacks;
//! an address is either an OpenBao Transit key or an SSH agent socket, in
//! whatever order the operator prefers.

mod constants;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::secrets::openbao::expand_tilde;
use crate::secrets::{BackendKind, SecretsSection};

use constants::{DEFAULT_TRANSIT_MOUNT, ED25519, ED25519_KEY_LEN, RSA, RSA_MAX_MODULUS_BYTES, RSA_MIN_MODULUS_BYTES};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublicKeyError {
    #[error("a public key must read `<type> <base64> [comment]`")]
    Shape,

    #[error("the key blob is not valid base64")]
    Encoding(#[source] base64::DecodeError),

    #[error("the key blob is not valid: {0}")]
    Blob(&'static str),

    #[error("`{0}` keys are not supported: only ssh-ed25519 and ssh-rsa can be signed with here")]
    Unsupported(String),
}

/// An SSH public key, compared by type and key material alone.
///
/// The comment is left out on purpose: the same key is routinely written with a
/// different comment in each place it is kept, and that must not make it a
/// different key. Only Ed25519 and RSA are representable, so a key of any other
/// type is refused when it is read instead of when it is first asked to sign.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum PublicKey {
    Ed25519([u8; ED25519_KEY_LEN]),
    Rsa(RsaPublicKey),
}

/// An RSA public key as big-endian integers without leading zeros, so the same
/// key reads the same however it was encoded.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RsaPublicKey {
    exponent: Vec<u8>,
    modulus: Vec<u8>,
}

impl RsaPublicKey {
    pub fn new(exponent: &[u8], modulus: &[u8]) -> Result<Self, PublicKeyError> {
        let exponent = strip_leading_zeros(exponent);
        let modulus = strip_leading_zeros(modulus);
        if exponent.is_empty() {
            return Err(PublicKeyError::Blob("empty RSA exponent"));
        }
        if !(RSA_MIN_MODULUS_BYTES..=RSA_MAX_MODULUS_BYTES).contains(&modulus.len()) {
            return Err(PublicKeyError::Blob("RSA keys must be between 2048 and 8192 bits"));
        }
        Ok(Self {
            exponent: exponent.to_vec(),
            modulus: modulus.to_vec(),
        })
    }

    pub fn exponent(&self) -> &[u8] {
        &self.exponent
    }

    pub fn modulus(&self) -> &[u8] {
        &self.modulus
    }
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    &bytes[zeros..]
}

impl PublicKey {
    /// A line as `authorized_keys` and `.pub` files write it.
    pub fn parse(line: &str) -> Result<Self, PublicKeyError> {
        let mut fields = line.split_whitespace();
        let (Some(declared), Some(encoded)) = (fields.next(), fields.next()) else {
            return Err(PublicKeyError::Shape);
        };
        if declared != ED25519 && declared != RSA {
            return Err(PublicKeyError::Unsupported(declared.to_string()));
        }
        let blob = BASE64.decode(encoded).map_err(PublicKeyError::Encoding)?;
        let mut rest = blob.as_slice();
        let found = read_string(&mut rest).ok_or(PublicKeyError::Blob("truncated"))?;
        if found != declared.as_bytes() {
            return Err(PublicKeyError::Unsupported(String::from_utf8_lossy(found).into_owned()));
        }
        let key = if declared == ED25519 {
            let key = read_string(&mut rest).ok_or(PublicKeyError::Blob("truncated"))?;
            Self::from_ed25519_bytes(key)?
        } else {
            let exponent = read_string(&mut rest).ok_or(PublicKeyError::Blob("truncated"))?;
            let modulus = read_string(&mut rest).ok_or(PublicKeyError::Blob("truncated"))?;
            Self::Rsa(RsaPublicKey::new(exponent, modulus)?)
        };
        if !rest.is_empty() {
            return Err(PublicKeyError::Blob("trailing bytes"));
        }
        Ok(key)
    }

    pub fn from_ed25519_bytes(bytes: &[u8]) -> Result<Self, PublicKeyError> {
        let key = <[u8; ED25519_KEY_LEN]>::try_from(bytes).or(Err(PublicKeyError::Blob("wrong key length")))?;
        Ok(Self::Ed25519(key))
    }

    /// The key type as it is written in a `.pub` file and in the blob.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Ed25519(_) => ED25519,
            Self::Rsa(_) => RSA,
        }
    }

    /// The wire form SSHSIG embeds: the type, then the key.
    pub fn blob(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, self.type_name().as_bytes());
        match self {
            Self::Ed25519(key) => put_string(&mut out, key),
            Self::Rsa(key) => {
                put_mpint(&mut out, key.exponent());
                put_mpint(&mut out, key.modulus());
            }
        }
        out
    }

    /// `<type> <base64>`, with no comment.
    pub fn to_line(&self) -> String {
        format!("{} {}", self.type_name(), BASE64.encode(self.blob()))
    }
}

fn put_mpint(out: &mut Vec<u8>, bytes: &[u8]) {
    if bytes.first().is_some_and(|b| b & 0x80 != 0) {
        let mut padded = Vec::with_capacity(bytes.len() + 1);
        padded.push(0);
        padded.extend_from_slice(bytes);
        put_string(out, &padded);
    } else {
        put_string(out, bytes);
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_line())
    }
}

impl TryFrom<String> for PublicKey {
    type Error = PublicKeyError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

pub fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).expect("an SSH string is shorter than 4 GiB");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

pub fn read_string<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, rest) = input.split_at_checked(4)?;
    let len = u32::from_be_bytes(len.try_into().ok()?) as usize;
    let (string, rest) = rest.split_at_checked(len)?;
    *input = rest;
    Some(string)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignerNameError {
    #[error("a signer name must not be empty")]
    Empty,

    #[error("signer name `{0}` may hold only ASCII letters, digits, `-` and `_`")]
    Characters(String),
}

/// The name a `[git.ssh.signers.<name>]` table is declared under.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct SignerName(String);

impl SignerName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SignerName {
    type Error = SignerNameError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() {
            return Err(SignerNameError::Empty);
        }
        if !raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
            return Err(SignerNameError::Characters(raw));
        }
        Ok(Self(raw))
    }
}

impl fmt::Display for SignerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The `[git]` table.
#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GitSection {
    #[serde(default)]
    pub ssh: GitSshSection,
}

/// The `[git.ssh]` table.
#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GitSshSection {
    #[serde(default)]
    pub signers: BTreeMap<SignerName, RawSigner>,
}

/// One `[git.ssh.signers.<name>]` table as the file spelled it.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawSigner {
    pub public_key: PublicKey,
    pub signer: RawPrimaryAddress,
}

/// The first address, which alone may carry `fallbacks`.
///
/// Fallback entries are [`RawAddress`], which has no `fallbacks` field, so a
/// list nested inside a list is refused by the parser instead of by a check.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawPrimaryAddress {
    backend: Option<String>,
    transit_key: Option<String>,
    transit_mount: Option<String>,
    ssh_agent: Option<String>,
    fallbacks: Option<Vec<RawAddress>>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RawAddress {
    backend: Option<String>,
    transit_key: Option<String>,
    transit_mount: Option<String>,
    ssh_agent: Option<String>,
}

impl From<&RawPrimaryAddress> for RawAddress {
    fn from(primary: &RawPrimaryAddress) -> Self {
        Self {
            backend: primary.backend.clone(),
            transit_key: primary.transit_key.clone(),
            transit_mount: primary.transit_mount.clone(),
            ssh_agent: primary.ssh_agent.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignerConfigError {
    #[error("`[git.ssh.signers.{signer}]`: `{backend}` is not a declared secrets backend; declared: {declared}")]
    UndeclaredBackend {
        signer: SignerName,
        backend: String,
        declared: String,
    },

    #[error(
        "`[git.ssh.signers.{signer}]`: backend `{backend}` is of kind `{kind}`, but a Transit key needs an `openbao` backend"
    )]
    NotOpenbao {
        signer: SignerName,
        backend: String,
        kind: BackendKind,
    },

    #[error("`[git.ssh.signers.{signer}]`: an address must be either `{{ backend, transit_key }}` or `{{ ssh_agent }}`: {problem}")]
    Address { signer: SignerName, problem: String },

    #[error("`[git.ssh.signers.{signer}]`: `fallbacks` is empty; list an address or remove it")]
    EmptyFallbacks { signer: SignerName },

    #[error("`[git.ssh.signers.{signer}]`: `{field}` must be one or more segments of [A-Za-z0-9._-] joined by `/`, got `{value}`")]
    Segment {
        signer: SignerName,
        field: &'static str,
        value: String,
    },

    #[error("`[git.ssh.signers.{second}]` repeats the public key of `[git.ssh.signers.{first}]`; a key identifies exactly one signer")]
    DuplicateKey { first: SignerName, second: SignerName },
}

/// A Transit key name, which becomes a URL path segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitKeyName(String);

impl TransitKeyName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Transit mount, which may be nested (`team/transit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitMount(String);

impl TransitMount {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_segment(text: &str) -> bool {
    !text.is_empty()
        && text != "."
        && text != ".."
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// An OpenBao Transit key reached through a declared backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitAddress {
    pub backend: String,
    pub key: TransitKeyName,
    pub mount: TransitMount,
}

/// The socket an SSH agent listens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSocket(PathBuf);

impl AgentSocket {
    pub fn path(&self) -> &Path {
        &self.0
    }
}

/// Where one signature can come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignerAddress {
    Transit(TransitAddress),
    SshAgent(AgentSocket),
}

impl SignerAddress {
    /// How a person reads this address in a warning or an error.
    pub fn describe(&self) -> String {
        match self {
            Self::Transit(t) => format!(
                "Transit key `{}` (mount `{}`) on backend `{}`",
                t.key.as_str(),
                t.mount.as_str(),
                t.backend
            ),
            Self::SshAgent(socket) => format!("SSH agent at `{}`", socket.path().display()),
        }
    }

    /// The low-cardinality name of this address, for telemetry: the backend
    /// that serves a Transit key, or `ssh_agent`.
    pub fn label(&self) -> &str {
        match self {
            Self::Transit(t) => &t.backend,
            Self::SshAgent(_) => "ssh_agent",
        }
    }
}

/// One signer with every address checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    name: SignerName,
    public_key: PublicKey,
    primary: SignerAddress,
    fallbacks: Vec<SignerAddress>,
}

impl Signer {
    pub fn name(&self) -> &SignerName {
        &self.name
    }

    pub fn public_key(&self) -> &PublicKey {
        &self.public_key
    }

    /// The addresses in the order they are tried.
    pub fn addresses(&self) -> impl Iterator<Item = &SignerAddress> {
        std::iter::once(&self.primary).chain(&self.fallbacks)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", self.message())]
pub struct NoSignerForKeyError {
    declared: Vec<SignerName>,
}

impl NoSignerForKeyError {
    fn message(&self) -> String {
        if self.declared.is_empty() {
            return "no signer is declared; add a `[git.ssh.signers.<name>]` section whose `public_key` is the key git is signing with".to_string();
        }
        let names: Vec<&str> = self.declared.iter().map(SignerName::as_str).collect();
        format!(
            "no declared signer has the public key git is signing with; declared signers: {}",
            names.join(", ")
        )
    }
}

/// The declared signers, which no two of share a key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signers(Vec<Signer>);

impl Signers {
    /// The signer whose key this is. There is no default: signing with
    /// something other than the key git asked for would produce a signature
    /// that verifies as someone else.
    pub fn select(&self, key: &PublicKey) -> Result<&Signer, NoSignerForKeyError> {
        self.0
            .iter()
            .find(|signer| &signer.public_key == key)
            .ok_or_else(|| NoSignerForKeyError {
                declared: self.0.iter().map(|s| s.name.clone()).collect(),
            })
    }

    pub fn iter(&self) -> impl Iterator<Item = &Signer> {
        self.0.iter()
    }
}

impl GitSection {
    /// Every declared signer, checked against the declared secrets backends.
    ///
    /// Run at config load for the same reason as the mirrors: a signer that
    /// could never sign is a config error wherever the file is read.
    pub fn resolve(&self, secrets: &SecretsSection) -> Result<Signers, SignerConfigError> {
        let mut resolved: Vec<Signer> = Vec::new();
        for (name, raw) in &self.ssh.signers {
            if let Some(first) = resolved.iter().find(|s| s.public_key == raw.public_key) {
                return Err(SignerConfigError::DuplicateKey {
                    first: first.name.clone(),
                    second: name.clone(),
                });
            }
            let primary = resolve_address(name, secrets, &RawAddress::from(&raw.signer))?;
            let fallbacks = match &raw.signer.fallbacks {
                None => Vec::new(),
                Some(list) if list.is_empty() => {
                    return Err(SignerConfigError::EmptyFallbacks { signer: name.clone() })
                }
                Some(list) => list
                    .iter()
                    .map(|address| resolve_address(name, secrets, address))
                    .collect::<Result<_, _>>()?,
            };
            resolved.push(Signer {
                name: name.clone(),
                public_key: raw.public_key.clone(),
                primary,
                fallbacks,
            });
        }
        Ok(Signers(resolved))
    }
}

fn resolve_address(
    signer: &SignerName,
    secrets: &SecretsSection,
    raw: &RawAddress,
) -> Result<SignerAddress, SignerConfigError> {
    let problem = |problem: &str| SignerConfigError::Address {
        signer: signer.clone(),
        problem: problem.to_string(),
    };
    let transit_fields = [&raw.backend, &raw.transit_key, &raw.transit_mount];
    match (&raw.ssh_agent, transit_fields.iter().any(|f| f.is_some())) {
        (Some(_), true) => Err(problem("`ssh_agent` cannot be combined with Transit fields")),
        (None, false) => Err(problem("neither was given")),
        (Some(socket), false) => {
            if socket.trim().is_empty() {
                return Err(problem("`ssh_agent` is empty"));
            }
            Ok(SignerAddress::SshAgent(AgentSocket(expand_tilde(socket))))
        }
        (None, true) => {
            let backend = raw.backend.clone().ok_or_else(|| problem("`backend` is missing"))?;
            let key = raw
                .transit_key
                .clone()
                .ok_or_else(|| problem("`transit_key` is missing"))?;
            let mount = raw
                .transit_mount
                .clone()
                .unwrap_or_else(|| DEFAULT_TRANSIT_MOUNT.to_string());
            match secrets.kind_of(&backend) {
                None => {
                    return Err(SignerConfigError::UndeclaredBackend {
                        signer: signer.clone(),
                        backend,
                        declared: secrets.declared_names().join(", "),
                    })
                }
                Some(BackendKind::Openbao) => {}
                Some(kind) => {
                    return Err(SignerConfigError::NotOpenbao {
                        signer: signer.clone(),
                        backend,
                        kind,
                    })
                }
            }
            if !is_segment(&key) {
                return Err(SignerConfigError::Segment {
                    signer: signer.clone(),
                    field: "transit_key",
                    value: key,
                });
            }
            if !mount.split('/').all(is_segment) {
                return Err(SignerConfigError::Segment {
                    signer: signer.clone(),
                    field: "transit_mount",
                    value: mount,
                });
            }
            Ok(SignerAddress::Transit(TransitAddress {
                backend,
                key: TransitKeyName(key),
                mount: TransitMount(mount),
            }))
        }
    }
}
