//! `[secrets]` deserialisation and resolution into a [`Registry`].
//!
//! Backends are **addressed, never searched**. A server names exactly one
//! backend and that is the one used; there is no ordered list, no probing, and
//! no fallback to a second backend when the first fails.
//!
//! The one exception is a config *value* that opts in with `fallbacks = [...]`,
//! a list of full addresses read in order after its own. It is a property of that one
//! read, spelled out where the value is declared, and it exists only on values
//! `trg` reads. What `trg` writes, the OAuth credentials a server's `secrets`
//! names, is a bare backend name and has no such form. See
//! [`crate::secrets::vars`].
//!
//! Backend declarations resolve through [`RawVarSource`] only, so a backend can
//! read `{ env = "BAO_ADDR" }` but cannot read `{ secret = "..." }`. That keeps
//! bootstrap acyclic: nothing needed to reach a secret store may itself live in
//! a secret store.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::Deserialize;

use super::keychain::{KeychainBackend, DEFAULT_SERVICE};
use super::mirror::{self, Mirror, MirrorConfig, MirrorConfigError, MirrorName};
use super::named::{NamedSecretConfig, NamedSecretError, SecretName};
use super::onepassword::{OnePasswordBackend, OpAccount};
use super::openbao::{
    expand_tilde, OpenBaoBackend, OpenBaoBuildError, OpenBaoSettings, TokenSource, DEFAULT_TIMEOUT_MS,
};
use super::{Backend, BackendKind};
use crate::config::{FetchedSecrets, RawVarSource, SecretRead, SecretVarError, VarResolveError, VarSite};

/// The `[secrets]` table.
#[derive(Debug, Default, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SecretsSection {
    #[serde(default)]
    pub backends: HashMap<String, BackendConfig>,
    #[serde(default)]
    pub mirrors: BTreeMap<MirrorName, MirrorConfig>,
    #[serde(default)]
    pub vars: BTreeMap<SecretName, NamedSecretConfig>,
}

impl SecretsSection {
    /// The kind a name was declared with, for the second config phase to
    /// decide how a var naming it must be addressed.
    pub fn kind_of(&self, name: &str) -> Option<BackendKind> {
        self.backends.get(name).map(BackendConfig::kind)
    }

    /// Every declared name, sorted, so an error about an undeclared one can
    /// show what was on offer instead.
    pub fn declared_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.backends.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// The declared mirrors, checked against the declared backends.
    ///
    /// Run at config load as well as by `trg secret mirror`, so a mirror that
    /// could never apply is a config error wherever the file is read, not a
    /// surprise the first time someone mirrors.
    pub fn resolve_mirrors(&self) -> Result<Vec<Mirror>, MirrorConfigError> {
        mirror::resolve_all(self)
    }

    /// Every declared named secret, in name order, against the declared
    /// backends.
    ///
    /// Run at config load for the same reason as the mirrors: a secret that
    /// could never be read is a config error wherever the file is read.
    pub fn resolve_named_all(&self) -> Result<BTreeMap<SecretName, SecretRead>, SecretVarError> {
        self.vars
            .iter()
            .map(|(name, config)| self.resolve_named_one(name, config).map(|read| (name.clone(), read)))
            .collect()
    }

    /// The load-time check alone, for callers that do not want the reads.
    pub fn check_named(&self) -> Result<(), SecretVarError> {
        self.resolve_named_all().map(drop)
    }

    /// The one secret a caller asked for by name.
    pub fn resolve_named(&self, name: &SecretName) -> Result<SecretRead, NamedSecretError> {
        let Some(config) = self.vars.get(name) else {
            return Err(if self.vars.is_empty() {
                NamedSecretError::NoneDeclared { name: name.clone() }
            } else {
                NamedSecretError::Unknown {
                    name: name.clone(),
                    declared: self.vars.keys().map(SecretName::as_str).collect::<Vec<_>>().join(", "),
                }
            });
        };
        Ok(self.resolve_named_one(name, config)?)
    }

    fn resolve_named_one(&self, name: &SecretName, config: &NamedSecretConfig) -> Result<SecretRead, SecretVarError> {
        let resolved = config
            .clone()
            .into_source()
            .into_resolved(self, || VarSite::named_secret(name.as_str()))?;
        Ok(resolved.secret_read().expect("a secret table always reads a secret"))
    }
}

/// One `[secrets.backends.<name>]` entry.
///
/// `kind` is a closed enum, so an unrecognised value fails at parse time with
/// serde naming the kinds that do exist.
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackendConfig {
    Keychain(KeychainConfig),
    Openbao(Box<OpenbaoConfig>),
    /// Renamed because the product is spelled `1Password`, so the Rust
    /// identifier follows it; the `kind = "onepassword"` a config file writes
    /// is unchanged.
    #[serde(rename = "onepassword")]
    OnePassword(OnePasswordConfig),
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct KeychainConfig {
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OnePasswordConfig {
    /// Which signed-in `op` account this backend reads from.
    ///
    /// Required, because a vault name is unique only within an account and
    /// `op`'s own default is machine-local state rather than something this
    /// config says. See the module docs on [`super::onepassword`].
    pub account: OpAccount,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct OpenbaoConfig {
    pub addr: RawVarSource,
    pub mount: String,
    pub path_prefix: String,
    /// Required, unlike every other segment field.
    ///
    /// A shared instance is the reason to reach for this backend at all, and
    /// on one the subtree a credential lands in is the ACL boundary. Letting
    /// it default means the first person to log in quietly takes `trg/mcp/*`
    /// for everyone, and the second silently rotates their refresh token. That
    /// is not a failure anyone sees until a provider treats the reuse as replay
    /// and revokes the grant, so it is refused while the config is still in
    /// view instead.
    pub owner: String,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub token_file: Option<String>,
    #[serde(default)]
    pub token: Option<RawVarSource>,
    #[serde(default)]
    pub ca_cert_file: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl BackendConfig {
    pub fn kind(&self) -> BackendKind {
        match self {
            Self::Keychain(_) => BackendKind::Keychain,
            Self::Openbao(_) => BackendKind::Openbao,
            Self::OnePassword(_) => BackendKind::OnePassword,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("`{name}` is not a declared secrets backend; declared: {declared}")]
    Unknown { name: String, declared: String },

    #[error("`{name}` is not a declared secrets backend; add a `[secrets.backends.{name}]` section")]
    NoneDeclared { name: String },

    #[error("`[secrets.backends.{name}]`: declare exactly one of `token_file` or `token`, not {found}")]
    TokenSource { name: String, found: &'static str },

    #[error("`[secrets.backends.{name}]`: `{field}` could not be resolved: {cause}")]
    Resolve {
        name: String,
        field: &'static str,
        #[source]
        cause: Box<VarResolveError>,
    },

    /// A backend reaching a backend to learn how to reach a backend.
    ///
    /// `addr` and `token` are what a read needs before it can happen, so a
    /// `{ backend = ... }` there would have to be resolved by the very backend
    /// it is describing. Refused while the config is being read, rather than
    /// left to fail as a cycle at the first use.
    #[error("`[secrets.backends.{name}]`: `{field}` cannot come from a secrets backend, because it is what reaching one requires")]
    SecretVar { name: String, field: &'static str },

    #[error("`[secrets.backends.{name}]`: {cause}")]
    Build {
        name: String,
        #[source]
        cause: OpenBaoBuildError,
    },
}

impl BackendError {
    /// Whether a backend that could not be built may be skipped by an opted-in
    /// fallback list.
    ///
    /// A failure qualifies when it is a property of the machine: a field that
    /// failed to resolve, such as `{ env = "BAO_ADDR" }` on a machine that does
    /// not set it, or a `ca_cert_file` this host cannot open. The next address
    /// is what the list is for. Every other variant is the config being wrong,
    /// and is wrong on every machine, so skipping it would hide a mistake
    /// behind a working fallback.
    pub fn allows_fallback(&self) -> bool {
        match self {
            Self::Resolve { .. } => true,
            Self::Unknown { .. } | Self::NoneDeclared { .. } | Self::TokenSource { .. } | Self::SecretVar { .. } => {
                false
            }
            Self::Build { cause, .. } => cause.is_machine_local(),
        }
    }
}

/// The declared backends, resolved by name on demand.
///
/// Resolution is lazy on purpose: a machine that has `BAO_ADDR` unset should
/// still be able to use an unrelated keychain-backed server, so declaring a
/// backend costs nothing until something addresses it. It is still built once
/// per process, never per operation.
#[derive(Debug, Default, Clone)]
pub struct Registry {
    declared: HashMap<String, BackendConfig>,
    mirrors: BTreeMap<MirrorName, MirrorConfig>,
    named: BTreeMap<SecretName, NamedSecretConfig>,
}

impl Registry {
    pub fn new(section: SecretsSection) -> Self {
        Self {
            declared: section.backends,
            mirrors: section.mirrors,
            named: section.vars,
        }
    }

    fn section(&self) -> SecretsSection {
        SecretsSection {
            backends: self.declared.clone(),
            mirrors: self.mirrors.clone(),
            vars: self.named.clone(),
        }
    }

    /// The declared mirrors, checked against the declared backends.
    pub fn mirrors(&self) -> Result<Vec<Mirror>, MirrorConfigError> {
        self.section().resolve_mirrors()
    }

    /// What a `[secrets.vars.<name>]` table asks to be read.
    pub fn named_read(&self, name: &SecretName) -> Result<SecretRead, NamedSecretError> {
        self.section().resolve_named(name)
    }

    pub fn is_empty(&self) -> bool {
        self.declared.is_empty()
    }

    /// The `kind` a declared backend was given, whether or not it builds.
    pub fn kind_of(&self, name: &str) -> Option<BackendKind> {
        self.declared.get(name).map(BackendConfig::kind)
    }

    /// Declared backend names, sorted.
    pub fn declared(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.declared.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Declared backend names, sorted, for error messages.
    pub fn names(&self) -> String {
        self.declared().join(", ")
    }

    pub fn resolve(&self, name: &str) -> Result<Backend, BackendError> {
        let Some(config) = self.declared.get(name) else {
            if self.declared.is_empty() {
                return Err(BackendError::NoneDeclared { name: name.to_string() });
            }
            return Err(BackendError::Unknown {
                name: name.to_string(),
                declared: self.names(),
            });
        };
        build(name, config)
    }

    /// The backend a server gets when it names none.
    ///
    /// This is what every macOS user had before `[secrets]` existed, so an
    /// existing config keeps working untouched and existing keychain items stay
    /// addressable. It is a default, not a fallback: nothing falls back to it
    /// when a named backend fails.
    pub fn default_backend() -> Backend {
        Backend::Keychain(KeychainBackend::with_default_service())
    }

    /// The backend one MCP server addresses.
    pub fn for_server(&self, server: &str, declared: Option<&str>) -> Result<Backend, ServerBackendError> {
        match declared {
            None => Ok(Self::default_backend()),
            Some(name) => self.resolve(name).map_err(|cause| ServerBackendError {
                server: server.to_string(),
                backend: name.to_string(),
                cause: Box::new(cause),
            }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("`[mcp.servers.{server}]` names `secrets = \"{backend}\"`, but {cause}")]
pub struct ServerBackendError {
    pub server: String,
    pub backend: String,
    #[source]
    pub cause: Box<BackendError>,
}

fn build(name: &str, config: &BackendConfig) -> Result<Backend, BackendError> {
    match config {
        BackendConfig::Keychain(KeychainConfig { service }) => Ok(Backend::Keychain(KeychainBackend::new(
            service.clone().unwrap_or_else(|| DEFAULT_SERVICE.to_string()),
        ))),
        BackendConfig::Openbao(openbao) => {
            let OpenbaoConfig {
                addr,
                mount,
                path_prefix,
                owner,
                machine_id,
                token_file,
                token,
                ca_cert_file,
                timeout_ms,
            } = openbao.as_ref();
            for (field, source) in [("addr", Some(addr)), ("token", token.as_ref())] {
                if source.is_some_and(|s| s.reads_secret()) {
                    return Err(BackendError::SecretVar {
                        name: name.to_string(),
                        field,
                    });
                }
            }

            let addr = addr
                .resolve_bootstrap(&FetchedSecrets::new())
                .map_err(|cause| BackendError::Resolve {
                    name: name.to_string(),
                    field: "addr",
                    cause: Box::new(cause),
                })?;

            let token = match (token_file, token) {
                (Some(file), None) => TokenSource::File(expand_tilde(file)),
                (None, Some(var)) => TokenSource::Var(var.clone()),
                (None, None) => {
                    return Err(BackendError::TokenSource {
                        name: name.to_string(),
                        found: "neither",
                    })
                }
                (Some(_), Some(_)) => {
                    return Err(BackendError::TokenSource {
                        name: name.to_string(),
                        found: "both",
                    })
                }
            };

            // Two separate axes, and neither is derived from anything.
            //
            // `owner` says whose credential it is, which is the boundary an
            // ACL draws. `machine_id` says which holder may refresh it, which
            // matters only where a provider rotates refresh tokens and treats
            // a reused one as replay. One person on two machines is one owner
            // and two holders, so neither substitutes for the other.
            let settings = OpenBaoSettings {
                addr,
                mount: mount.clone(),
                path_prefix: path_prefix.clone(),
                owner: owner.clone(),
                machine_id: machine_id.clone(),
                token,
                ca_cert_file: ca_cert_file.as_deref().map(expand_tilde),
                timeout: Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)),
            };

            OpenBaoBackend::new(settings)
                .map(Backend::OpenBao)
                .map_err(|cause| BackendError::Build {
                    name: name.to_string(),
                    cause,
                })
        }
        BackendConfig::OnePassword(OnePasswordConfig { account }) => {
            Ok(Backend::OnePassword(OnePasswordBackend::new(account.clone())))
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
