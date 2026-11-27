//! One-way mirroring of a value from one backend into another.
//!
//! The source stays the source of truth: a mirror copies a value toward a
//! target `trg` can write to, and never reads the target back as an input.
//! That is also why a mirror is a declaration rather than a flag on
//! `trg secret put`. The pair is what is worth keeping, so that running it
//! again converges on the same state instead of depending on whoever typed
//! the pipeline last.
//!
//! Whether two values agree is decided by a [`Fingerprint`], never by
//! comparing or printing the values themselves.

mod constants;
mod openssh;
mod transit;
use constants::SOURCE_KEY;
use std::collections::HashMap;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::field::Empty;
use tracing::Instrument;

use super::telemetry::record_error;
use super::vars::{self, VarFetchError};
use super::{
    Backend, BackendConfig, BackendError, BackendKind, GcpProject, GcpSecretId, SecretAddress, SecretKey, SecretPath,
    SecretsError, SecretsSection,
};
use crate::config::signers::{TransitKeyName, TransitMount};
use crate::config::{
    AddressShapeError, FetchedSecrets, RawAddress, RawSecretVar, SecretRead, SecretVar, SecretVarError, VarSite,
};
use crate::git::ssh::transit_key::TransitKeyError;
use crate::secrets::openbao::WrapError;
use crate::telemetry::semconv::trg::SECRETS_BACKEND_NAME;
use openssh::{SourceKey, SourceKeyError};

/// `<byte length>:<sha256>` of a value with one trailing newline removed.
///
/// One newline comes off because a backend may add one on the way in, and a
/// value that differs only by that is the same value. The length leads so an
/// empty value can never be mistaken for an agreeing one: its digest is a
/// constant, but its length is not a length any real secret has.
#[derive(Clone, PartialEq, Eq)]
pub struct Fingerprint {
    length: usize,
    digest: [u8; 32],
}

impl Fingerprint {
    pub fn of(value: &str) -> Self {
        let value = value.strip_suffix('\n').unwrap_or(value);
        Self {
            length: value.len(),
            digest: Sha256::digest(value.as_bytes()).into(),
        }
    }

    /// The fingerprint of every byte, trailing newline included.
    ///
    /// For a backend that stores bytes as given, where a value ending in a
    /// newline (a PEM key) and the same value without it are two values, and
    /// treating them as one would report as in sync a target that is not.
    pub fn exact(value: &str) -> Self {
        Self {
            length: value.len(),
            digest: Sha256::digest(value.as_bytes()).into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.length, crate::agentskills::hex_encode(self.digest))
    }
}

/// Redacted to the length, because a digest of a weak secret is a way to
/// guess it and this type reaches `Debug` through the error types.
impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({} bytes)", self.length)
    }
}

/// The name a `[secrets.mirrors.<name>]` table is declared under.
///
/// Limited to what a TOML bare key holds, so a mirror can always be written
/// as `[secrets.mirrors.<name>]` without quoting and can be typed on the
/// command line as it is written in the file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "String")]
pub struct MirrorName(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MirrorNameError {
    #[error("a mirror name must not be empty")]
    Empty,

    #[error("mirror name `{0}` may hold only ASCII letters, digits, `-` and `_`")]
    Characters(String),
}

impl MirrorName {
    pub fn parse(raw: &str) -> Result<Self, MirrorNameError> {
        if raw.is_empty() {
            return Err(MirrorNameError::Empty);
        }
        if !raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
            return Err(MirrorNameError::Characters(raw.to_string()));
        }
        Ok(Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MirrorName {
    type Error = MirrorNameError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::parse(&raw)
    }
}

impl fmt::Display for MirrorName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One `[secrets.mirrors.<name>]` table as the file spelled it.
///
/// The source is the inline table a var is written as, so a value is
/// addressed here exactly as it is anywhere else in the config. A target is
/// that same table, or a Transit key addressed the way a signer addresses one.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    pub source: RawSecretVar,
    pub targets: Vec<RawMirrorTarget>,
}

/// One entry of `targets` as the file spelled it: either a var (`backend`
/// with `path` and `key`, `ref`, or `secret`), or `backend` with `transit_key`
/// and optionally `transit_mount`.
///
/// Both spellings share one table because the fields present tell which one
/// applies, the way they do for a signer address. The choice is made while
/// parsing, so a Transit target that also carries an address, or a mount with
/// no key, cannot be built.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(try_from = "RawMirrorTargetFields")]
pub enum RawMirrorTarget {
    Var(RawSecretVar),
    Transit(RawTransitTarget),
}

/// A Transit target as the file spelled it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawTransitTarget {
    pub backend: String,
    pub transit_key: String,
    pub transit_mount: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawMirrorTargetFields {
    backend: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default, rename = "ref")]
    reference: Option<String>,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    transit_key: Option<String>,
    #[serde(default)]
    transit_mount: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RawMirrorTargetError {
    #[error(
        "a Transit target is `{{ backend, transit_key }}` with an optional `transit_mount`, and cannot also carry `path`, `key`, `ref` or `secret`"
    )]
    MixedTarget,

    #[error("a Transit target needs `transit_key`")]
    MissingTransitKey,

    #[error(transparent)]
    Address(#[from] AddressShapeError),
}

impl TryFrom<RawMirrorTargetFields> for RawMirrorTarget {
    type Error = RawMirrorTargetError;

    fn try_from(fields: RawMirrorTargetFields) -> Result<Self, Self::Error> {
        let has_address =
            fields.path.is_some() || fields.key.is_some() || fields.reference.is_some() || fields.secret.is_some();
        match (fields.transit_key, fields.transit_mount) {
            (None, None) => {
                let address = RawAddress::from_parts(
                    &fields.backend,
                    fields.path,
                    fields.key,
                    fields.reference,
                    fields.secret,
                )?;
                Ok(Self::Var(RawSecretVar::new(fields.backend, address)))
            }
            _ if has_address => Err(RawMirrorTargetError::MixedTarget),
            (None, Some(_)) => Err(RawMirrorTargetError::MissingTransitKey),
            (Some(transit_key), transit_mount) => Ok(Self::Transit(RawTransitTarget {
                backend: fields.backend,
                transit_key,
                transit_mount,
            })),
        }
    }
}

/// A value written into a backend that stores values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueTarget {
    var: SecretVar,
    path: SecretPath,
    key: SecretKey,
}

/// A Secret Manager secret that receives a new version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretTarget {
    var: SecretVar,
    /// The project the backend writes into. Two backends may name one
    /// project under different accounts, so the backend name does not say
    /// which secret is written; this does.
    project: GcpProject,
    secret: GcpSecretId,
}

/// An OpenBao Transit key a private SSH key is imported into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransitTarget {
    backend: String,
    /// Which OpenBao the backend reaches: its `addr` as `OpenBaoBackend::new`
    /// normalizes it, so two backends naming one server share a value. A
    /// backend whose `addr` cannot be read without the environment it is
    /// declared against falls back to `backend:<name>`, which no address
    /// spells.
    endpoint: String,
    key: TransitKeyName,
    mount: TransitMount,
}

/// One place a mirror writes, already checked to be one `trg` can write to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorTarget {
    Value(ValueTarget),
    Secret(SecretTarget),
    Transit(TransitTarget),
}

impl MirrorTarget {
    pub fn backend(&self) -> &str {
        match self {
            Self::Value(t) => t.var.backend(),
            Self::Secret(t) => t.var.backend(),
            Self::Transit(t) => &t.backend,
        }
    }

    /// The address, for machine-readable output.
    pub fn address(&self) -> String {
        match self {
            Self::Value(t) => t.var.to_string(),
            Self::Secret(t) => t.var.to_string(),
            Self::Transit(t) => format!(
                "Transit key `{}` (mount `{}`) from backend `{}`",
                t.key.as_str(),
                t.mount.as_str(),
                t.backend
            ),
        }
    }

    /// `<path>#<key> (<backend>)`, for a line of output that already says
    /// which mirror it is about.
    pub fn describe(&self) -> String {
        match self {
            Self::Value(t) => format!("{}#{} ({})", t.path, t.key, t.var.backend()),
            Self::Secret(t) => format!("{} ({})", t.secret, t.var.backend()),
            Self::Transit(t) => format!("{}/keys/{} ({})", t.mount.as_str(), t.key.as_str(), t.backend),
        }
    }
}

/// A mirror whose sides have been checked against the declared backends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    name: MirrorName,
    source: SecretVar,
    targets: Vec<MirrorTarget>,
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorConfigError {
    #[error("{0}")]
    Var(#[from] SecretVarError),

    #[error("`[secrets.mirrors.{name}]`: `targets` must name at least one target")]
    NoTargets { name: MirrorName },

    #[error(
        "`[secrets.mirrors.{name}]`: target backend `{backend}` is of kind `{kind}`, which `trg` only reads from; mirror into a `keychain`, `openbao` or `gcp_secret_manager` backend"
    )]
    TargetNotWritable {
        name: MirrorName,
        backend: String,
        kind: BackendKind,
    },

    #[error("`[secrets.mirrors.{name}]`: `{backend}` is not a declared secrets backend; declared: {declared}")]
    UndeclaredTransitBackend {
        name: MirrorName,
        backend: String,
        declared: String,
    },

    #[error(
        "`[secrets.mirrors.{name}]`: backend `{backend}` is of kind `{kind}`, but a Transit key needs an `openbao` backend"
    )]
    TransitNotOpenbao {
        name: MirrorName,
        backend: String,
        kind: BackendKind,
    },

    #[error(
        "`[secrets.mirrors.{name}]`: `{field}` must be made of [A-Za-z0-9._-] (`transit_mount` may nest with `/`), got `{value}`"
    )]
    TransitSegment {
        name: MirrorName,
        field: &'static str,
        value: String,
    },

    #[error("`[secrets.mirrors.{name}]`: the source and a target are the same address, {address}")]
    SameAddress { name: MirrorName, address: String },

    #[error(
        "`[secrets.mirrors.{name}]`: target key `{SOURCE_KEY}` is reserved for the reference to where the value came from"
    )]
    ReservedKey { name: MirrorName },

    #[error("`[secrets.mirrors.{name}]`: target {target} is listed more than once")]
    RepeatedTarget { name: MirrorName, target: String },

    #[error("`[secrets.mirrors.{name}]`: target {target} is already a target of `[secrets.mirrors.{first}]`")]
    DuplicateTarget {
        name: MirrorName,
        first: MirrorName,
        target: String,
    },

    #[error(
        "`[secrets.mirrors.{name}]`: path `{path}` in backend `{backend}` is also written by `[secrets.mirrors.{first}]` from a different source; the `{SOURCE_KEY}` key there records one origin, so mirrors sharing a path must share a source"
    )]
    ConflictingSource {
        name: MirrorName,
        first: MirrorName,
        backend: String,
        path: String,
    },
}

impl Mirror {
    pub fn name(&self) -> &MirrorName {
        &self.name
    }

    pub fn source(&self) -> &SecretVar {
        &self.source
    }

    pub fn targets(&self) -> &[MirrorTarget] {
        &self.targets
    }

    /// Where the value came from, as written into each target's `source` key.
    ///
    /// A 1Password reference already is a portable pointer. The other kinds
    /// are addressed by backend name, path and key, so the name is part of it.
    fn source_reference(&self) -> String {
        match self.source.address() {
            SecretAddress::OnePassword(r) => r.to_string(),
            SecretAddress::Keychain(r) => format!("{}:{}#{}", self.source.backend(), r.path(), r.key()),
            SecretAddress::Openbao(r) => format!("{}:{}#{}", self.source.backend(), r.path(), r.key()),
            SecretAddress::GcpSecretManager(r) => format!("{}:{}", self.source.backend(), r.secret()),
        }
    }
}

/// Resolve every declared mirror, in name order, so output and errors do not
/// depend on how the file happened to be arranged.
pub fn resolve_all(secrets: &SecretsSection) -> Result<Vec<Mirror>, MirrorConfigError> {
    let mut out = Vec::with_capacity(secrets.mirrors.len());
    let mut claimed: HashMap<SecretVar, MirrorName> = HashMap::new();
    let mut claimed_transit: HashMap<(String, String, String), MirrorName> = HashMap::new();
    let mut claimed_secrets: HashMap<(GcpProject, GcpSecretId), MirrorName> = HashMap::new();
    let mut paths: HashMap<(String, SecretPath), (MirrorName, SecretVar)> = HashMap::new();

    for (name, config) in &secrets.mirrors {
        let mirror = resolve_one(name, config, secrets)?;
        for target in &mirror.targets {
            let target = match target {
                MirrorTarget::Value(target) => target,
                MirrorTarget::Secret(target) => {
                    let at = (target.project.clone(), target.secret.clone());
                    match claimed_secrets.get(&at) {
                        Some(first) if first == name => {
                            return Err(MirrorConfigError::RepeatedTarget {
                                name: name.clone(),
                                target: target.var.to_string(),
                            })
                        }
                        Some(first) => {
                            return Err(MirrorConfigError::DuplicateTarget {
                                name: name.clone(),
                                first: first.clone(),
                                target: target.var.to_string(),
                            })
                        }
                        None => {
                            claimed_secrets.insert(at, name.clone());
                        }
                    }
                    match claimed.get(&target.var) {
                        Some(first) if first == name => {
                            return Err(MirrorConfigError::RepeatedTarget {
                                name: name.clone(),
                                target: target.var.to_string(),
                            })
                        }
                        Some(first) => {
                            return Err(MirrorConfigError::DuplicateTarget {
                                name: name.clone(),
                                first: first.clone(),
                                target: target.var.to_string(),
                            })
                        }
                        None => {
                            claimed.insert(target.var.clone(), name.clone());
                        }
                    }
                    continue;
                }
                MirrorTarget::Transit(target) => {
                    let at = (
                        target.endpoint.clone(),
                        target.mount.as_str().to_string(),
                        target.key.as_str().to_string(),
                    );
                    let described = MirrorTarget::Transit(target.clone()).address();
                    match claimed_transit.get(&at) {
                        Some(first) if first == name => {
                            return Err(MirrorConfigError::RepeatedTarget {
                                name: name.clone(),
                                target: described,
                            })
                        }
                        Some(first) => {
                            return Err(MirrorConfigError::DuplicateTarget {
                                name: name.clone(),
                                first: first.clone(),
                                target: described,
                            })
                        }
                        None => {
                            claimed_transit.insert(at, name.clone());
                        }
                    }
                    continue;
                }
            };
            match claimed.get(&target.var) {
                Some(first) if first == name => {
                    return Err(MirrorConfigError::RepeatedTarget {
                        name: name.clone(),
                        target: target.var.to_string(),
                    })
                }
                Some(first) => {
                    return Err(MirrorConfigError::DuplicateTarget {
                        name: name.clone(),
                        first: first.clone(),
                        target: target.var.to_string(),
                    })
                }
                None => {
                    claimed.insert(target.var.clone(), name.clone());
                }
            }
            let at = (target.var.backend().to_string(), target.path.clone());
            match paths.get(&at) {
                Some((first, source)) if source != &mirror.source => {
                    return Err(MirrorConfigError::ConflictingSource {
                        name: name.clone(),
                        first: first.clone(),
                        backend: at.0,
                        path: at.1.to_string(),
                    })
                }
                Some(_) => {}
                None => {
                    paths.insert(at, (name.clone(), mirror.source.clone()));
                }
            }
        }
        out.push(mirror);
    }

    Ok(out)
}

fn resolve_one(
    name: &MirrorName,
    config: &MirrorConfig,
    secrets: &SecretsSection,
) -> Result<Mirror, MirrorConfigError> {
    if config.targets.is_empty() {
        return Err(MirrorConfigError::NoTargets { name: name.clone() });
    }
    let source = SecretVar::resolve(&config.source, secrets, VarSite::mirror(name.as_str(), "source"))?;

    let mut targets = Vec::with_capacity(config.targets.len());
    for (offset, raw) in config.targets.iter().enumerate() {
        let side = format!("targets[{offset}]");
        let raw = match raw {
            RawMirrorTarget::Transit(transit) => {
                targets.push(MirrorTarget::Transit(resolve_transit(name, transit, secrets)?));
                continue;
            }
            RawMirrorTarget::Var(raw) => raw,
        };
        let var = SecretVar::resolve(raw, secrets, VarSite::mirror(name.as_str(), &side))?;

        let kind = var.address().backend_kind();
        if !kind.is_writable() {
            return Err(MirrorConfigError::TargetNotWritable {
                name: name.clone(),
                backend: var.backend().to_string(),
                kind,
            });
        }
        if source == var {
            return Err(MirrorConfigError::SameAddress {
                name: name.clone(),
                address: source.to_string(),
            });
        }
        if let SecretAddress::GcpSecretManager(r) = var.address() {
            // A secret holds one value and records no origin beside it, so
            // there is no `source` key to reserve and no path to share.
            let secret = r.secret().clone();
            let Some(BackendConfig::GcpSecretManager(gcp)) = secrets.backends.get(var.backend()) else {
                unreachable!("a gcp address is only resolved against a gcp backend")
            };
            let project = gcp.project.clone();
            targets.push(MirrorTarget::Secret(SecretTarget { var, project, secret }));
            continue;
        }
        let Some((path, key)) = var.address().path_key() else {
            unreachable!("a writable kind is addressed by path and key")
        };
        if key.as_str() == SOURCE_KEY {
            return Err(MirrorConfigError::ReservedKey { name: name.clone() });
        }
        let (path, key) = (path.clone(), key.clone());
        targets.push(MirrorTarget::Value(ValueTarget { var, path, key }));
    }

    Ok(Mirror {
        name: name.clone(),
        source,
        targets,
    })
}

fn resolve_transit(
    name: &MirrorName,
    raw: &RawTransitTarget,
    secrets: &SecretsSection,
) -> Result<TransitTarget, MirrorConfigError> {
    let key = raw.transit_key.as_str();
    match secrets.kind_of(&raw.backend) {
        None => {
            return Err(MirrorConfigError::UndeclaredTransitBackend {
                name: name.clone(),
                backend: raw.backend.clone(),
                declared: secrets.declared_names().join(", "),
            })
        }
        Some(BackendKind::Openbao) => {}
        Some(kind) => {
            return Err(MirrorConfigError::TransitNotOpenbao {
                name: name.clone(),
                backend: raw.backend.clone(),
                kind,
            })
        }
    }
    let segment = |field: &'static str, value: &str| MirrorConfigError::TransitSegment {
        name: name.clone(),
        field,
        value: value.to_string(),
    };
    let key = TransitKeyName::parse(key).ok_or_else(|| segment("transit_key", key))?;
    let mount = match raw.transit_mount.as_deref() {
        None => TransitMount::default(),
        Some(mount) => TransitMount::parse(mount).ok_or_else(|| segment("transit_mount", mount))?,
    };
    let endpoint = match secrets.backends.get(&raw.backend) {
        Some(BackendConfig::Openbao(config)) => config
            .addr
            .resolve_bootstrap(&FetchedSecrets::new())
            .map(|addr| canonical_endpoint(&addr))
            .unwrap_or_else(|_| format!("backend:{}", raw.backend)),
        _ => format!("backend:{}", raw.backend),
    };
    Ok(TransitTarget {
        backend: raw.backend.clone(),
        endpoint,
        key,
        mount,
    })
}

/// The server an OpenBao address reaches, spelled one way: the host
/// lowercased and a default port dropped, as URL parsing does, and no trailing
/// `/`. An address that does not parse is compared as written.
fn canonical_endpoint(addr: &str) -> String {
    reqwest::Url::parse(addr)
        .map(|url| url.as_str().trim_end_matches('/').to_string())
        .unwrap_or_else(|_| addr.trim_end_matches('/').to_string())
}

/// What a mirror did, or in a dry run what it would do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorOutcome {
    UpToDate,
    WouldCreate,
    WouldUpdate,
    Created,
    Updated,
}

impl MirrorOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::UpToDate => "up to date",
            Self::WouldCreate => "would create",
            Self::WouldUpdate => "would update",
            Self::Created => "created",
            Self::Updated => "updated",
        }
    }

    /// Whether the target held something other than what the source holds,
    /// whether or not this run changed it.
    pub fn is_drift(self) -> bool {
        !matches!(self, Self::UpToDate)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorError {
    #[error("could not read the source: {0}")]
    Source(#[from] VarFetchError),

    #[error("refusing to mirror an empty source value; a source that was emptied or never filled is the usual reason for one")]
    EmptySource,

    #[error("target backend `{name}`: {cause}")]
    TargetBackend {
        name: String,
        #[source]
        cause: Box<BackendError>,
    },

    #[error("could not read the target: {0}")]
    TargetRead(#[source] SecretsError),

    #[error("could not write the target: {0}")]
    TargetWrite(#[source] SecretsError),

    #[error("the target did not read back what was written")]
    ReadbackMismatch,

    #[error("the source is not a key Transit can take: {0}")]
    SourceKey(#[source] SourceKeyError),

    #[error("target backend is of kind `{kind}`, but a Transit key needs an `openbao` backend (`{backend}`)")]
    TransitNeedsOpenbao { backend: String, kind: &'static str },

    #[error("the Transit key's public key could not be read: {0}")]
    TransitKey(#[source] TransitKeyError),

    #[error(
        "the Transit key holds a `{held}` key but the source is `{wanted}`, and a Transit key cannot change type; mirror into a new `transit_key` or delete this one"
    )]
    TransitTypeChange { held: String, wanted: &'static str },

    #[error(
        "the Transit key {}, so a new version of it would be a private key that can be taken back out of Transit; mirror into a new `transit_key` name instead",
        exposure(*exportable, *allow_plaintext_backup)
    )]
    TransitKeyExposed {
        exportable: bool,
        allow_plaintext_backup: bool,
    },

    #[error("the key could not be wrapped for Transit: {0}")]
    Wrap(#[source] WrapError),
}

fn exposure(exportable: bool, allow_plaintext_backup: bool) -> &'static str {
    match (exportable, allow_plaintext_backup) {
        (true, true) => "is exportable and allows plaintext backup",
        (true, false) => "is exportable",
        _ => "allows plaintext backup",
    }
}

impl MirrorError {
    /// A low-cardinality name for this failure, for `error.type`.
    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Source(e) => e.error_type(),
            Self::EmptySource => "empty_source",
            Self::TargetBackend { .. } => "backend_config",
            Self::TargetRead(e) | Self::TargetWrite(e) => e.error_type(),
            Self::ReadbackMismatch => "readback_mismatch",
            Self::SourceKey(_) => "source_key",
            Self::TransitNeedsOpenbao { .. } => "backend_config",
            Self::TransitKey(_) => "transit_key",
            Self::TransitTypeChange { .. } => "transit_type_change",
            Self::TransitKeyExposed { .. } => "transit_key_exposed",
            Self::Wrap(_) => "wrap",
        }
    }
}

/// What each target of one mirror did, in the order the mirror lists them.
pub type TargetOutcomes = Vec<Result<MirrorOutcome, MirrorError>>;

/// Bring every target of a mirror in line with its source, or report how it
/// would.
///
/// The source is read once for all of them, and an empty or unreadable source
/// is the one failure that stops every target. After that each target stands
/// alone, so one backend being down does not hide that the others are in
/// sync or still drifted.
///
/// A dry run reads both sides and writes nothing. An applied run writes only
/// when the fingerprints differ, then reads the target back and fails if what
/// it holds is not what was written, since a write the backend acknowledged
/// but did not keep must not be reported as mirrored.
pub async fn mirror<F>(resolve: &F, mirror: &Mirror, apply: bool) -> Result<TargetOutcomes, MirrorError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let source = SecretRead::from(mirror.source.clone());
    let fetched = vars::fetch_with(resolve, std::slice::from_ref(&source)).await?;
    let Some(value) = fetched.get(&source) else {
        unreachable!("a fetch answers for every var it was asked about")
    };
    let wanted = Fingerprint::of(value.expose_secret());
    let wanted_exact = Fingerprint::exact(value.expose_secret());
    if wanted.is_empty() {
        return Err(MirrorError::EmptySource);
    }
    let reference = mirror.source_reference();
    let key = mirror
        .targets
        .iter()
        .any(|target| matches!(target, MirrorTarget::Transit(_)))
        .then(|| openssh::parse(value.expose_secret()));

    let mut outcomes = Vec::with_capacity(mirror.targets.len());
    for target in &mirror.targets {
        let span = tracing::info_span!(
            "mirror target",
            { SECRETS_BACKEND_NAME } = target.backend(),
            "error.type" = Empty,
            "otel.status_code" = Empty,
        );
        let outcome = match (target, &key) {
            (MirrorTarget::Value(target), _) => {
                sync_target(resolve, target, value, &wanted, &reference, apply)
                    .instrument(span.clone())
                    .await
            }
            (MirrorTarget::Secret(target), _) => {
                sync_secret(resolve, target, value, &wanted_exact, apply)
                    .instrument(span.clone())
                    .await
            }
            (MirrorTarget::Transit(target), Some(key)) => {
                sync_transit(resolve, target, key, apply).instrument(span.clone()).await
            }
            (MirrorTarget::Transit(_), None) => unreachable!("a key is parsed whenever a Transit target exists"),
        };
        if let Err(e) = &outcome {
            record_error(&span, e.error_type());
        }
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

async fn sync_transit<F>(
    resolve: &F,
    target: &TransitTarget,
    key: &Result<SourceKey, SourceKeyError>,
    apply: bool,
) -> Result<MirrorOutcome, MirrorError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let backend = resolve(&target.backend).map_err(|cause| MirrorError::TargetBackend {
        name: target.backend.clone(),
        cause: Box::new(cause),
    })?;
    transit::sync(&backend, target, key, apply).await
}

async fn sync_target<F>(
    resolve: &F,
    target: &ValueTarget,
    value: &SecretString,
    wanted: &Fingerprint,
    reference: &str,
    apply: bool,
) -> Result<MirrorOutcome, MirrorError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let backend = resolve(target.var.backend()).map_err(|cause| MirrorError::TargetBackend {
        name: target.var.backend().to_string(),
        cause: Box::new(cause),
    })?;
    let existing = backend.get(&target.path).await.map_err(MirrorError::TargetRead)?;
    let held = existing
        .as_ref()
        .and_then(|map| map.get(&target.key))
        .map(|held| Fingerprint::of(held.expose_secret()));

    let outcome = match &held {
        None => MirrorOutcome::WouldCreate,
        Some(held) if held == wanted => return Ok(MirrorOutcome::UpToDate),
        Some(_) => MirrorOutcome::WouldUpdate,
    };
    if !apply {
        return Ok(outcome);
    }

    let mut map = existing.unwrap_or_default();
    map.insert(target.key.clone(), value.clone());
    let Ok(source_key) = SecretKey::parse(SOURCE_KEY) else {
        unreachable!("`{SOURCE_KEY}` is a valid key")
    };
    map.insert(source_key, SecretString::from(reference.to_string()));
    backend
        .set(&target.path, &map)
        .await
        .map_err(MirrorError::TargetWrite)?;

    verify_readback(&backend, target, wanted).await?;

    Ok(match outcome {
        MirrorOutcome::WouldCreate => MirrorOutcome::Created,
        _ => MirrorOutcome::Updated,
    })
}

/// Add a version to a Secret Manager secret when its latest one differs.
///
/// Compared byte for byte, since the store keeps bytes as given and a PEM
/// key's final newline is part of it. The secret is never created: one that
/// is missing is an error even in a dry run, because applying could not fix it.
async fn sync_secret<F>(
    resolve: &F,
    target: &SecretTarget,
    value: &SecretString,
    wanted: &Fingerprint,
    apply: bool,
) -> Result<MirrorOutcome, MirrorError>
where
    F: Fn(&str) -> Result<Backend, BackendError>,
{
    let backend = resolve(target.var.backend()).map_err(|cause| MirrorError::TargetBackend {
        name: target.var.backend().to_string(),
        cause: Box::new(cause),
    })?;
    let held = backend
        .get_secret(&target.secret)
        .await
        .map_err(MirrorError::TargetRead)?;
    let outcome = match &held {
        Some(held) if Fingerprint::exact(held.expose_secret()) == *wanted => return Ok(MirrorOutcome::UpToDate),
        Some(_) => MirrorOutcome::WouldUpdate,
        None => {
            // Nothing readable is either a secret with no version yet or no
            // secret at all, and only the first is something to fill.
            if !backend
                .secret_exists(&target.secret)
                .await
                .map_err(MirrorError::TargetRead)?
            {
                return Err(MirrorError::TargetRead(backend.missing_container(&target.secret)));
            }
            MirrorOutcome::WouldCreate
        }
    };
    if !apply {
        return Ok(outcome);
    }

    let created = backend
        .add_secret_version(&target.secret, value)
        .await
        .map_err(MirrorError::TargetWrite)?
        .ok_or_else(|| {
            MirrorError::TargetRead(SecretsError::Unavailable(
                "gcloud did not say which version it added, so the write could not be read back".to_string(),
            ))
        })?;

    // The version that was just created, never `latest`: reading a version by
    // number right after adding it is strongly consistent, and the alias is
    // not, so it can still answer with the previous value for a while.
    let back = backend
        .get_secret_version(&target.secret, &created)
        .await
        .map_err(MirrorError::TargetRead)?;
    match back {
        Some(back) if Fingerprint::exact(back.expose_secret()) == *wanted => {}
        _ => return Err(MirrorError::ReadbackMismatch),
    }

    Ok(match outcome {
        MirrorOutcome::WouldCreate => MirrorOutcome::Created,
        _ => MirrorOutcome::Updated,
    })
}

async fn verify_readback(backend: &Backend, target: &ValueTarget, wanted: &Fingerprint) -> Result<(), MirrorError> {
    let back = backend
        .get(&target.path)
        .await
        .map_err(MirrorError::TargetRead)?
        .and_then(|map| map.get(&target.key).cloned());
    match back {
        Some(back) if Fingerprint::of(back.expose_secret()) == *wanted => Ok(()),
        _ => Err(MirrorError::ReadbackMismatch),
    }
}

#[cfg(test)]
#[path = "mirror_tests.rs"]
mod tests;
